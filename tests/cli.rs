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
fn serve_that_cannot_start_is_not_a_fake_running_agent() {
    // No ready line, no success and no socket unless a session really exists.
    let missing = std::env::temp_dir().join(format!("carlo-cli-missing-{}", std::process::id()));
    let refused = command(&["serve", "--session-dir", missing.to_str().unwrap()]);
    assert_eq!(refused.status.code(), Some(69));
    assert!(refused.stdout.is_empty());
    assert!(
        String::from_utf8(refused.stderr)
            .unwrap()
            .contains("session directory does not exist")
    );
    assert!(!missing.exists());
    let bare = command(&["serve"]);
    assert_eq!(bare.status.code(), Some(64));
    assert!(bare.stdout.is_empty());
    assert!(
        String::from_utf8(bare.stderr)
            .unwrap()
            .contains("--session-dir")
    );
    let doctor = command(&["doctor", "--json"]);
    let report: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let serve = report["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|capability| capability["name"] == "serve_protocol")
        .unwrap();
    assert_eq!(serve["available"], false);
}

#[test]
fn display_mirror_outside_the_pinned_profile_emits_no_stream_and_reports_no_capture() {
    let dump = command(&["display-mirror-dump"]);
    assert_eq!(dump.status.code(), Some(69));
    assert!(dump.stdout.is_empty());
    let probe = command(&["display-mirror-probe"]);
    assert_eq!(probe.status.code(), Some(69));
    let text = String::from_utf8(probe.stdout).unwrap();
    assert!(text.contains("\"source\": \"display_mirror\""));
    assert!(text.contains("\"screen_capture\": false"));
    assert!(text.contains("\"kind\": \"unsupported_profile\""));
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

#[cfg(not(target_os = "android"))]
#[test]
fn explicit_node_read_is_guarded_and_has_no_payload_on_unsupported_host() {
    let output = command(&["node-read-probe"]);
    assert_eq!(output.status.code(), Some(69));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["outcome"]["error"]["kind"], "unsupported_profile");
    assert!(report.get("node").is_none());
    assert_eq!(report["hierarchy_available"], false);
    assert_eq!(
        command(&["node-read-probe", "--all"]).status.code(),
        Some(64)
    );
}
