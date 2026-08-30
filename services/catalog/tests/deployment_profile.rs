//! Deployable boundary for the arm64 GitHub Catalog role.

#[test]
fn github_service_profile_is_bounded_and_protected() -> Result<(), std::io::Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let unit = std::fs::read_to_string(root.join("deploy/systemd/ratatoskr-github.service"))?;
    let environment =
        std::fs::read_to_string(root.join("deploy/systemd/ratatoskr-github.env.example"))?;
    let logrotate = std::fs::read_to_string(root.join("deploy/logrotate/ratatoskr-github"))?;
    let cargo = std::fs::read_to_string(root.join(".cargo/config.toml"))?;

    for required in [
        "Type=exec",
        "User=ratatoskr-github",
        "Group=ratatoskr-github",
        "TimeoutStopSec=130s",
        "NoNewPrivileges=true",
        "ProtectSystem=strict",
        "ProtectHome=true",
        "PrivateTmp=true",
        "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6",
        "LoadCredential=github.nkey:/etc/ratatoskr/github.nkey",
        "LoadCredential=credential-key:/etc/ratatoskr/github-credential-key",
        "After=network-online.target docker.service",
        "Requires=docker.service",
        "StandardOutput=append:/mnt/nvme/ratatoskr/logs/github/catalog.log",
    ] {
        assert!(unit.contains(required), "unit missing {required}");
    }
    for required in [
        "RATATOSKR__API__LISTEN_ADDRESS=127.0.0.1:8092",
        "RATATOSKR__ADMIN__LISTEN_ADDRESS=127.0.0.1:9469",
        "RATATOSKR__BUS__NKEY_SEED_PATH=/run/credentials/ratatoskr-github/github.nkey",
        "postgres://ratatoskr_github@127.0.0.1:5432/ratatoskr_github",
    ] {
        assert!(
            environment.contains(required),
            "environment missing {required}"
        );
    }
    assert!(logrotate.contains("/mnt/nvme/ratatoskr/logs/github/catalog.log"));
    assert!(!unit.contains("postgresql.service"));
    assert!(!unit.contains("nats.service"));
    assert!(cargo.contains("aarch64-unknown-linux-gnu"));
    for forbidden in ["SUAAAAAAAA", "github_pat_", "ghp_", "PASSWORD="] {
        assert!(!unit.contains(forbidden));
        assert!(!environment.contains(forbidden));
    }
    Ok(())
}
