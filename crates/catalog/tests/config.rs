//! Configuration boundary tests.

use ratatoskr_github_catalog::Config;

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
