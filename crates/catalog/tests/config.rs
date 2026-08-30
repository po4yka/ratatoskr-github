//! Configuration boundary tests.

use ratatoskr_github_catalog::Config;

#[test]
fn serving_bus_configuration_is_complete_finite_and_redacted() {
    let seed_path = "/run/credentials/ratatoskr-github/github.nkey";
    let configured = Config::from_environment([
        ("RATATOSKR__BUS__URL", "nats://127.0.0.1:4222"),
        ("RATATOSKR__BUS__NKEY_SEED_PATH", seed_path),
        ("RATATOSKR__BUS__CONNECT_TIMEOUT_MS", "5000"),
        ("RATATOSKR__BUS__PUBLISH_ACK_TIMEOUT_MS", "5000"),
        ("RATATOSKR__BUS__POLL_INTERVAL_MS", "250"),
        ("RATATOSKR__BUS__LEASE_MS", "30000"),
        ("RATATOSKR__BUS__BATCH_SIZE", "16"),
        ("RATATOSKR__BUS__MAX_ATTEMPTS", "10"),
        ("RATATOSKR__BUS__WORKER_JOIN_TIMEOUT_MS", "120000"),
    ])
    .expect("complete finite bus configuration");
    let serialized = serde_json::to_string(&configured).unwrap_or_default();
    let debug = format!("{configured:?}");
    assert!(!serialized.contains(seed_path));
    assert!(!debug.contains(seed_path));
}

#[test]
fn serving_bus_configuration_refuses_unbounded_limits() {
    for (key, value) in [
        ("RATATOSKR__BUS__CONNECT_TIMEOUT_MS", "30001"),
        ("RATATOSKR__BUS__BATCH_SIZE", "257"),
        ("RATATOSKR__BUS__MAX_ATTEMPTS", "101"),
        ("RATATOSKR__LIMITS__SHUTDOWN_TIMEOUT_MS", "120000"),
    ] {
        assert!(
            Config::from_environment([(key, value)]).is_err(),
            "unbounded setting was accepted: {key}"
        );
    }
}

#[test]
fn defaults_are_finite_and_loopback() {
    let config = Config::default();

    assert!(config.admin.listen_address.ip().is_loopback());
    assert_ne!(config.admin.listen_address.port(), 0);
    assert!(config.limits.database_connections > 0);
    assert!(config.limits.database_acquire_timeout_ms > 0);
    assert!(config.limits.shutdown_timeout_ms > 0);
}

#[test]
fn domain_api_listener_is_loopback_and_provider_test_url_is_bounded() {
    let configured = Config::from_environment([
        ("RATATOSKR__API__LISTEN_ADDRESS", "127.0.0.1:8092"),
        ("RATATOSKR__PROVIDER__BASE_URL", "http://127.0.0.1:18092"),
    ]);
    assert!(
        configured.is_ok(),
        "the domain listener and bounded provider test URL must be recognized"
    );

    let config = configured.unwrap_or_default();
    let encoded = serde_json::to_value(&config).unwrap_or_default();
    let api_address = encoded
        .pointer("/api/listen_address")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let provider_base_url = encoded
        .pointer("/provider/base_url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    assert_eq!(api_address, "127.0.0.1:8092");
    assert_ne!(api_address, config.admin.listen_address.to_string());
    assert_eq!(provider_base_url, "http://127.0.0.1:18092");

    for value in ["0.0.0.0:8092", "127.0.0.1:0"] {
        assert!(
            Config::from_environment([("RATATOSKR__API__LISTEN_ADDRESS", value)]).is_err(),
            "unsafe domain listener was accepted: {value}"
        );
    }
    for value in [
        "http://github.com",
        "http://192.0.2.1:18092",
        "ftp://127.0.0.1:18092",
    ] {
        assert!(
            Config::from_environment([("RATATOSKR__PROVIDER__BASE_URL", value)]).is_err(),
            "unsafe provider base URL was accepted: {value}"
        );
    }
}

#[test]
fn serialization_omits_the_database_url() -> Result<(), serde_json::Error> {
    let config = Config::default();

    let encoded = serde_json::to_string(&config)?;
    assert!(!encoded.contains("postgres://"));
    Ok(())
}

#[test]
fn credential_key_configuration_is_accepted_but_never_serialized_or_debugged()
-> Result<(), Box<dyn std::error::Error>> {
    let configured_key = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    let config = Config::from_environment([
        ("RATATOSKR__CREDENTIALS__ENCRYPTION_KEY_HEX", configured_key),
        ("RATATOSKR__CREDENTIALS__KEY_VERSION", "key-2026-08"),
    ])?;

    let serialized = serde_json::to_string(&config)?;
    let debug = format!("{config:?}");
    assert!(!serialized.contains(configured_key));
    assert!(!debug.contains("255"));
    Ok(())
}

#[test]
fn oauth_app_configuration_is_complete_and_redacted() -> Result<(), Box<dyn std::error::Error>> {
    let client_id = "Iv1.abcdef0123456789";
    let client_secret = "synthetic-oauth-client-secret";
    let configured = Config::from_environment([
        ("RATATOSKR__GITHUB_OAUTH__CLIENT_ID", client_id),
        ("RATATOSKR__GITHUB_OAUTH__CLIENT_SECRET", client_secret),
    ]);

    assert!(configured.is_ok(), "complete OAuth configuration must load");
    let configured = configured?;
    let serialized = serde_json::to_string(&configured)?;
    let debug = format!("{configured:?}");
    assert!(!serialized.contains(client_secret));
    assert!(!debug.contains(client_secret));

    for entries in [
        vec![("RATATOSKR__GITHUB_OAUTH__CLIENT_ID", client_id)],
        vec![("RATATOSKR__GITHUB_OAUTH__CLIENT_SECRET", client_secret)],
    ] {
        let result = Config::from_environment(entries);
        assert!(
            result.is_err(),
            "partial OAuth configuration must be refused"
        );
    }

    Ok(())
}

#[test]
fn legacy_source_configuration_is_accepted_but_never_serialized_or_debugged()
-> Result<(), Box<dyn std::error::Error>> {
    let source_url = "postgres://legacy-reader:synthetic@127.0.0.1:5435/legacy";
    let config =
        Config::from_environment([("RATATOSKR__LEGACY__SOURCE_DATABASE_URL", source_url)])?;

    let serialized = serde_json::to_string(&config)?;
    let debug = format!("{config:?}");
    assert!(!serialized.contains(source_url));
    assert!(!debug.contains(source_url));
    Ok(())
}

#[test]
fn unknown_key_is_refused_without_echoing_value() {
    let result = Config::from_environment([("RATATOSKR__LIMITS__MYSTERY", "LEAKME")]);

    let diagnostic = result.expect_err("unknown key must fail").to_string();
    assert!(diagnostic.contains("RATATOSKR__LIMITS__MYSTERY"));
    assert!(!diagnostic.contains("LEAKME"));
}

#[test]
fn invalid_limit_values_are_refused_without_echoing_value() {
    for value in ["0", "-5", "not-a-number"] {
        let result = Config::from_environment([("RATATOSKR__LIMITS__SHUTDOWN_TIMEOUT_MS", value)]);

        let diagnostic = result.expect_err("invalid limit must fail").to_string();
        assert!(
            diagnostic.contains("RATATOSKR__LIMITS__SHUTDOWN_TIMEOUT_MS"),
            "diagnostic must name the key: {diagnostic}"
        );
        assert!(!diagnostic.contains(value), "diagnostic echoed the value");
    }
}

#[test]
fn malformed_database_url_is_refused_without_echoing_value() {
    let result = Config::from_environment([("RATATOSKR__STORAGE__DATABASE_URL", "LEAKME")]);

    let diagnostic = result.expect_err("malformed url must fail").to_string();
    assert!(diagnostic.contains("RATATOSKR__STORAGE__DATABASE_URL"));
    assert!(!diagnostic.contains("LEAKME"));
}

#[test]
fn recognized_overrides_change_exactly_their_own_field() {
    let config = Config::from_environment([
        ("RATATOSKR__ADMIN__LISTEN_ADDRESS", "127.0.0.1:9100"),
        (
            "RATATOSKR__STORAGE__DATABASE_URL",
            "postgres://github:github@127.0.0.1:5435/github",
        ),
        ("RATATOSKR__BUS__WORKER_JOIN_TIMEOUT_MS", "20000"),
        ("RATATOSKR__LIMITS__SHUTDOWN_TIMEOUT_MS", "25000"),
    ])
    .expect("recognized keys must load");

    assert_eq!(config.admin.listen_address.port(), 9100);
    assert_eq!(
        config.storage.database_url,
        "postgres://github:github@127.0.0.1:5435/github"
    );
    assert_eq!(config.limits.shutdown_timeout_ms, 25_000);
    assert_eq!(config.limits.database_connections, 8);
    assert_eq!(config.limits.database_acquire_timeout_ms, 5_000);

    let non_loopback =
        Config::from_environment([("RATATOSKR__ADMIN__LISTEN_ADDRESS", "0.0.0.0:9100")]);
    assert!(
        non_loopback.is_err(),
        "a non-loopback admin address must be refused"
    );
}
