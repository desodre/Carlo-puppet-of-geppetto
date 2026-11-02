#!/usr/bin/env python3
"""Verify decoded root and changed text on our fixture; persist only redacted evidence."""
from __future__ import annotations
import argparse
import hashlib
import json
import time
from pathlib import Path
from android_session import Adb, ROOT, SessionError, run_session
from smoke_accessibility import active, wait_for

PACKAGE = "org.geppetto.carlo.fixture"


def validate(session, expected):
    if session["exit_code"] != 0 or not session["cleanup_removed"]:
        raise SessionError("node read failed or left its session directory")
    report = json.loads(session["stdout"])
    outcome = report["outcome"]
    node = report.get("node")
    if not (outcome["initialized"] and outcome["unregistered"]
            and outcome["error"] is None and outcome["cleanup_error"] is None
            and outcome["query"]["fields_decoded"] and isinstance(node, dict)
            and report["hierarchy_available"] is False):
        raise SessionError("decoded query or lifecycle contract failed")
    if not (node["package"] == PACKAGE and node["text"] == expected
            and node["content_description"] == "Carlo ação 🪵 <&>"
            and node["resource_id"] == PACKAGE + ":id/fixture_root"
            and node["unique_id"] == "carlo-fixture-root"
            and node["window_id"] >= 0 and node["source_id"] != 0x7fffffff7fffffff
            and node["class"] and node["properties"]["enabled"]
            and node["properties"]["clickable"] and not node["properties"]["password"]):
        raise SessionError("fixture fields did not match; UI contents redacted")
    bounds = node["bounds_in_screen"]
    if not (bounds["right"] > bounds["left"] and bounds["bottom"] > bounds["top"]):
        raise SessionError("fixture bounds are empty or inverted")
    return bounds


def observe(adb, manifest, expected):
    deadline = time.monotonic() + 15
    rejected = 0
    while time.monotonic() < deadline:
        report = run_session(adb, manifest, "node-read-probe", min(10, deadline - time.monotonic()), False)
        if not report["cleanup_removed"]:
            raise SessionError("query directory not removed")
        if report["exit_code"] == 69:
            outcome = json.loads(report["stdout"])["outcome"]
            if outcome["error"] == {"kind": "query_rejected"}:
                rejected += 1
                time.sleep(min(0.02, max(0, deadline - time.monotonic())))
                continue
        return validate(report, expected), rejected
    raise SessionError("fixture root did not become available within deadline")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--manifest", type=Path, default=ROOT / "dist/manifest.json")
    parser.add_argument("--fixture", type=Path, default=ROOT / "dist/node-fixture/fixture.apk")
    parser.add_argument("--output", type=Path, default=ROOT / "results/node-decoder/fixture.json")
    args = parser.parse_args()
    adb = Adb(args.serial)
    if adb.shell(["getprop", "ro.kernel.qemu"]).stdout.strip() != "1":
        parser.error("decoder fixture smoke requires a disposable emulator")
    if active(adb): parser.error("existing automation registration; refusing to disturb it")
    if adb.shell(["pm", "path", PACKAGE], check=False).stdout.strip():
        parser.error("fixture package already exists; refusing to replace it")
    fixture_manifest = json.loads(args.fixture.with_name("manifest.json").read_text())
    digest = hashlib.sha256(args.fixture.read_bytes()).hexdigest()
    if fixture_manifest["package"] != PACKAGE or fixture_manifest["sha256"] != digest:
        parser.error("fixture manifest/checksum mismatch")
    summary = {"serial": args.serial, "fixture_sha256": digest, "agent_manifest": json.loads(args.manifest.read_text()), "ui_contents_recorded": False}
    try:
        adb.run(["install", "--no-streaming", str(args.fixture.resolve())], timeout=60)
        adb.shell(["am", "start", "-W", "-n", PACKAGE + "/.FixtureActivity"], timeout=30)
        bounds, before_rejections = observe(adb, args.manifest.resolve(), "CARLO_STATE_0")
        # Touch stays with ADB; Carlo only reads the root supplied by the app.
        adb.shell(["input", "tap", str((bounds["left"] + bounds["right"]) // 2), str((bounds["top"] + bounds["bottom"]) // 2)])
        _, after_rejections = observe(adb, args.manifest.resolve(), "CARLO_STATE_1")
        summary.update(decoded_root=True, text_changed_after_adb_tap=True, unicode_preserved=True,
                       identifiers_match=True, bounds_valid=True, boolean_properties_match=True,
                       initial_query_rejections=before_rejections, final_query_rejections=after_rejections,
                       separate_query_sessions=True)
    finally:
        adb.run(["uninstall", PACKAGE], check=False)
        removed = not adb.shell(["pm", "path", PACKAGE], check=False).stdout.strip()
        if not removed: raise SessionError("owned fixture package was not removed")
        wait_for(lambda: not active(adb))
        summary.update(fixture_removed=True, automation_active_after_checks=False)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(summary, indent=2) + "\n")
    print("Decoded fixture, Unicode and text change verified; fixture and agent cleaned up.")

if __name__ == "__main__": main()
