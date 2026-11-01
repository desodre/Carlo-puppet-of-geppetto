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

#[cfg(not(target_os = "android"))]
#[test]
fn binder_probe_reports_unsupported_host_without_claiming_hierarchy() {
    let output = command(&["binder-probe"]);
    assert_eq!(output.status.code(), Some(69));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["outcome"]["status"], "unsupported_target");
    assert!(report["outcome"]["symbols"].as_array().unwrap().is_empty());
    assert!(report["outcome"]["lookups"].as_array().unwrap().is_empty());
    assert_eq!(report["hierarchy_available"], false);
}

#[cfg(not(target_os = "android"))]
#[test]
fn accessibility_probe_does_not_attempt_private_calls_on_host() {
    let output = command(&["accessibility-probe"]);
    assert_eq!(output.status.code(), Some(69));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["outcome"]["error"]["kind"], "unsupported_profile");
    assert_eq!(report["hierarchy_available"], false);
    assert_eq!(report["outcome"]["initialized"], false);
}

#[test]
fn accessibility_hold_is_bounded_and_invalid_values_are_not_echoed() {
    for args in [
        vec!["accessibility-probe", "--hold-ms", "10001"],
        vec!["accessibility-probe", "--hold-ms", "-1"],
        vec!["accessibility-probe", "--hold-ms"],
        vec!["accessibility-probe", "--hold-ms", "0", "extra"],
        vec!["accessibility-probe", "sensitive-value"],
    ] {
        let output = command(&args);
        assert_eq!(output.status.code(), Some(64));
        assert!(output.stdout.is_empty());
        assert!(
            !String::from_utf8(output.stderr)
                .unwrap()
                .contains("sensitive-value")
        );
    }
}

#[cfg(not(target_os = "android"))]
#[test]
fn node_query_is_profile_guarded_and_never_advertises_decoded_hierarchy() {
    let output = command(&["node-query-probe"]);
    assert_eq!(output.status.code(), Some(69));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["outcome"]["error"]["kind"], "unsupported_profile");
    assert_eq!(report["hierarchy_available"], false);
    assert!(report["outcome"].get("query").is_none());
    let invalid = command(&["node-query-probe", "sensitive-value"]);
    assert_eq!(invalid.status.code(), Some(64));
    assert!(invalid.stdout.is_empty());
    assert!(
        !String::from_utf8(invalid.stderr)
            .unwrap()
            .contains("sensitive-value")
    );
}
