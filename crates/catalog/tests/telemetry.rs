//! Telemetry bootstrap behavior.

use ratatoskr_github_catalog::init_telemetry;

#[test]
fn initialization_succeeds_once_and_refuses_a_second_install() {
    init_telemetry().expect("first initialization must succeed");

    let second = init_telemetry();
    let diagnostic = second
        .expect_err("second initialization must fail")
        .to_string();
    assert!(diagnostic.contains("telemetry"));
}
