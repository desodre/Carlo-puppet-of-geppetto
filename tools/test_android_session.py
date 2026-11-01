"""Deployment safety and cleanup behavior without a device."""

import hashlib
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from android_session import LAUNCH, Adb, SessionError, cleanup, run_session


class FakeAdb:
    serial = "emulator-test"

    def __init__(self, *, emulator=True, failure=None, collision=False):
        self.emulator, self.failure, self.collision = emulator, failure, collision
        self.calls = []

    def shell(self, args, **kwargs):
        self.calls.append(args)
        if args[:1] == ["getprop"]:
            values = {
                "ro.kernel.qemu": "1" if self.emulator else "0",
                "ro.build.version.sdk": "34",
                "ro.product.cpu.abi": "x86_64",
            }
            return subprocess.CompletedProcess(args, 0, values[args[1]] + "\n", "")
        if args[0] == "mkdir" and self.collision:
            raise subprocess.CalledProcessError(1, args)
        if args[:3] == ["sh", "-c", LAUNCH]:
            if self.failure:
                raise self.failure
            return subprocess.CompletedProcess(args, 69, "", "unavailable")
        return subprocess.CompletedProcess(args, 0, "", "")

    def run(self, args, **kwargs):
        self.calls.append(args)
        return subprocess.CompletedProcess(args, 0, "", "")


class SessionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        (root / "carlo").write_bytes(b"native-binary-fixture")
        self.manifest = root / "manifest.json"
        self.manifest.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "android_api": 29,
                    "artifacts": [
                        {
                            "target": "x86_64-linux-android",
                            "file": "carlo",
                            "sha256": hashlib.sha256((root / "carlo").read_bytes()).hexdigest(),
                        }
                    ],
                }
            )
        )

    def test_physical_device_refused_before_deployment(self):
        adb = FakeAdb(emulator=False)
        with self.assertRaises(SessionError):
            run_session(adb, self.manifest, "doctor", 1, False)
        self.assertEqual(len(adb.calls), 1)

    def test_nonzero_agent_exit_keeps_its_code_and_cleans_session(self):
        adb = FakeAdb()
        result = run_session(adb, self.manifest, "serve", 1, False)
        self.assertEqual(result["exit_code"], 69)
        self.assertTrue(result["cleanup_removed"])
        self.assertTrue(any(len(call) > 3 and call[3] == "carlo-cleanup" for call in adb.calls))

    def test_binder_probe_uses_bounded_launch_and_existing_cleanup(self):
        adb = FakeAdb()
        result = run_session(adb, self.manifest, "binder-probe", 1, False)
        self.assertEqual(result["command"], ["binder-probe"])
        self.assertTrue(result["cleanup_removed"])
        launch = next(call for call in adb.calls if call[:3] == ["sh", "-c", LAUNCH])
        self.assertEqual(launch[-1], "binder-probe")

    def test_timeout_and_push_failure_both_clean_owned_directory(self):
        adb = FakeAdb(failure=subprocess.TimeoutExpired("adb", 1))
        result = run_session(adb, self.manifest, "doctor", 1, False)
        self.assertEqual(result["exit_code"], 124)
        self.assertTrue(result["cleanup_removed"])
        adb = FakeAdb()
        with patch.object(adb, "run", side_effect=subprocess.CalledProcessError(1, "push")):
            with self.assertRaises(SessionError):
                run_session(adb, self.manifest, "doctor", 1, False)
        self.assertTrue(any(len(call) > 3 and call[3] == "carlo-cleanup" for call in adb.calls))

    def test_collision_does_not_clean_a_directory_we_did_not_create(self):
        adb = FakeAdb(collision=True)
        with self.assertRaises(subprocess.CalledProcessError):
            run_session(adb, self.manifest, "doctor", 1, False)
        self.assertFalse(any(call[0] == "sh" for call in adb.calls))

    def test_hash_mismatch_refuses_before_mkdir(self):
        (self.manifest.parent / "carlo").write_bytes(b"changed")
        adb = FakeAdb()
        with self.assertRaises(SessionError):
            run_session(adb, self.manifest, "doctor", 1, False)
        self.assertFalse(any(call[0] == "mkdir" for call in adb.calls))

    def test_cleanup_rejects_paths_outside_our_session_prefix(self):
        for directory in [
            "/data/local/tmp",
            "/data/local/tmp/carlo-../other",
            "/data/local/tmp/carlo-" + "a" * 32 + "/..",
        ]:
            with self.assertRaises(SessionError):
                cleanup(FakeAdb(), directory)

    def test_shell_caller_data_is_quoted_before_device_reparse(self):
        with patch("android_session.subprocess.run") as run:
            Adb("serial").shell(["echo", "$(sensitive)", "a b"])
            self.assertEqual(
                run.call_args.args[0],
                ["adb", "-s", "serial", "shell", "-T", "echo '$(sensitive)' 'a b'"],
            )


if __name__ == "__main__":
    unittest.main()
