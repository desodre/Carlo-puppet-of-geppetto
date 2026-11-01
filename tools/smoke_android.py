#!/usr/bin/env python3
"""Exercise foundation commands and optional slow fixture on an authorized emulator."""

import argparse
import json
from pathlib import Path

from android_session import Adb, run_session


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--timeout-manifest", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    adb = Adb(args.serial)
    results = []
    for command, expected in [("doctor", 0), ("version", 0), ("serve", 69)]:
        report = run_session(adb, args.manifest.resolve(), command, 15, False)
        if report["exit_code"] != expected or not report["cleanup_removed"]:
            raise RuntimeError(f"{command} failed: {report}")
        if command == "doctor":
            diagnostic = json.loads(report["stdout"])
            if not diagnostic["foundation_ready"] or diagnostic["effective_uid"] not in (0, 2000):
                raise RuntimeError("native foundation/identity check failed")
        (args.output / (command + ".json")).write_text(json.dumps(report, indent=2) + "\n")
        results.append({"command": command, "exit_code": expected, "cleanup_removed": True})
    if args.timeout_manifest:
        report = run_session(adb, args.timeout_manifest.resolve(), "doctor", 0.5, False)
        if report["exit_code"] != 124 or not report["cleanup_removed"]:
            raise RuntimeError(f"slow native process was not timed out/cleaned: {report}")
        (args.output / "timeout.json").write_text(json.dumps(report, indent=2) + "\n")
        processes = adb.shell(["ps", "-A", "-o", "PID,ARGS"]).stdout
        if report["session_directory"] in processes:
            raise RuntimeError("slow native process survived session cleanup")
        (args.output / "timeout.json").write_text(json.dumps(report, indent=2) + "\n")
        results.append({"command": "slow fixture", "exit_code": 124, "cleanup_removed": True})
    (args.output / "summary.json").write_text(json.dumps(results, indent=2) + "\n")
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
