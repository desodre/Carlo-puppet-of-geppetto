"""Lifecycle smoke assertions and ownership checks without a device."""

import json
import subprocess
import unittest
from unittest.mock import patch

from android_session import SessionError
from smoke_accessibility import TrackedAdb, verify_success, wait_for


class SmokeTests(unittest.TestCase):
    def test_success_requires_registration_and_explicit_cleanup(self):
        outcome = {"initialized": True, "unregistered": True,
                   "error": None, "cleanup_error": None}
        report = {"exit_code": 0, "cleanup_removed": True,
                  "stdout": json.dumps({"outcome": outcome})}
        self.assertEqual(verify_success(report), outcome)
        for key in ["initialized", "unregistered"]:
            failed = dict(outcome, **{key: False})
            with self.assertRaises(SessionError):
                verify_success(dict(report, stdout=json.dumps({"outcome": failed})))
        with self.assertRaises(SessionError):
            verify_success(dict(report, cleanup_removed=False))
        with self.assertRaises(SessionError):
            verify_success(dict(report, exit_code=69))

    def test_never_signals_foreign_or_missing_executable(self):
        adb = TrackedAdb("emulator-test")
        with self.assertRaises(SessionError):
            adb.signal_owned("TERM")
        adb.directory = "/data/local/tmp/carlo-" + "a" * 32
        responses = [subprocess.CompletedProcess([], 0, "123\n", ""),
                     subprocess.CompletedProcess([], 0, "/somebody/else\n", "")]
        with patch.object(adb, "shell", side_effect=responses) as shell:
            with self.assertRaises(SessionError):
                adb.signal_owned("KILL")
            self.assertEqual(shell.call_count, 2)  # no kill issued
        with patch.object(adb, "shell") as shell:
            with self.assertRaises(SessionError):
                adb.signal_owned("invalid")
            shell.assert_not_called()

    def test_signal_targets_only_verified_owned_pid(self):
        adb = TrackedAdb("emulator-test")
        adb.directory = "/data/local/tmp/carlo-" + "b" * 32
        responses = [subprocess.CompletedProcess([], 0, "321\n", ""),
                     subprocess.CompletedProcess([], 0, adb.directory + "/carlo\n", ""),
                     subprocess.CompletedProcess([], 0, "", "")]
        with patch.object(adb, "shell", side_effect=responses) as shell:
            adb.signal_owned("INT")
            self.assertEqual(shell.call_args.args[0], ["kill", "-INT", "321"])

    def test_observation_deadline_is_bounded(self):
        with self.assertRaises(SessionError):
            wait_for(lambda: False, timeout=0)


if __name__ == "__main__":
    unittest.main()
