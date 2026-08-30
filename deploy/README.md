# GitHub Catalog deployment

Build the pinned arm64 artifact with `cargo build-arm64`. Install the binary under
`/opt/ratatoskr/github/bin`, the environment file as `/etc/ratatoskr/github.env`, and the unit as
`ratatoskr-github.service`.

The service account owns no shell and has an owned PostgreSQL role/database named
`ratatoskr_github`. Operators install the GitHub NKey seed and the 32-byte hexadecimal credential
encryption key as root-owned mode `0640` files at `/etc/ratatoskr/github.nkey` and
`/etc/ratatoskr/github-credential-key`. The checked-in environment contains paths only.

Platform Edge must provision and verify all four fixed GitHub durables before this unit starts.
After start, verify `http://127.0.0.1:9469/live` and `.../ready`; ready is valid only while the
database, bus, exact durables and all seven workers are healthy. Send `SIGTERM` and confirm exit
within 130 seconds before promoting a release. Rollback disables schedules, stops this unit first,
and retains outbox/inbox rows and durable cursors.
