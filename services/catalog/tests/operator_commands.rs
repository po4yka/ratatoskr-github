//! Operator command boundary tests.

use std::process::Command;

#[test]
fn legacy_commands_reject_secret_bearing_arguments_and_unapproved_activation()
-> Result<(), Box<dyn std::error::Error>> {
    let supplied_token = "synthetic-pat-must-not-be-accepted";
    let output = Command::new(env!("CARGO_BIN_EXE_ratatoskr-github-catalog"))
        .args(["check-config", "--pat", supplied_token])
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;

    assert!(
        !output.status.success(),
        "token-like CLI input was accepted"
    );
    assert!(
        !stderr.contains(supplied_token),
        "diagnostic echoed token input"
    );
    let source_url = "postgres://synthetic-source-must-not-be-accepted";
    let output = Command::new(env!("CARGO_BIN_EXE_ratatoskr-github-catalog"))
        .args(["import-legacy", "--source-url", source_url])
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        !output.status.success(),
        "source URL CLI input was accepted"
    );
    assert!(!stderr.contains(source_url), "diagnostic echoed source URL");
    let unapproved_activation = "synthetic-unapproved-activation";
    let output = Command::new(env!("CARGO_BIN_EXE_ratatoskr-github-catalog"))
        .args([
            "activate-legacy-cutover",
            "--approval",
            unapproved_activation,
        ])
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(!output.status.success(), "cutover activation was accepted");
    assert!(
        !stderr.contains(unapproved_activation),
        "diagnostic echoed unapproved activation material"
    );
    Ok(())
}
