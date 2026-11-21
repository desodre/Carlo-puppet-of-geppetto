#!/usr/bin/env python3
"""Prove the hierarchy dump on our fixture: contract, uiautomator parity, update after touch.

Both dumps are UI contents of the screen. They live only in memory and in a
private 0700 temporary directory, are never printed or logged and are always
deleted; the summary holds counters and booleans only, and never the serial.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import tempfile
import time
import uuid
from pathlib import Path

import hierarchy_parity
from android_session import ROOT, Adb, SessionError, run_session

PACKAGE = "org.geppetto.carlo.fixture"
ROOT_ID = PACKAGE + ":id/fixture_root"
STATE = re.compile(r"^CARLO_STATE_(\d+)$")
SESSION_DIRECTORY = re.compile(r"/data/local/tmp/carlo-[0-9a-f]{32}")
REFERENCE_FILE = re.compile(r"/data/local/tmp/carlo-ref-[0-9a-f]{32}\.xml")
SAFE_KEY = re.compile(r"^[a-z0-9_]{1,64}$")
ERROR_CLASS = re.compile(r"^[a-z0-9_]{1,64}$")

SETTLE = 1.0            # seconds between two UiAutomation owners (Carlo, uiautomator)
POLL = 0.2
RESTORE_POLLS = 25      # 5 s for accessibility_enabled to return to its baseline
IDLE_POLLS = 25         # 5 s for a previous automation registration to disappear
RESUME_POLLS = 75       # 15 s for the fixture activity to resume
DUMP_ATTEMPTS = 3
REFERENCE_ATTEMPTS = 2  # one retry on the idle-state/null-root style failure
AGENT_TIMEOUT = 40
REFERENCE_TIMEOUT = 30


def parse_setting(text: str):
    """Value printed by `settings get`, or None when unset."""
    value = (text or "").strip()
    return None if value in ("", "null") else value


def accessibility_state(adb) -> dict:
    """Only the enabled flag and whether a service list exists; never its contents."""
    enabled = parse_setting(
        adb.shell(["settings", "get", "secure", "accessibility_enabled"]).stdout)
    services = parse_setting(
        adb.shell(["settings", "get", "secure", "enabled_accessibility_services"]).stdout)
    return {"enabled": enabled, "services_present": services is not None}


def refuse_if_accessibility_in_use(state: dict) -> None:
    if state["services_present"] or state["enabled"] == "1":
        raise SessionError(
            "an accessibility service is enabled on this device; refusing to run so the "
            "smoke cannot interfere with it (services are not listed)")


def automation_active(adb) -> bool:
    # Read only the automation-presence marker; never log or persist the dump.
    return "Ui Automation[" in adb.shell(["dumpsys", "accessibility"]).stdout


def wait_automation_idle(adb) -> None:
    """Carlo and uiautomator each register a UiAutomation: run them strictly in turn."""
    for _ in range(IDLE_POLLS):
        if not automation_active(adb):
            time.sleep(SETTLE)
            return
        time.sleep(POLL)
    raise SessionError("a UiAutomation registration did not go away within its bound")


def restored(adb, baseline: dict) -> bool:
    """True once accessibility_enabled equals its recorded value again (bounded poll)."""
    for _ in range(RESTORE_POLLS):
        value = parse_setting(
            adb.shell(["settings", "get", "secure", "accessibility_enabled"]).stdout)
        if value == baseline["enabled"]:
            return True
        time.sleep(POLL)
    return False


def whitelist(value, key: str = "", parent: str = ""):
    """Copy numbers, booleans and nulls; keep a string only as the error class."""
    if value is None or isinstance(value, (bool, int, float)):
        return value
    if isinstance(value, str):
        is_class = key == "error" or (parent == "error" and key == "kind")
        return value if is_class and ERROR_CLASS.match(value) else _DROP
    if isinstance(value, dict):
        kept = {}
        for name, item in value.items():
            if isinstance(name, str) and SAFE_KEY.match(name):
                copied = whitelist(item, name, key)
                if copied is not _DROP:
                    kept[name] = copied
        return kept
    if isinstance(value, list):
        return [copied for copied in (whitelist(item, key, parent) for item in value)
                if copied is not _DROP and not isinstance(copied, str)]
    return _DROP


_DROP = object()
_MISSING = object()


def lookup(report, key: str):
    """First value of `key`, searched breadth-first from the top of the report."""
    queue = [report]
    while queue:
        current = queue.pop(0)
        if isinstance(current, dict):
            if key in current:
                return current[key]
            queue.extend(current.values())
        elif isinstance(current, list):
            queue.extend(current)
    return _MISSING


def validate_probe(session: dict) -> dict:
    """Counters of a successful hierarchy-probe; strings other than the error class drop."""
    if session["exit_code"] != 0 or not session["cleanup_removed"]:
        raise SessionError("hierarchy probe failed or left its session directory")
    try:
        report = json.loads(session["stdout"])
    except ValueError:
        raise SessionError("hierarchy probe did not print one JSON report") from None
    if not isinstance(report, dict):
        raise SessionError("hierarchy probe did not print one JSON report")
    nodes, windows = lookup(report, "nodes"), lookup(report, "windows")
    if lookup(report, "error") is not None:
        raise SessionError("hierarchy probe reported an error or no error field")
    for count in (nodes, windows):
        if isinstance(count, bool) or not isinstance(count, int) or count < 1:
            raise SessionError("hierarchy probe did not count at least one window and node")
    return whitelist(report)


def fixture_state(carlo_root) -> dict:
    """Counter and bounds of the fixture root; values are compared here, never stored."""
    for window in carlo_root:
        for node in window.iter("node"):
            fields = node.attrib
            match = STATE.match(fields.get("text", ""))
            if (fields.get("resource-id") == ROOT_ID and fields.get("package") == PACKAGE
                    and fields.get("clickable") == "true" and fields.get("enabled") == "true"
                    and match is not None and hierarchy_parity.valid_bounds(fields.get("bounds"))):
                return {"counter": int(match.group(1)), "bounds": fields["bounds"]}
    raise SessionError("fixture root not found in any window tree with its package, "
                       "resource id, state text and clickable/enabled flags; contents redacted")


def tap_centre(bounds: str) -> tuple:
    match = hierarchy_parity.BOUNDS.match(bounds)
    if match is None:
        raise SessionError("fixture bounds are malformed")
    left, top, right, bottom = map(int, match.groups())
    if right <= left or bottom <= top:
        raise SessionError("fixture bounds are empty or inverted")
    return (left + right) // 2, (top + bottom) // 2


def wait_resumed(adb) -> None:
    for _ in range(RESUME_POLLS):
        dump = adb.shell(["dumpsys", "activity", "activities"], check=False).stdout
        if any("ResumedActivity" in line and PACKAGE + "/" in line for line in dump.splitlines()):
            return
        time.sleep(POLL)
    raise SessionError("fixture activity did not resume within its bound")


def carlo_dump(adb, manifest: Path, physical: bool, scratch: Path, baseline: dict,
               flags: list, session=run_session) -> dict:
    """One validated Carlo dump: its parsed tree stays in memory, its file is removed."""
    path = scratch / ("carlo-" + uuid.uuid4().hex + ".xml")
    try:
        for attempt in range(DUMP_ATTEMPTS):
            wait_automation_idle(adb)
            try:
                report = session(adb, manifest, "hierarchy-dump", AGENT_TIMEOUT, physical, path)
            finally:
                flags.append(restored(adb, baseline))
            if not flags[-1]:
                raise SessionError("accessibility_enabled was not restored after the agent ran")
            if not report["cleanup_removed"]:
                raise SessionError("hierarchy dump left its session directory")
            if report["exit_code"] == 69 and attempt + 1 < DUMP_ATTEMPTS:
                continue  # transient refusal (e.g. window not ready); bounded retry
            if report["exit_code"] != 0 or not report.get("stdout_bytes"):
                raise SessionError("hierarchy dump failed or produced no document")
            data = path.read_bytes()
            break
        try:
            root, text = hierarchy_parity.parse(data, "carlo")
        except hierarchy_parity.ParityError as error:
            raise SessionError("Carlo dump refused: " + error.code) from None
        errors = hierarchy_parity.validate_carlo(root, text)
        if errors:
            raise SessionError("Carlo dump violates its contract: " + ", ".join(errors))
        return {"data": data, "root": root, "attempts": attempt + 1,
                "windows": len(root), "nodes": sum(1 for _ in root.iter("node"))}
    finally:
        path.unlink(missing_ok=True)


def reference_dump(adb, baseline: dict, flags: list) -> tuple:
    """`uiautomator dump` of the active window, read into memory: (bytes, attempts)."""
    for attempt in range(REFERENCE_ATTEMPTS):
        device_file = "/data/local/tmp/carlo-ref-" + uuid.uuid4().hex + ".xml"
        wait_automation_idle(adb)
        data = b""
        try:
            # The status line and any diagnostics are inspected here and never echoed.
            result = adb.shell(["uiautomator", "dump", device_file],
                               timeout=REFERENCE_TIMEOUT, check=False)
            failed = result.returncode != 0 or "ERROR" in (result.stdout or "") + (
                result.stderr or "")
            if not failed:
                data = adb.run(["exec-out", "cat", device_file], timeout=30, check=False,
                               text=False).stdout or b""
        except subprocess.TimeoutExpired:
            failed = True
        finally:
            remove_reference(adb, device_file)
            flags.append(restored(adb, baseline))
        if not flags[-1]:
            raise SessionError("accessibility_enabled was not restored after uiautomator ran")
        if not failed and data:
            try:
                hierarchy_parity.reference_root(hierarchy_parity.parse(data, "reference")[0])
                return data, attempt + 1
            except hierarchy_parity.ParityError as error:
                if error.code != "reference_empty" or attempt + 1 == REFERENCE_ATTEMPTS:
                    raise SessionError("uiautomator reference refused: " + error.code) from None
    raise SessionError("uiautomator did not produce a reference dump (idle state or null root)")


def remove_reference(adb, device_file: str) -> None:
    if REFERENCE_FILE.fullmatch(device_file) is None:
        raise SessionError("refusing to remove a file this smoke does not own")
    adb.shell(["rm", "-f", device_file], check=False)
    if adb.shell(["sh", "-c", 'test ! -e "$1"', "carlo-ref", device_file],
                 check=False).returncode != 0:
        raise SessionError("device-side reference dump was not removed")


def session_directories(adb) -> set:
    listing = adb.shell(["ls", "/data/local/tmp"], check=False).stdout
    return {"/data/local/tmp/" + name.strip() for name in listing.splitlines()
            if SESSION_DIRECTORY.fullmatch("/data/local/tmp/" + name.strip())}


def verify(adb, manifest: Path, physical: bool, scratch: Path, session=run_session) -> dict:
    """Steps on an installed, resumed fixture; documents never leave this call."""
    baseline = accessibility_state(adb)
    refuse_if_accessibility_in_use(baseline)
    if automation_active(adb):
        raise SessionError("existing automation registration; refusing to disturb it")
    flags: list = []

    try:
        probe_session = session(adb, manifest, "hierarchy-probe", AGENT_TIMEOUT, physical)
    finally:
        flags.append(restored(adb, baseline))
    if not flags[-1]:
        raise SessionError("accessibility_enabled was not restored after the probe ran")
    probe = validate_probe(probe_session)

    before = carlo_dump(adb, manifest, physical, scratch, baseline, flags, session)
    first = fixture_state(before["root"])

    reference, reference_attempts = reference_dump(adb, baseline, flags)
    try:
        parity = hierarchy_parity.evaluate(before["data"], reference)
    except hierarchy_parity.ParityError as error:
        raise SessionError("parity comparison refused: " + error.code) from None
    del reference
    if not (parity["parity"] and parity["carlo_valid"]):
        raise SessionError(
            "Carlo and uiautomator disagree: "
            f"{parity['unmatched_reference_nodes']} unmatched reference nodes, mismatching "
            f"attributes {sorted(parity['attribute_mismatches'])}; contents redacted")

    # Touch stays with ADB: exactly one tap at the centre of the fixture root.
    x, y = tap_centre(first["bounds"])
    adb.shell(["input", "tap", str(x), str(y)])
    after = carlo_dump(adb, manifest, physical, scratch, baseline, flags, session)
    second = fixture_state(after["root"])
    old, new = before["root"].attrib, after["root"].attrib
    update = {
        "text_counter_advanced_by_one": second["counter"] == first["counter"] + 1,
        "snapshot_id_changed": old.get("snapshot-id") != new.get("snapshot-id"),
        "start_ms_changed": old.get("start-ms") != new.get("start-ms"),
        "dump_attempts": after["attempts"],
    }
    if not update["text_counter_advanced_by_one"]:
        raise SessionError("fixture state did not advance by one after the tap")
    if not (update["snapshot_id_changed"] or update["start_ms_changed"]):
        raise SessionError("second dump carries the first snapshot's identity")
    return {
        "accessibility_enabled_before": baseline["enabled"] == "1",
        "accessibility_services_present_before": baseline["services_present"],
        "accessibility_enabled_restored": all(flags),
        "accessibility_restoration_checks": len(flags),
        "probe": probe,
        "dump": {"windows": before["windows"], "nodes": before["nodes"],
                 "attempts": before["attempts"], "contract_valid": True,
                 "fixture_root_found": True},
        "reference": {"attempts": reference_attempts, "device_file_removed": True},
        "parity": parity,
        "update_after_touch": update,
    }


def check_fixture(fixture: Path) -> str:
    fixture_manifest = json.loads(fixture.with_name("manifest.json").read_text())
    digest = hashlib.sha256(fixture.read_bytes()).hexdigest()
    if fixture_manifest["package"] != PACKAGE or fixture_manifest["sha256"] != digest:
        raise SessionError("fixture manifest/checksum mismatch")
    return digest


def run(adb, manifest: Path, fixture: Path, physical: bool, session=run_session,
        make_scratch=None) -> dict:
    """Refusals, fixture lifecycle and cleanup around verify(); returns the summary."""
    emulator = adb.shell(["getprop", "ro.kernel.qemu"]).stdout.strip() == "1"
    if not emulator and not physical:
        raise SessionError("hierarchy fixture smoke requires a disposable emulator "
                           "or explicit --allow-physical authorization")
    if adb.shell(["pm", "path", PACKAGE], check=False).stdout.strip():
        raise SessionError("fixture package already exists; refusing to replace it")
    digest = check_fixture(fixture)
    # Refuse before installing anything if a user's accessibility setup is in use.
    refuse_if_accessibility_in_use(accessibility_state(adb))
    api = int(adb.shell(["getprop", "ro.build.version.sdk"]).stdout.strip())
    abi = adb.shell(["getprop", "ro.product.cpu.abi"]).stdout.strip()
    summary = {"emulator": emulator, "android_api": api, "abi": abi,
               "fixture_sha256": digest, "ui_contents_recorded": False}
    existing = session_directories(adb)
    scratch = (make_scratch or (lambda: Path(tempfile.mkdtemp(prefix="carlo-hierarchy-"))))()
    scratch.chmod(0o700)
    try:
        adb.run(["install", "--no-streaming", str(fixture.resolve())], timeout=60)
        adb.shell(["input", "keyevent", "KEYCODE_WAKEUP"])
        adb.shell(["am", "start", "-W", "-n", PACKAGE + "/.FixtureActivity"], timeout=30)
        wait_resumed(adb)
        summary.update(verify(adb, manifest, physical, scratch, session))
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
        adb.run(["uninstall", PACKAGE], check=False)
        if scratch.exists():
            raise SessionError("private dump directory was not removed")
        if adb.shell(["pm", "path", PACKAGE], check=False).stdout.strip():
            raise SessionError("owned fixture package was not removed")
        if session_directories(adb) - existing:
            raise SessionError("an agent session directory was left on the device")
    summary.update(fixture_removed=True, dumps_deleted=True, session_directories_removed=True)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--manifest", type=Path, default=ROOT / "dist/manifest.json")
    parser.add_argument("--fixture", type=Path, default=ROOT / "dist/node-fixture/fixture.apk")
    parser.add_argument("--output", type=Path, default=ROOT / "results/hierarchy")
    parser.add_argument("--allow-physical", action="store_true",
                        help="the owner authorized this physical device for this run")
    args = parser.parse_args()
    manifest = args.manifest.resolve()
    summary = run(Adb(args.serial), manifest, args.fixture, args.allow_physical)
    summary["agent_manifest"] = json.loads(manifest.read_text())
    args.output.mkdir(parents=True, exist_ok=True)
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    parity = summary["parity"]
    print(f"Hierarchy dump valid, {parity['matched_nodes']}/{parity['reference_nodes']} "
          "reference nodes matched uiautomator and the fixture state updated after one tap; "
          "dumps deleted, fixture and sessions cleaned up.")


if __name__ == "__main__":
    try:
        main()
    except subprocess.SubprocessError as error:
        # A failed command's output may quote the screen: name the failure only.
        raise SystemExit(f"carlo-hierarchy-smoke: device command failed "
                         f"({type(error).__name__})") from None
    except (SessionError, OSError, ValueError, KeyError) as error:
        raise SystemExit(f"carlo-hierarchy-smoke: {error}") from error
