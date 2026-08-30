# Developing Ratatoskr GitHub

> Status: Active  
> Last reviewed: 2026-08-23

The service foundation and repository domain API are implemented. The real process binds loopback operator and Edge-authenticated domain listeners, stores replacement PATs encrypted, serves read-only repository previews, and executes confirmed `metadata`/`track`/`star` actions with durable exact replay and truthful component outcomes. Snapshot, list, mode, mutation, watch, analysis-request, and desired-policy behavior remains as documented in the repository specs. OAuth and live fleet-bus handling are not implemented.

### Local process configuration

The defaults bind operator routes to `127.0.0.1:9095`, domain routes to
`127.0.0.1:8092`, PostgreSQL to `127.0.0.1:5435`, and the provider to
`https://api.github.com`. Deployment may override them with:

```bash
RATATOSKR__ADMIN__LISTEN_ADDRESS=127.0.0.1:9095
RATATOSKR__API__LISTEN_ADDRESS=127.0.0.1:8092
RATATOSKR__INTERNAL_API__LISTEN_ADDRESS=0.0.0.0:8093
RATATOSKR__STORAGE__DATABASE_URL=postgres://github:github@127.0.0.1:5435/github
RATATOSKR__PROVIDER__BASE_URL=https://api.github.com
RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX=<64 lowercase or uppercase hex characters>
RATATOSKR__CREDENTIALS__KEY_VERSION=<non-secret key label>
RATATOSKR__SERVICE_AUTH__KNOWLEDGE_TOKEN_FILE=/etc/ratatoskr/github/knowledge.token
```

The operator and Edge domain listeners must remain distinct loopback sockets. The separately bound
internal listener may use loopback, a private/link-local address, or `0.0.0.0`/`::` for a private
container network; never publish its port through the host or an ingress. It is bound only when the
Knowledge token file is configured, and it serves no Edge routes. Provider HTTP is accepted only
for a numeric loopback origin in synthetic tests. The database URL and encryption key are secrets
and must come from the deployment secret store, not arguments or logs.

The Knowledge README resolver is enabled only when
`RATATOSKR__SERVICE_AUTH__KNOWLEDGE_TOKEN_FILE` names an absolute regular file. Store one opaque
32-256 character token there, share it only with the Knowledge deployable, and keep the file
unreadable by users outside its owner/group boundary (`0640` or stricter). The token value is never
accepted in process arguments or configuration fields. Without this file the internal listener is
not bound, so an accidentally unconfigured Catalog cannot silently expose immutable evidence.

Knowledge calls `POST /internal/v1/repository-readmes/resolve` on the private internal listener with
a Bearer token and only the typed `owner`, `repository_id`, and `content_ref` from the retained
analysis request. Catalog returns at most 1 MiB of `text/markdown` bytes after confirming the exact
owner/repository publication and independently verifying digest, media type, and length. The route
does not exist on the Edge API listener and does not accept a provider URL, filesystem path, or raw
database key.

## Toolchain and gate

`rust-toolchain.toml` pins Rust 1.97. Every command uses the committed lock file.

### Rust - the CI gate

```bash
cargo fetch --locked
cargo deny --locked check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
cargo test --workspace --locked --doc
cargo build --workspace --locked --release
```

The file-size ratchet is the one check that Cargo cannot express:

```bash
git ls-files -z "*.rs" | xargs -0 -r wc -l | awk '$2 != "total" && $1 > 850 { print; bad = 1 } END { exit bad }'
```

Database tests create disposable databases from the current `schema.sql`. Locally they need the compose stack:

```bash
docker compose up -d --wait
```

Tests use `GITHUB_CATALOG_TEST_DATABASE_URL`, which defaults to `postgres://github:github@127.0.0.1:5435/github`; CI provisions its own service container.

## Code size limits

`clippy.toml` enforces the current function, nesting, signature, and type limits. CI also rejects a
tracked Rust source file above 850 lines.

`ratatoskr-workspace/docs/QUALITY_GATES.md` holds the numbers the repositories with code use today, the command that measured each one, and the limits that were rejected with the reason. Read it before you choose numbers, then measure this tree. Each limit is set at the worst case the tree already has, so that the check fails on a regression and not on work that has not been done yet.

## Workflow

1. Identify provider capability, required scope, mutation/consent, rate-limit cost, and authority semantics.
2. Preserve GitHub numeric IDs and treat names as mutable aliases.
3. Never infer removal from an incomplete scan.
4. Add pagination, checkpoint, redelivery, partial-success, and rate-limit tests.
5. Delegate Git execution to Vault and analysis to Knowledge through contracts.

The service-foundation change supplied the exact build, test, current-schema, and local-stack
commands above. This development repository has no database migrations. Default tests use
synthetic fixtures and never personal tokens.

## What a clone needs before you plan a change

A change is planned with OpenSpec, which is a CLI a clone installs for itself. Use the version
`.github/workflows/openspec.yml` pins, so your terminal and the gate answer the same:

```bash
npm install --global @fission-ai/openspec@1.10.0
```

Cross-repository behaviour lives in a store, and registering one is per-machine state that no
repository can turn on for you — the same kind of step as `git config core.hooksPath .githooks`:

```bash
git clone git@github.com:po4yka/ratatoskr-workspace.git <path>
openspec store register <path> --id ratatoskr-workspace
```

`openspec doctor` reports whether both are in place.

## The Rust skills in this repository

`.agents/skills/` holds eighteen Rust skills vendored from `po4yka/rust-skills`, and
`.claude/skills/` symlinks to them. Unlike the steps above this needs nothing from your machine: the
files are in the tree, so a fresh clone already has them.

Update them with the catalogue and never by hand:

```bash
npx skills update
```

That rewrites `.agents/skills/` and `skills-lock.json` from the catalogue. Run it in one repository,
read the diff, then apply the same change to every Ratatoskr repository whose stack is Rust.
`ratatoskr-workspace/.github/workflows/drift.yml` fails when one copy differs from the others.
