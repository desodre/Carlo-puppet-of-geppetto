#!/usr/bin/env python3
"""Run a bounded native diagnostic command in a private, verified Android session."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shlex
import subprocess
import time
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
COMMANDS = {
    "doctor": ["doctor", "--json"],
    "version": ["--version"],
    "serve": ["serve"],
    "binder-probe": ["binder-probe"],
    "node-query-probe": ["node-query-probe"],
    "accessibility-tests": ["--test-threads=1", "--nocapture"],
    "accessibility-probe": ["accessibility-probe"],
    "accessibility-hold": ["accessibility-probe", "--hold-ms", "10000"],
}
ABI_TARGETS = {"x86_64": "x86_64-linux-android", "arm64-v8a": "aarch64-linux-android"}

# Caller data is passed as positional shell arguments, never interpolated as code.
LAUNCH = """directory="$1"; shift
"$@" & child=$!
printf '%s\n' "$child" > "$directory/pid"
trap 'kill "$child" 2>/dev/null || :; wait "$child" 2>/dev/null || :' HUP INT TERM
wait "$child"; result=$?
rm -f "$directory/pid"
exit "$result"
"""
CLEANUP = """directory="$1"
if test -f "$directory/pid"; then
  pid=$(cat "$directory/pid")
  case "$pid" in ''|*[!0-9]*) exit 65;; esac
  actual=$(readlink "/proc/$pid/exe")
  if test "$actual" = "$directory/carlo"; then
    kill "$pid" 2>/dev/null || :
    n=0
    while test "$n" -lt 10 && test "$(readlink "/proc/$pid/exe")" = "$directory/carlo"; do
      sleep 0.1; n=$((n + 1))
    done
    if test "$(readlink "/proc/$pid/exe")" = "$directory/carlo"; then
      kill -9 "$pid" 2>/dev/null || :
      n=0
      while test "$n" -lt 10 && test "$(readlink "/proc/$pid/exe")" = "$directory/carlo"; do
        sleep 0.1; n=$((n + 1))
      done
      test "$(readlink "/proc/$pid/exe")" != "$directory/carlo" || exit 75
    fi
  fi
fi
rm -rf "$directory"
test ! -e "$directory"
"""


class SessionError(RuntimeError):
    """A deployment failure with a safe, actionable message."""


class Adb:
    def __init__(self, serial: str):
        self.serial = serial

    def run(self, args: list[str], *, timeout: float = 15, check: bool = True):
        return subprocess.run(
            ["adb", "-s", self.serial, *args],
            capture_output=True,
            text=True,
            timeout=timeout,
            check=check,
        )

    def shell(self, args: list[str], *, timeout: float = 15, check: bool = True):
        return self.run(["shell", "-T", shlex.join(args)], timeout=timeout, check=check)


def cleanup(adb: Adb, directory: str) -> None:
    if re.fullmatch(r"/data/local/tmp/carlo-[0-9a-f]{32}", directory) is None:
        raise SessionError("refusing cleanup outside an owned session directory")
    adb.shell(["sh", "-c", CLEANUP, "carlo-cleanup", directory], timeout=15)
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        processes = adb.shell(["ps", "-A", "-o", "PID,ARGS"], timeout=5).stdout
        if directory not in processes:
            return
        time.sleep(0.05)
    raise SessionError("session processes did not exit after cleanup")


def run_session(
    adb: Adb, manifest_path: Path, command: str, timeout: float, allow_physical: bool
) -> dict:
    emulator = adb.shell(["getprop", "ro.kernel.qemu"]).stdout.strip() == "1"
    if not emulator and not allow_physical:
        raise SessionError(
            "physical device refused; explicit --allow-physical authorization required"
        )
    api = int(adb.shell(["getprop", "ro.build.version.sdk"]).stdout.strip())
    abi = adb.shell(["getprop", "ro.product.cpu.abi"]).stdout.strip()
    manifest = json.loads(manifest_path.read_text())
    if manifest.get("schema_version") != 1 or api < manifest["android_api"]:
        raise SessionError("unsupported artifact manifest or Android API below its build floor")
    target = ABI_TARGETS.get(abi)
    artifact = next((entry for entry in manifest["artifacts"] if entry["target"] == target), None)
    if artifact is None:
        raise SessionError("manifest has no artifact for device ABI")
    binary = manifest_path.parent / artifact["file"]
    if hashlib.sha256(binary.read_bytes()).hexdigest() != artifact["sha256"]:
        raise SessionError("artifact SHA-256 does not match manifest")
    directory = "/data/local/tmp/carlo-" + uuid.uuid4().hex
    report = {
        "serial": adb.serial,
        "emulator": emulator,
        "android_api": api,
        "abi": abi,
        "session_directory": directory,
        "command": COMMANDS[command],
        "cleanup_removed": False,
    }
    # Ownership starts only after mkdir succeeds; never clean an existing path on failure.
    adb.shell(["mkdir", "-m", "700", directory])
    failure = None
    try:
        adb.run(["push", str(binary), directory + "/carlo"])
        adb.shell(["chmod", "700", directory + "/carlo"])
        result = adb.shell(
            [
                "sh",
                "-c",
                LAUNCH,
                "carlo-launch",
                directory,
                directory + "/carlo",
                *COMMANDS[command],
            ],
            timeout=timeout,
            check=False,
        )
        report.update(
            {"exit_code": result.returncode, "stdout": result.stdout, "stderr": result.stderr}
        )
    except subprocess.TimeoutExpired:
        report.update({"exit_code": 124, "error": "agent command timed out"})
    except (subprocess.SubprocessError, OSError) as error:
        failure = error
    finally:
        try:
            cleanup(adb, directory)
            report["cleanup_removed"] = True
        except (subprocess.SubprocessError, OSError, SessionError) as error:
            if failure is None:
                failure = error
            else:
                report["cleanup_error"] = str(error)
    if failure is not None:
        raise SessionError("deployment or cleanup failed: " + str(failure)) from failure
    return report


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--manifest", type=Path, default=ROOT / "dist/manifest.json")
    parser.add_argument("--command", choices=COMMANDS, default="doctor")
    parser.add_argument("--timeout", type=float, default=15)
    parser.add_argument("--allow-physical", action="store_true")
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    if not 0 < args.timeout <= 300:
        parser.error("timeout must be finite and in (0, 300] seconds")
    try:
        report = run_session(
            Adb(args.serial),
            args.manifest.resolve(),
            args.command,
            args.timeout,
            args.allow_physical,
        )
    except (SessionError, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        parser.exit(1, f"carlo-session: {error}\n")
    text = json.dumps(report, indent=2) + "\n"
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(text)
    print(text, end="")
    raise SystemExit(report["exit_code"])


if __name__ == "__main__":
    main()
