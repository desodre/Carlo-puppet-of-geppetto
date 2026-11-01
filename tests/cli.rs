//! Process-level checks for output separation and unavailable capabilities.

use std::process::Command;

fn command(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_carlo"))
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn version_has_one_source_and_no_error_output() {
    let output = command(&["--version"]);
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        format!("carlo {}\n", env!("CARGO_PKG_VERSION")).as_bytes()
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn unavailable_serve_is_not_a_fake_running_agent() {
    let output = command(&["serve"]);
    assert_eq!(output.status.code(), Some(69));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("planned in P3")
    );
}

#[test]
fn invalid_cli_has_stable_exit_code_without_echoing_caller_data() {
    let output = command(&["sensitive-value"]);
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty());
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("sensitive-value")
    );
}

#[test]
fn json_report_remains_one_document_when_environment_is_unavailable() {
    let output = command(&["doctor", "--json"]);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["agent_version"], env!("CARGO_PKG_VERSION"));
    assert!(
        report["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .all(|capability| capability["available"] == false)
    );
    if cfg!(not(target_os = "android")) {
        assert_eq!(output.status.code(), Some(69));
        assert_eq!(report["foundation_ready"], false);
        assert!(report["android_api"].is_null());
    }
}
