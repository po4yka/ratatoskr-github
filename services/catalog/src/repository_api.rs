//! Edge-authenticated repository domain routes.

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ratatoskr_error_contracts::{ErrorCode, ErrorEnvelope};
use ratatoskr_github_catalog::provider::{
    FetchOutcome, FreshRepository, GithubApi, ReqwestGithubApi,
};
use ratatoskr_github_catalog::rate_limit::{RateLimitLedger, TokenRef};
use ratatoskr_github_catalog::{
    AliasKind, AppliedOutcome, CredentialKey, Database, MutationContext, MutationError,
    MutationRequest, MutationSource, MutationStatus, RefusalReason, RepositoryRef, TrackOutcome,
    apply_fresh_body, execute_mutation, load_active_pat, record_alias, track_repository,
    upsert_repository,
};
use ratatoskr_github_contracts::{
    GitHubAccountRef, GitHubRepositoryNumericId, GitHubRepositoryUrl, RepositoryActionCapability,
    RepositoryActionFailureReason, RepositoryActionRefusalReason, RepositoryActionRequest,
    RepositoryActionResult, RepositoryActionSkipReason, RepositoryDescription,
    RepositoryDesiredBackupOutcome, RepositoryFullName, RepositoryLanguage,
    RepositoryMetadataOutcome, RepositoryPreviewRequest, RepositoryPreviewResponse,
    RepositoryPreviewTarget, RepositoryProviderStarOutcome,
};
use ratatoskr_identifiers::SafeMessage;
use secrecy::ExposeSecret as _;
use serde::Serialize;
use uuid::Uuid;

use crate::repository_action_attempts::{ActionClaim, claim_action, complete_action};

const USER_HEADER: &str = "x-ratatoskr-user-id";
const MAX_REQUEST_BYTES: usize = 16 * 1024;

/// Collaborators shared by repository domain handlers.
#[derive(Debug, Clone)]
pub struct RepositoryApiState {
    pub(crate) database: Database,
    provider: ReqwestGithubApi,
    ledger: Arc<RateLimitLedger>,
    credential_key: Option<CredentialKey>,
    pub(crate) knowledge_service_token: Option<crate::ServiceBearerToken>,
}

impl RepositoryApiState {
    /// Builds API state without exposing provider credentials.
    #[must_use]
    pub fn new(
        database: Database,
        provider: ReqwestGithubApi,
        credential_key: Option<CredentialKey>,
    ) -> Self {
        Self {
            database,
            provider,
            ledger: Arc::new(RateLimitLedger::new()),
            credential_key,
            knowledge_service_token: None,
        }
    }

    /// Enables the exact service credential accepted by the Knowledge content boundary.
    #[must_use]
    pub fn with_knowledge_service_token(mut self, token: crate::ServiceBearerToken) -> Self {
        self.knowledge_service_token = Some(token);
        self
    }
}

/// Builds the separately bound host-local domain router.
pub fn domain_router(state: RepositoryApiState) -> Router {
    Router::new()
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/gh/repositories/preview", post(preview))
        .route("/v1/gh/repositories/actions", post(action))
        .with_state(state)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .layer(middleware::from_fn(no_store))
}

#[derive(Serialize)]
struct Capabilities {
    repository_preview: bool,
    repository_actions: [RepositoryActionCapability; 3],
}

async fn capabilities(headers: HeaderMap) -> Result<Json<Capabilities>, ApiFault> {
    let _user_id = authenticated_user(&headers)?;
    Ok(Json(Capabilities {
        repository_preview: true,
        repository_actions: [
            RepositoryActionCapability::Metadata,
            RepositoryActionCapability::Track,
            RepositoryActionCapability::Star,
        ],
    }))
}

async fn preview(
    State(state): State<RepositoryApiState>,
    headers: HeaderMap,
    payload: Result<Json<RepositoryPreviewRequest>, JsonRejection>,
) -> Result<Json<RepositoryPreviewResponse>, ApiFault> {
    let user_id = authenticated_user(&headers)?;
    let Json(request) = payload.map_err(|_| ApiFault::InvalidRequest)?;
    let (owner, name) = repository_parts(&request.repository_url)?;
    let account = eligible_account(&state, user_id).await?;
    let credential = match (&account, &state.credential_key) {
        (Some((account_id, _)), Some(key)) => load_active_pat(&state.database, *account_id, key)
            .await
            .ok(),
        _ => None,
    };
    let token_ref = TokenRef::from_label(
        account
            .as_ref()
            .map_or_else(|| format!("anonymous:{user_id}"), |(id, _)| id.to_string()),
    );
    state
        .ledger
        .acquire(&token_ref)
        .map_err(|_| ApiFault::RateLimited)?;
    let reply = state
        .provider
        .fetch_repository(
            credential
                .as_ref()
                .map(secrecy::ExposeSecret::expose_secret),
            owner,
            name,
            None,
        )
        .await
        .map_err(|error| ApiFault::from_provider(&error))?;
    state.ledger.observe(&token_ref, &reply.rate_limit);
    let FetchOutcome::Fresh(fresh) = reply.outcome else {
        return Err(ApiFault::ProviderUnavailable);
    };
    let requested_full_name = format!("{owner}/{name}");
    if !fresh
        .body
        .full_name
        .eq_ignore_ascii_case(&requested_full_name)
    {
        return Err(ApiFault::NotFound);
    }
    let star_account = account
        .filter(|(_, scopes)| star_scope(scopes) && credential.is_some())
        .map(|(id, _)| id);
    map_preview(&fresh.body, star_account).map(Json)
}

async fn action(
    State(state): State<RepositoryApiState>,
    headers: HeaderMap,
    payload: Result<Json<RepositoryActionRequest>, JsonRejection>,
) -> Result<Json<RepositoryActionResult>, ApiFault> {
    let user_id = authenticated_user(&headers)?;
    let Json(request) = payload.map_err(|_| ApiFault::InvalidRequest)?;
    validate_action_target(&request)?;
    let owner_ref = format!("user:{user_id}");
    match claim_action(&state.database, &owner_ref, &request)
        .await
        .map_err(|_| ApiFault::Persistence)?
    {
        ActionClaim::Execute => {}
        ActionClaim::Replay(result) => return Ok(Json(result)),
        ActionClaim::Conflict => return Err(ApiFault::IdempotencyConflict),
    }
    let result = if request.mode == RepositoryActionCapability::Star {
        if let Some(reason) = star_refusal(&state, user_id, &request).await? {
            refused_star_result(reason)
        } else {
            execute_catalog_action(&state, user_id, &request).await?
        }
    } else {
        execute_catalog_action(&state, user_id, &request).await?
    };
    // The in-flight response preserves observed component truth even when the
    // final replay write fails after a provider-confirmed mutation.
    let _recorded = complete_action(&state.database, &owner_ref, &request, &result).await;
    Ok(Json(result))
}

async fn execute_catalog_action(
    state: &RepositoryApiState,
    user_id: Uuid,
    request: &RepositoryActionRequest,
) -> Result<RepositoryActionResult, ApiFault> {
    let fresh = match fetch_action_metadata(state, user_id, request).await? {
        Ok(fresh) => fresh,
        Err(result) => return Ok(result),
    };
    let Ok(provider_repository_id) =
        i64::try_from(request.target.github_repository_numeric_id.get())
    else {
        return Ok(target_changed_result(request.mode));
    };
    let Ok(identity) = upsert_repository(&state.database, provider_repository_id).await else {
        return Ok(metadata_persistence_failure(request.mode));
    };
    if record_alias(
        &state.database,
        identity.repository_id,
        AliasKind::OwnerName,
        &fresh.body.full_name,
    )
    .await
    .is_err()
        || record_alias(
            &state.database,
            identity.repository_id,
            AliasKind::HtmlUrl,
            request.target.canonical_url.as_str(),
        )
        .await
        .is_err()
    {
        return Ok(metadata_persistence_failure(request.mode));
    }
    let metadata = match apply_fresh_body(
        &state.database,
        identity.repository_id,
        &fresh.body,
        fresh.etag.as_deref(),
    )
    .await
    {
        Ok(AppliedOutcome::Unchanged) => RepositoryMetadataOutcome::AlreadyApplied,
        Ok(AppliedOutcome::Created | AppliedOutcome::Updated) => {
            RepositoryMetadataOutcome::Succeeded
        }
        Err(_) => return Ok(metadata_persistence_failure(request.mode)),
    };
    if request.mode == RepositoryActionCapability::Star {
        return execute_star_action(state, user_id, request, provider_repository_id, metadata)
            .await;
    }
    let desired_backup = match request.mode {
        RepositoryActionCapability::Track => {
            match track_repository(&state.database, provider_repository_id).await {
                Ok(TrackOutcome::Accepted) => RepositoryDesiredBackupOutcome::Accepted,
                Ok(TrackOutcome::AlreadyApplied) => RepositoryDesiredBackupOutcome::AlreadyApplied,
                Err(_) => RepositoryDesiredBackupOutcome::Failed {
                    reason: RepositoryActionFailureReason::PolicyPublicationFailed,
                },
            }
        }
        RepositoryActionCapability::Metadata => RepositoryDesiredBackupOutcome::Skipped {
            reason: RepositoryActionSkipReason::NotApplicable,
        },
        RepositoryActionCapability::Star => unreachable!("star handled above"),
        _ => return Err(ApiFault::InvalidRequest),
    };
    Ok(RepositoryActionResult::new(
        metadata,
        RepositoryProviderStarOutcome::Skipped {
            reason: RepositoryActionSkipReason::NotApplicable,
        },
        desired_backup,
    ))
}

async fn execute_star_action(
    state: &RepositoryApiState,
    user_id: Uuid,
    request: &RepositoryActionRequest,
    provider_repository_id: i64,
    metadata: RepositoryMetadataOutcome,
) -> Result<RepositoryActionResult, ApiFault> {
    let account_id = star_account_id(request)?;
    let Some(key) = state.credential_key.as_ref() else {
        return Ok(star_component_failed(
            metadata,
            RepositoryActionFailureReason::DependencyUnavailable,
        ));
    };
    let Ok(credential) = load_active_pat(&state.database, account_id, key).await else {
        return Ok(star_component_failed(
            metadata,
            RepositoryActionFailureReason::DependencyUnavailable,
        ));
    };
    let (owner, name) = request
        .target
        .repository_full_name
        .as_str()
        .split_once('/')
        .ok_or(ApiFault::InvalidRequest)?;
    let token = TokenRef::from_label(account_id.to_string());
    let context = MutationContext {
        account_id,
        principal: format!("user:{user_id}"),
        source: MutationSource::Telegram,
    };
    let outcome = execute_mutation(
        &state.database,
        &state.provider,
        &state.ledger,
        &token,
        Some(credential.expose_secret()),
        &context,
        MutationRequest::Star {
            repository: RepositoryRef {
                provider_repository_id,
                owner: owner.to_owned(),
                name: name.to_owned(),
            },
            idempotency_key: request.idempotency_key.as_str().to_owned(),
        },
    )
    .await;
    let (provider_star, desired_backup) = match outcome {
        Ok(outcome) => match outcome.status {
            MutationStatus::Applied => (
                RepositoryProviderStarOutcome::Succeeded,
                RepositoryDesiredBackupOutcome::Accepted,
            ),
            MutationStatus::AlreadyApplied => (
                RepositoryProviderStarOutcome::AlreadyApplied,
                RepositoryDesiredBackupOutcome::AlreadyApplied,
            ),
            MutationStatus::Rejected { reason } => (
                RepositoryProviderStarOutcome::Refused {
                    reason: map_mutation_refusal(reason),
                },
                RepositoryDesiredBackupOutcome::Skipped {
                    reason: RepositoryActionSkipReason::PrerequisiteFailed,
                },
            ),
            MutationStatus::Failed { .. } => (
                RepositoryProviderStarOutcome::Failed {
                    reason: RepositoryActionFailureReason::ProviderUnavailable,
                },
                RepositoryDesiredBackupOutcome::Skipped {
                    reason: RepositoryActionSkipReason::PrerequisiteFailed,
                },
            ),
        },
        Err(MutationError::ProviderConfirmedPersistence { .. }) => (
            RepositoryProviderStarOutcome::Succeeded,
            RepositoryDesiredBackupOutcome::Failed {
                reason: RepositoryActionFailureReason::PolicyPublicationFailed,
            },
        ),
        Err(_) => {
            return Ok(star_component_failed(
                metadata,
                RepositoryActionFailureReason::DependencyUnavailable,
            ));
        }
    };
    Ok(RepositoryActionResult::new(
        metadata,
        provider_star,
        desired_backup,
    ))
}

fn map_mutation_refusal(reason: RefusalReason) -> RepositoryActionRefusalReason {
    match reason {
        RefusalReason::MissingScope => RepositoryActionRefusalReason::ScopeMissing,
        RefusalReason::AccountNotConnected => RepositoryActionRefusalReason::AccountRequired,
        RefusalReason::AutoNotDirectlyRequestable
        | RefusalReason::RepositoryCurrentlyStarred
        | RefusalReason::RepositoryIgnored => RepositoryActionRefusalReason::NotAuthorized,
    }
}

fn star_component_failed(
    metadata: RepositoryMetadataOutcome,
    reason: RepositoryActionFailureReason,
) -> RepositoryActionResult {
    RepositoryActionResult::new(
        metadata,
        RepositoryProviderStarOutcome::Failed { reason },
        RepositoryDesiredBackupOutcome::Skipped {
            reason: RepositoryActionSkipReason::PrerequisiteFailed,
        },
    )
}

async fn fetch_action_metadata(
    state: &RepositoryApiState,
    user_id: Uuid,
    request: &RepositoryActionRequest,
) -> Result<Result<FreshRepository, RepositoryActionResult>, ApiFault> {
    let account = eligible_account(state, user_id).await?;
    let credential = match (&account, &state.credential_key) {
        (Some((account_id, _)), Some(key)) => load_active_pat(&state.database, *account_id, key)
            .await
            .ok(),
        _ => None,
    };
    let token_ref = TokenRef::from_label(
        account
            .as_ref()
            .map_or_else(|| format!("anonymous:{user_id}"), |(id, _)| id.to_string()),
    );
    if state.ledger.acquire(&token_ref).is_err() {
        return Ok(Err(metadata_provider_failure(request.mode)));
    }
    let (owner, name) = repository_parts(&request.target.canonical_url)?;
    let Ok(reply) = state
        .provider
        .fetch_repository(
            credential
                .as_ref()
                .map(secrecy::ExposeSecret::expose_secret),
            owner,
            name,
            None,
        )
        .await
    else {
        return Ok(Err(metadata_provider_failure(request.mode)));
    };
    state.ledger.observe(&token_ref, &reply.rate_limit);
    let FetchOutcome::Fresh(fresh) = reply.outcome else {
        return Ok(Err(metadata_provider_failure(request.mode)));
    };
    let numeric_matches = i64::try_from(request.target.github_repository_numeric_id.get())
        .is_ok_and(|expected| expected == fresh.body.provider_repository_id);
    let name_matches = fresh
        .body
        .full_name
        .eq_ignore_ascii_case(request.target.repository_full_name.as_str());
    if numeric_matches && name_matches {
        Ok(Ok(fresh))
    } else {
        Ok(Err(target_changed_result(request.mode)))
    }
}

fn metadata_provider_failure(mode: RepositoryActionCapability) -> RepositoryActionResult {
    action_stopped_at_metadata(
        mode,
        RepositoryMetadataOutcome::Failed {
            reason: RepositoryActionFailureReason::ProviderUnavailable,
        },
    )
}

fn metadata_persistence_failure(mode: RepositoryActionCapability) -> RepositoryActionResult {
    action_stopped_at_metadata(
        mode,
        RepositoryMetadataOutcome::Failed {
            reason: RepositoryActionFailureReason::CatalogPersistenceFailed,
        },
    )
}

fn target_changed_result(mode: RepositoryActionCapability) -> RepositoryActionResult {
    action_stopped_at_metadata(
        mode,
        RepositoryMetadataOutcome::Refused {
            reason: RepositoryActionRefusalReason::TargetChanged,
        },
    )
}

fn action_stopped_at_metadata(
    mode: RepositoryActionCapability,
    metadata: RepositoryMetadataOutcome,
) -> RepositoryActionResult {
    RepositoryActionResult::new(
        metadata,
        RepositoryProviderStarOutcome::Skipped {
            reason: if mode == RepositoryActionCapability::Star {
                RepositoryActionSkipReason::PrerequisiteFailed
            } else {
                RepositoryActionSkipReason::NotApplicable
            },
        },
        RepositoryDesiredBackupOutcome::Skipped {
            reason: RepositoryActionSkipReason::PrerequisiteFailed,
        },
    )
}

fn validate_action_target(request: &RepositoryActionRequest) -> Result<(), ApiFault> {
    let (owner, name) = repository_parts(&request.target.canonical_url)?;
    let expected = format!("{owner}/{name}");
    if request
        .target
        .repository_full_name
        .as_str()
        .eq_ignore_ascii_case(&expected)
    {
        Ok(())
    } else {
        Err(ApiFault::InvalidRequest)
    }
}

async fn star_refusal(
    state: &RepositoryApiState,
    user_id: Uuid,
    request: &RepositoryActionRequest,
) -> Result<Option<RepositoryActionRefusalReason>, ApiFault> {
    let account_id = star_account_id(request)?;
    let account = sqlx::query_as::<_, (String, Vec<String>)>(
        "select status, granted_scopes
         from github_catalog.github_accounts
         where account_id = $1 and owner_ref = $2",
    )
    .bind(account_id)
    .bind(format!("user:{user_id}"))
    .fetch_optional(state.database.pool())
    .await
    .map_err(|_| ApiFault::Persistence)?;
    let Some((status, scopes)) = account else {
        return Ok(Some(RepositoryActionRefusalReason::NotAuthorized));
    };
    if status != "connected" {
        return Ok(Some(RepositoryActionRefusalReason::AccountRequired));
    }
    if !star_scope(&scopes) {
        return Ok(Some(RepositoryActionRefusalReason::ScopeMissing));
    }
    if state.credential_key.is_none() {
        return Ok(Some(RepositoryActionRefusalReason::AccountRequired));
    }
    Ok(None)
}

fn star_account_id(request: &RepositoryActionRequest) -> Result<Uuid, ApiFault> {
    request
        .account_ref
        .as_ref()
        .and_then(|account| account.as_str().strip_prefix("github-account:"))
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or(ApiFault::InvalidRequest)
}

fn refused_star_result(reason: RepositoryActionRefusalReason) -> RepositoryActionResult {
    RepositoryActionResult::new(
        RepositoryMetadataOutcome::Skipped {
            reason: RepositoryActionSkipReason::PrerequisiteFailed,
        },
        RepositoryProviderStarOutcome::Refused { reason },
        RepositoryDesiredBackupOutcome::Skipped {
            reason: RepositoryActionSkipReason::PrerequisiteFailed,
        },
    )
}

async fn eligible_account(
    state: &RepositoryApiState,
    user_id: Uuid,
) -> Result<Option<(Uuid, Vec<String>)>, ApiFault> {
    let owner_ref = format!("user:{user_id}");
    let accounts = sqlx::query_as::<_, (Uuid, Vec<String>)>(
        "select account_id, granted_scopes
         from github_catalog.github_accounts
         where owner_ref = $1 and status = 'connected'
         order by account_id",
    )
    .bind(owner_ref)
    .fetch_all(state.database.pool())
    .await
    .map_err(|_| ApiFault::Persistence)?;
    Ok(match accounts.as_slice() {
        [account] => Some(account.clone()),
        _ => None,
    })
}

fn map_preview(
    body: &ratatoskr_github_catalog::provider::ProviderRepositoryBody,
    account_id: Option<Uuid>,
) -> Result<RepositoryPreviewResponse, ApiFault> {
    let numeric_id = u64::try_from(body.provider_repository_id)
        .ok()
        .and_then(|value| GitHubRepositoryNumericId::new(value).ok())
        .ok_or(ApiFault::ProviderUnavailable)?;
    let full_name =
        RepositoryFullName::parse(&body.full_name).map_err(|_| ApiFault::ProviderUnavailable)?;
    let canonical_url =
        GitHubRepositoryUrl::parse(&format!("https://github.com/{}", body.full_name))
            .map_err(|_| ApiFault::ProviderUnavailable)?;
    let description = body
        .description
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(RepositoryDescription::parse)
        .transpose()
        .map_err(|_| ApiFault::ProviderUnavailable)?;
    let primary_language = body
        .language
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(RepositoryLanguage::parse)
        .transpose()
        .map_err(|_| ApiFault::ProviderUnavailable)?;
    let stargazer_count =
        u64::try_from(body.stargazers).map_err(|_| ApiFault::ProviderUnavailable)?;
    let account_ref = account_id
        .map(|id| GitHubAccountRef::parse(&format!("github-account:{id}")))
        .transpose()
        .map_err(|_| ApiFault::ProviderUnavailable)?;
    let mut available_actions = vec![
        RepositoryActionCapability::Metadata,
        RepositoryActionCapability::Track,
    ];
    if account_ref.is_some() {
        available_actions.push(RepositoryActionCapability::Star);
    }
    Ok(RepositoryPreviewResponse {
        target: RepositoryPreviewTarget {
            github_repository_numeric_id: numeric_id,
            repository_full_name: full_name,
            canonical_url,
        },
        description,
        stargazer_count,
        primary_language,
        account_ref,
        available_actions,
    })
}

fn authenticated_user(headers: &HeaderMap) -> Result<Uuid, ApiFault> {
    headers
        .get(USER_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or(ApiFault::Unauthenticated)
}

fn repository_parts(url: &GitHubRepositoryUrl) -> Result<(&str, &str), ApiFault> {
    url.as_str()
        .strip_prefix("https://github.com/")
        .and_then(|value| value.split_once('/'))
        .ok_or(ApiFault::InvalidRequest)
}

fn star_scope(scopes: &[String]) -> bool {
    scopes
        .iter()
        .any(|scope| scope == "repo" || scope == "public_repo")
}

async fn no_store(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

#[derive(Debug, Clone, Copy)]
enum ApiFault {
    Unauthenticated,
    InvalidRequest,
    NotFound,
    RateLimited,
    ProviderUnavailable,
    Persistence,
    IdempotencyConflict,
}

impl ApiFault {
    fn from_provider(error: &ratatoskr_github_catalog::provider::ProviderError) -> Self {
        match error {
            ratatoskr_github_catalog::provider::ProviderError::NotFound
            | ratatoskr_github_catalog::provider::ProviderError::Unauthorized => Self::NotFound,
            _ => Self::ProviderUnavailable,
        }
    }

    const fn status(self) -> StatusCode {
        match self {
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::InvalidRequest => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::ProviderUnavailable | Self::Persistence => StatusCode::SERVICE_UNAVAILABLE,
            Self::IdempotencyConflict => StatusCode::CONFLICT,
        }
    }

    const fn fields(self) -> (&'static str, &'static str, bool) {
        match self {
            Self::Unauthenticated => (
                "github.request.unauthenticated",
                "Authentication is required.",
                false,
            ),
            Self::InvalidRequest => (
                "github.request.invalid",
                "The repository request is invalid.",
                false,
            ),
            Self::NotFound => (
                "github.repository.not_found",
                "The repository is unavailable.",
                false,
            ),
            Self::RateLimited => (
                "github.provider.rate_limited",
                "GitHub is temporarily rate limited.",
                true,
            ),
            Self::ProviderUnavailable => (
                "github.provider.unavailable",
                "GitHub is temporarily unavailable.",
                true,
            ),
            Self::Persistence => (
                "github.catalog.unavailable",
                "The GitHub catalog is temporarily unavailable.",
                true,
            ),
            Self::IdempotencyConflict => (
                "github.action.idempotency_conflict",
                "The repository action key conflicts with another request.",
                false,
            ),
        }
    }
}

impl IntoResponse for ApiFault {
    fn into_response(self) -> Response {
        let (code, message, retryable) = self.fields();
        match (ErrorCode::parse(code), SafeMessage::parse(message)) {
            (Ok(code), Ok(message)) => (
                self.status(),
                Json(ErrorEnvelope::new(code, message, retryable)),
            )
                .into_response(),
            _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
}
