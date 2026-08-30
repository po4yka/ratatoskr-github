//! Operator command boundary tests.

use std::process::Command;

use ratatoskr_github_catalog_service::{OperatorCommand, parse_operator_command};
use uuid::Uuid;

#[test]
fn dead_letter_requeue_accepts_only_one_exact_message_identity() {
    let message_id = Uuid::now_v7();
    assert_eq!(
        parse_operator_command([
            "catalog".to_owned(),
            "requeue-dead-letter".to_owned(),
            "--message-id".to_owned(),
            message_id.to_string(),
        ])
        .expect("exact command"),
        OperatorCommand::RequeueDeadLetter {
            message_id: message_id.to_string(),
        }
    );
    assert!(
        parse_operator_command([
            "catalog".to_owned(),
            "requeue-dead-letter".to_owned(),
            "--message-id".to_owned(),
            message_id.to_string(),
            "extra".to_owned(),
        ])
        .is_err()
    );
}

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
