# Developing Ratatoskr GitHub

> Status: Active  
> Last reviewed: 2026-08-23

The service foundation is implemented: a Rust workspace with typed configuration, structured telemetry, operator health routes, and the first-version `github_catalog` schema. Accounts and credentials, synchronization, mutations, and event handling are not implemented.

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

There is no code here yet, so no limit is enforced yet. The commit that brings the first manifest brings the configuration that carries the limits with it: `clippy.toml` beside a `Cargo.toml`, `eslint.config.js` beside a `package.json`. `fleet.yml` fails the gate when a manifest arrives without one, so the rule has a check behind it and not only this paragraph.

`ratatoskr-workspace/docs/QUALITY_GATES.md` holds the numbers the repositories with code use today, the command that measured each one, and the limits that were rejected with the reason. Read it before you choose numbers, then measure this tree. Each limit is set at the worst case the tree already has, so that the check fails on a regression and not on work that has not been done yet.

## Workflow

1. Identify provider capability, required scope, mutation/consent, rate-limit cost, and authority semantics.
2. Preserve GitHub numeric IDs and treat names as mutable aliases.
3. Never infer removal from an incomplete scan.
4. Add pagination, checkpoint, redelivery, partial-success, and rate-limit tests.
5. Delegate Git execution to Vault and analysis to Knowledge through contracts.

The first scaffold PR must specify exact build/test/migration/local-server commands. Default tests use recorded/synthetic fixtures, never personal tokens.

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
