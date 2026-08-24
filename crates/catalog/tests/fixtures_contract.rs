//! Recorded provider payloads are a parsing contract.
//!
//! Fixtures are synthetic redacted bodies shaped like GitHub REST responses;
//! they pin how provider field names and shapes normalize into catalog
//! metadata. They never contain personal data or live tokens.

use ratatoskr_github_catalog::provider::ProviderRepositoryBody;

fn read_fixture(relative: &str) -> Result<String, Box<dyn std::error::Error>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative);
    std::fs::read_to_string(&path)
        .map_err(|error| format!("missing recorded fixture {}: {error}", path.display()).into())
}

#[test]
fn recorded_fixture_parses_to_expected_projection() -> Result<(), Box<dyn std::error::Error>> {
    let raw = read_fixture("repos/widget.json")?;
    let body: ProviderRepositoryBody = serde_json::from_str(&raw)?;

    assert_eq!(body.provider_repository_id, 300_000_100);
    assert_eq!(body.full_name, "acme/widgets");
    assert_eq!(
        body.owner_name().map(|o| o.to_string()).as_deref(),
        Some("acme/widgets")
    );
    assert_eq!(body.description.as_deref(), Some("A synthetic widget"));
    assert_eq!(body.language.as_deref(), Some("Rust"));
    assert_eq!(body.stargazers, 42);
    assert_eq!(body.topics, vec!["widgets".to_owned(), "rust".to_owned()]);
    assert_eq!(body.default_branch.as_deref(), Some("main"));
    assert_eq!(body.pushed_at.as_deref(), Some("2026-08-01T10:00:00Z"));
    Ok(())
}

#[test]
fn minimal_fixture_defaults_topics_to_empty() -> Result<(), Box<dyn std::error::Error>> {
    let raw = read_fixture("repos/minimal.json")?;
    let body: ProviderRepositoryBody = serde_json::from_str(&raw)?;

    assert!(
        body.topics.is_empty(),
        "absent topics must default to empty"
    );
    assert_eq!(body.description, None);
    assert_eq!(body.language, None);
    Ok(())
}
