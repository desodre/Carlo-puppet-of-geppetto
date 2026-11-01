#!/usr/bin/env python3
"""Prove native accessibility lifecycle on a disposable API 34 reference emulator."""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import re
import threading
import time
from pathlib import Path

from android_session import ROOT, Adb, SessionError, run_session


def active(adb: Adb) -> bool:
    # Read only the automation-presence marker; never log or persist the dump.
    return "Ui Automation[" in adb.shell(["dumpsys", "accessibility"]).stdout


def wait_for(predicate, timeout=5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.02)
    raise SessionError("bounded lifecycle observation timed out")


def verify_success(session):
    if session["exit_code"] != 0 or not session["cleanup_removed"]:
        raise SessionError("session failed or did not clean up")
    outcome = json.loads(session["stdout"])["outcome"]
    if not (outcome["initialized"] and outcome["unregistered"]
            and outcome["error"] is None and outcome["cleanup_error"] is None):
        raise SessionError("registration/init/unregister proof failed")
    return outcome


class TrackedAdb(Adb):
    """Observe only directories this run_session successfully creates."""
    def __init__(self, serial):
        super().__init__(serial)
        self.directory = None
        self.created = threading.Event()

    def shell(self, args, **kwargs):
        result = super().shell(args, **kwargs)
        if args[:3] == ["mkdir", "-m", "700"]:
            self.directory = args[3]
            self.created.set()
        return result

    def owned_pid(self):
        directory = self.directory
        if directory is None or re.fullmatch(r"/data/local/tmp/carlo-[0-9a-f]{32}", directory) is None:
            raise SessionError("no owned session directory")
        pid = self.shell(["cat", directory + "/pid"], check=False).stdout.strip()
        if re.fullmatch(r"[1-9][0-9]*", pid) is None:
            return None
        actual = self.shell(["readlink", "/proc/" + pid + "/exe"], check=False).stdout.strip()
        return pid if actual == directory + "/carlo" else None

    def signal_owned(self, signal):
        if signal not in {"INT", "TERM", "KILL"}:
            raise SessionError("unsupported smoke-test signal")
        pid = self.owned_pid()
        if pid is None:
            raise SessionError("refusing signal without verified session executable")
        self.shell(["kill", "-" + signal, pid])


def held_case(adb, manifest, signal, output):
    held = TrackedAdb(adb.serial)
    with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
        future = executor.submit(run_session, held, manifest, "accessibility-hold", 20, False)
        try:
            if not held.created.wait(5):
                raise SessionError("session deployment did not create its directory")
            wait_for(held.owned_pid)
            wait_for(lambda: active(adb))
            pid = held.owned_pid()
            if pid is None:
                raise SessionError("owned process disappeared")
            maps = held.shell(["cat", "/proc/" + pid + "/maps"]).stdout
            if re.search(r"/libart(?:d)?\.so(?:\s|$)", maps):
                raise SessionError("unexpected ART runtime in native agent")
            if signal == "TERM":
                conflict = run_session(adb, manifest, "accessibility-probe", 15, False)
                outcome = json.loads(conflict["stdout"])["outcome"]
                if not (conflict["exit_code"] == 69 and conflict["cleanup_removed"]
                        and outcome["error"]["kind"] == "conflict"
                        and not outcome["unregistered"] and active(adb)):
                    raise SessionError("competing registration was not safely refused")
                (output / "conflict.json").write_text(json.dumps(conflict, indent=2) + "\n")
            held.signal_owned(signal)
            report = future.result(timeout=20)
        finally:
            if not future.done() and held.directory is not None and held.owned_pid() is not None:
                held.signal_owned("TERM")
        if signal == "KILL":
            if report["exit_code"] != 137 or not report["cleanup_removed"]:
                raise SessionError("SIGKILL did not terminate and clean the owned session")
        else:
            if not verify_success(report)["interrupted"]:
                raise SessionError("signal was not observed by the native process")
    wait_for(lambda: not active(adb))
    (output / (signal.lower() + ".json")).write_text(json.dumps(report, indent=2) + "\n")
    recovery = run_session(adb, manifest, "accessibility-probe", 15, False)
    verify_success(recovery)
    (output / (signal.lower() + "-recovery.json")).write_text(json.dumps(recovery, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--manifest", type=Path, default=ROOT / "dist/manifest.json")
    parser.add_argument("--test-manifest", type=Path, default=ROOT / "dist/native-tests/manifest.json")
    parser.add_argument("--output", type=Path, default=ROOT / "results/accessibility-smoke")
    args = parser.parse_args()
    adb = Adb(args.serial)
    if adb.shell(["getprop", "ro.kernel.qemu"]).stdout.strip() != "1":
        parser.error("lifecycle smoke requires a disposable emulator")
    if active(adb):
        parser.error("existing automation registration; refusing to disturb it")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    native = run_session(adb, args.test_manifest.resolve(), "accessibility-tests", 60, False)
    if native["exit_code"] != 0 or "CARLO_RESOURCE_CYCLES " not in native["stdout"]:
        raise SessionError("native tests failed or resource-cycle test did not execute")
    (output / "native-tests.json").write_text(json.dumps(native, indent=2) + "\n")
    for signal in ["TERM", "INT", "KILL"]:
        held_case(adb, args.manifest.resolve(), signal, output)
    timeout = run_session(adb, args.manifest.resolve(), "accessibility-hold", 0.2, False)
    if timeout["exit_code"] != 124 or not timeout["cleanup_removed"]:
        raise SessionError("host timeout did not clean the session")
    wait_for(lambda: not active(adb))
    verify_success(run_session(adb, args.manifest.resolve(), "accessibility-probe", 15, False))
    (output / "timeout.json").write_text(json.dumps(timeout, indent=2) + "\n")
    (output / "summary.json").write_text(json.dumps({
        "native_tests_passed": True, "same_process_resource_cycles": 20,
        "art_runtime_mapped": False,
        "conflict_refused_without_unregistering_owner": True,
        "sigterm_unregister": True, "sigint_unregister": True,
        "sigkill_system_cleanup_and_recovery": True, "host_timeout_cleanup": True,
        "automation_active_after_checks": active(adb),
    }, indent=2) + "\n")
    print((output / "summary.json").read_text(), end="")


if __name__ == "__main__":
    main()
