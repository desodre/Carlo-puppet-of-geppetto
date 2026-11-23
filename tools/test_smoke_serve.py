"""Serve smoke: pure parsing/validation, cleanup with mocks, and the host binary.

No device is used. The host-binary tests run only when `cargo build`/`cargo test`
has produced `target/debug/carlo`; they drive the same exercises as the smoke
over a local Unix socket.
"""

import copy
import hashlib
import json
import os
import socket
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import serve_session
from android_session import LAUNCH, ROOT, SessionError
from carlo_protocol import CONTROL, ControlClient, ProtocolError, encode, read_frame
from smoke_serve import (
    CASES, ExposureError, exercise_bad_magic, exercise_complete, exercise_wrong_session,
    forward_target, hierarchy_snapshot, parse_forward_port, parse_ready, read_line, run,
    sanitise, serve_case, validate_hello, validate_summary,
)

DIRECTORY = "/data/local/tmp/carlo-" + "a" * 32
SESSION = "0123456789abcdef" * 2
HOST_BINARY = ROOT / "target/debug/carlo"


def ready_line(**changes) -> bytes:
    ready = {"ready": True, "schema_version": 1, "protocol": 1, "transport": "filesystem",
             "socket": DIRECTORY + "/control.sock", "session": SESSION}
    ready.update(changes)
    return json.dumps({key: value for key, value in ready.items() if value is not ...}).encode() + b"\n"


def hello_response(**device):
    return {
        "ok": True, "protocol": 1, "agent": {"name": "carlo", "version": "0.1.0"},
        "device": {"android_api": 34, "abi": "x86_64", "architecture": "x86_64", **device},
        "capabilities": [{"name": "serve_protocol", "available": True, "reason": "r"}],
        "limits": {"control_bytes": 65536, "payload_bytes": 8388608},
    }


ABSTRACT = "carlo-" + "fedcba9876543210" * 2


def case(name, transport="filesystem", **changes):
    result = {"case": name, "session_id_length": 32, "transport": transport,
              "socket_mode": "600" if transport == "filesystem" else None,
              "forward_removed": True, "cleanup_removed": True,
              "socket_present_after_exit": False, "exit_code": 0 if CASES[name][1] else 74}
    if name == "complete":
        result.update({"hello_ok": True, "capabilities": 4, "capabilities_available": 1,
                       "serve_protocol_available": True,
                       "binder_hierarchy_available": False, "hierarchy": "unavailable",
                       "video_without_channel": "video_channel_absent",
                       "reserved_unavailable": 2,
                       "unknown_kind_recovered": True, "requests": 13})
    else:
        result.update({"rejected": {"wrong_session": "invalid_session"}.get(name, name),
                       "closed": True})
    result.update(changes)
    return result


def summary(transport="filesystem"):
    return {"emulator": True, "android_api": 34, "abi": "x86_64", "transport": transport,
            "filesystem_socket_exposed": transport == "filesystem",
            "cases": [case(name, transport) for name in CASES]}


class PipeProcess:
    """A process whose stdout is a real pipe pre-filled with `output`."""

    def __init__(self, output: bytes, returncode=None, stderr: bytes = b""):
        read_end, write_end = os.pipe()
        os.write(write_end, output)
        os.close(write_end)
        self.stdout = os.fdopen(read_end, "rb", buffering=0)
        error_read, error_write = os.pipe()
        os.write(error_write, stderr)
        os.close(error_write)
        self.stderr = os.fdopen(error_read, "rb", buffering=0)
        self.returncode, self.killed = returncode, False

    def poll(self):
        return self.returncode

    def kill(self):
        self.killed, self.returncode = True, -9

    def wait(self, timeout=None):
        return self.returncode


class FakeAdb:
    serial = "emulator-test"

    def __init__(self, *, output=None, collision=False, forward="40123\n", push_fails=False,
                 exit_code=None, stderr=b"", socket_file=True):
        self.exit_code, self.stderr, self.socket_file = exit_code, stderr, socket_file
        self.output = ready_line() if output is None else output
        self.collision, self.forward, self.push_fails = collision, forward, push_fails
        self.calls, self.process, self.directory = [], None, None

    def shell(self, args, **kwargs):
        self.calls.append(args)
        if args[:1] == ["getprop"]:
            values = {"ro.kernel.qemu": "1", "ro.build.version.sdk": "34",
                      "ro.product.cpu.abi": "x86_64"}
            return subprocess.CompletedProcess(args, 0, values[args[1]] + "\n", "")
        if args[0] == "mkdir":
            if self.collision:
                raise subprocess.CalledProcessError(1, args)
            self.directory = args[3]
        if args[:2] == ["stat", "-c"]:
            return subprocess.CompletedProcess(args, 0, "600\n", "")
        if args[0] == "test":
            return subprocess.CompletedProcess(args, 0 if self.socket_file else 1, "", "")
        return subprocess.CompletedProcess(args, 0, "", "")

    def run(self, args, **kwargs):
        self.calls.append(args)
        if args[0] == "push" and self.push_fails:
            raise subprocess.CalledProcessError(1, args)
        if args[:2] == ["forward", "tcp:0"]:
            return subprocess.CompletedProcess(args, 0, self.forward, "")
        return subprocess.CompletedProcess(args, 0, "", "")

    def popen(self, args):
        self.calls.append(args)
        output = self.output.replace(DIRECTORY.encode(), self.directory.encode())
        self.process = PipeProcess(output, self.exit_code, self.stderr)
        return self.process

    def pipes_closed(self):
        return self.process.stdout.closed and self.process.stderr.closed

    def cleaned(self):
        return any(len(call) > 3 and call[3] == "carlo-cleanup" for call in self.calls)

    def forward_removed(self):
        return ["forward", "--remove", "tcp:40123"] in self.calls


class ParsingTests(unittest.TestCase):
    def test_ready_line_must_match_the_contract_exactly(self):
        ready = parse_ready(ready_line(), DIRECTORY)
        self.assertEqual(ready["session"], SESSION)
        self.assertEqual(forward_target(ready), f"localfilesystem:{DIRECTORY}/control.sock")
        abstract = parse_ready(ready_line(transport="abstract", socket=ABSTRACT), DIRECTORY)
        self.assertEqual(forward_target(abstract), "localabstract:" + ABSTRACT)
        for name in [
            "carlo-" + "A" * 32, "carlo-" + "a" * 31, "carlo-" + "a" * 33, "@" + ABSTRACT,
            ABSTRACT + " tcp:1", ABSTRACT + "\n", "x" + ABSTRACT, "other-" + "a" * 32, "",
            "carlo-" + "a" * 32 + ";reboot", DIRECTORY + "/control.sock", "carlo-" + SESSION, 7,
        ]:
            with self.assertRaises(SessionError, msg=name):
                parse_ready(ready_line(transport="abstract", socket=name), DIRECTORY)
        for transport in ["tcp", "", None, "Abstract", ...]:
            with self.assertRaises(SessionError, msg=transport):
                parse_ready(ready_line(transport=transport), DIRECTORY)
        with self.assertRaises(SessionError):
            parse_ready(ready_line(socket=ABSTRACT), DIRECTORY)
        for change in [
            {"ready": False}, {"ready": 1}, {"schema_version": 2}, {"protocol": 2},
            {"socket": "/data/local/tmp/other/control.sock"}, {"socket": DIRECTORY},
            {"session": SESSION.upper()}, {"session": SESSION[:-1]}, {"session": 7},
            {"session": ...}, {"extra": 1},
        ]:
            with self.assertRaises(SessionError, msg=change) as raised:
                parse_ready(ready_line(**change), DIRECTORY)
            self.assertNotIn(SESSION, str(raised.exception))
        for line in [b"", b"\n", b"not json\n", b"[]\n", b'"ready"\n', b"\xff\n"]:
            with self.assertRaises(SessionError):
                parse_ready(line, DIRECTORY)

    def test_forward_port_is_a_single_valid_port(self):
        self.assertEqual(parse_forward_port("40123\n"), 40123)
        self.assertEqual(parse_forward_port("65535"), 65535)
        for text in ["", "0\n", "65536\n", "tcp:40123\n", "40123\n40124\n", "-1\n", "error: x\n"]:
            with self.assertRaises(SessionError, msg=text):
                parse_forward_port(text)

    def test_ready_line_read_is_bounded_in_time_and_size_and_stops_at_the_newline(self):
        def pipe(output):
            process = PipeProcess(output)
            self.addCleanup(process.stdout.close)
            self.addCleanup(process.stderr.close)
            return process.stdout

        stream = pipe(b'{"a":1}\nREST')
        self.assertEqual(read_line(stream, 1), b'{"a":1}\n')
        self.assertEqual(stream.read(), b"REST")
        with self.assertRaisesRegex(SessionError, "exited before"):
            read_line(pipe(b"partial"), 1)
        with self.assertRaisesRegex(SessionError, "size bound"):
            read_line(pipe(b"x" * 64 + b"\n"), 1, limit=16)
        read_end, write_end = os.pipe()
        self.addCleanup(os.close, write_end)
        with os.fdopen(read_end, "rb", buffering=0) as silent:
            with self.assertRaisesRegex(SessionError, "in time"):
                read_line(silent, 0.05)

    def test_hello_is_checked_against_the_device_properties(self):
        self.assertEqual(validate_hello(hello_response(), 34, "x86_64"), 1)
        self.assertEqual(
            validate_hello(hello_response(android_api=None, abi=None), None, None), 1
        )
        for api, abi in [(35, "x86_64"), (34, "arm64-v8a"), (None, None)]:
            with self.assertRaises(SessionError):
                validate_hello(hello_response(), api, abi)
        for key, value in [("ok", False), ("protocol", 2), ("agent", {"name": "other"}),
                           ("limits", {}), ("capabilities", []), ("capabilities", [{"name": 1}]),
                           ("device", None)]:
            response = hello_response()
            response[key] = value
            with self.assertRaises(SessionError, msg=key):
                validate_hello(response, 34, "x86_64")

    def test_summary_requires_every_proof_and_carries_no_identifier(self):
        self.assertTrue(validate_summary(summary())["filesystem_socket_exposed"])
        fallback = validate_summary(summary("abstract"))
        self.assertEqual(
            (fallback["transport"], fallback["filesystem_socket_exposed"]), ("abstract", False)
        )
        for mutate in [lambda s: s.update(filesystem_socket_exposed=True),
                       lambda s: s.update(transport="tcp"),
                       lambda s: s["cases"][1].update(transport="filesystem"),
                       lambda s: s["cases"][0].update(socket_mode="600")]:
            broken = summary("abstract")
            mutate(broken)
            with self.assertRaises(SessionError):
                validate_summary(broken)
        for index, change in [
            (0, {"hello_ok": False}), (0, {"serve_protocol_available": False}),
            (0, {"video_without_channel": None}), (0, {"unknown_kind_recovered": False}),
            (0, {"reserved_unavailable": 3}), (0, {"exit_code": 74}), (1, {"exit_code": 0}),
            (0, {"hierarchy": None}), (0, {"hierarchy": "not_requested"}),
            (0, {"binder_hierarchy_available": True}), (0, {"binder_hierarchy_available": None}),
            (0, {"hierarchy_bytes": 10}),
            (1, {"rejected": "bad_magic"}), (2, {"rejected": None}),
            (2, {"forward_removed": False}), (1, {"cleanup_removed": False}),
            (0, {"socket_present_after_exit": True}), (2, {"session_id_length": 16}),
            (0, {"socket_mode": "666"}), (1, {"note": SESSION}), (2, {"note": ABSTRACT}),
            (1, {"transport": "abstract"}),
        ]:
            broken = summary()
            broken["cases"][index].update(change)
            with self.assertRaises(SessionError, msg=change):
                validate_summary(broken)
        for mutate in [lambda s: s.update(filesystem_socket_exposed=False),
                       lambda s: s["cases"].pop(), lambda s: s["cases"].append(case("complete"))]:
            broken = copy.deepcopy(summary())
            mutate(broken)
            with self.assertRaises(SessionError):
                validate_summary(broken)

    def test_summary_accepts_each_hierarchy_outcome_only_with_its_evidence(self):
        dumped = {"binder_hierarchy_available": True, "hierarchy": "dumped",
                  "hierarchy_frame": "hierarchy", "hierarchy_dumps": 2,
                  "hierarchy_bytes": [2048, 2051], "accessibility_enabled_restored": True}
        for change in [{"binder_hierarchy_available": True, "hierarchy": "not_requested"},
                       dumped]:
            valid = summary()
            valid["cases"][0].update(change)
            self.assertIs(validate_summary(valid), valid)
        for change in [{"hierarchy": "unavailable"}, {"hierarchy_frame": "control"},
                       {"hierarchy_dumps": 1}, {"hierarchy_dumps": None},
                       {"hierarchy_bytes": [2048]}, {"hierarchy_bytes": [2048, 0]},
                       {"hierarchy_bytes": [True, 10]}, {"hierarchy_bytes": 2048},
                       {"hierarchy_bytes": None}, {"accessibility_enabled_restored": False},
                       {"accessibility_enabled_restored": None}]:
            broken = summary()
            broken["cases"][0].update({**dumped, **change})
            with self.assertRaises(SessionError, msg=change):
                validate_summary(broken)
        for stray in [{"hierarchy_dumps": 2}, {"accessibility_enabled_restored": True}]:
            broken = summary()
            broken["cases"][0].update(stray)
            with self.assertRaises(SessionError, msg=stray):
                validate_summary(broken)


class HierarchyTests(unittest.TestCase):
    """The snapshot path of the complete session, with a fake client and device."""

    class Client:
        def __init__(self, capabilities, answer):
            self.capabilities, self.answer, self.calls, self.next_id = capabilities, answer, [], 1

        def call(self, message):
            self.calls.append(message["command"])
            self.next_id += 1
            if message["command"] == "hello":
                return {**hello_response(), "capabilities": self.capabilities}
            if message["command"] == "hierarchy.dump":
                return {"ok": False, "error": {"code": "unavailable"}}
            raise ProtocolError("closed", "fake stops after the hierarchy step")

        def hierarchy_dump(self):
            self.calls.append("hierarchy.dump")
            if isinstance(self.answer, Exception):
                raise self.answer
            return self.answer

    AVAILABLE = [{"name": "serve_protocol", "available": True, "reason": "r"},
                 {"name": "binder_hierarchy", "available": True, "reason": "r"}]

    def settings_adb(self, values):
        values = iter(values)

        class Settings:
            def shell(self, args, **kwargs):
                return subprocess.CompletedProcess(args, 0, next(values) + "\n", "")

        return Settings()

    def exercise(self, client, dump=None):
        with self.assertRaises(ProtocolError):
            exercise_complete(client, SESSION, 34, "x86_64", dump)

    def test_unavailable_hierarchy_must_answer_unavailable_and_never_dumps(self):
        client = self.Client(hello_response()["capabilities"], b"<x/>")
        self.exercise(client, dump=lambda c: self.fail("dump ran without binder_hierarchy"))
        self.assertEqual(client.calls[:2], ["hello", "hierarchy.dump"])

    def test_available_hierarchy_is_dumped_only_when_allowed(self):
        client = self.Client(self.AVAILABLE, b"<x/>")
        self.exercise(client)
        self.assertNotIn("hierarchy.dump", client.calls)
        taken = []
        client = self.Client(self.AVAILABLE, b"<x/>")
        self.exercise(client, dump=lambda c: taken.append(c) or {})
        self.assertEqual(taken, [client])

    def test_snapshot_keeps_only_the_size_and_requires_the_restore(self):
        baseline = {"enabled": "0", "services_present": False}
        adb = self.settings_adb(["0"])
        dump = hierarchy_snapshot(adb, baseline)
        self.assertFalse(dump.attempted)
        client = self.Client(self.AVAILABLE, b"<hierarchy secret/>")
        evidence = dump(client)
        # Two consecutive dumps in the same session, sizes only.
        self.assertEqual(client.calls, ["hierarchy.dump", "hierarchy.dump"])
        self.assertEqual(evidence, {"hierarchy_frame": "hierarchy", "hierarchy_dumps": 2,
                                    "hierarchy_bytes": [19, 19]})
        self.assertNotIn("secret", json.dumps(evidence))
        self.assertTrue(dump.attempted)
        # The registration is held until the agent exits: the dumps themselves
        # never read the setting; only the post-exit check does.
        self.assertTrue(dump.restored_after_exit())
        with self.assertRaises(StopIteration):
            adb.shell([])
        with patch("smoke_hierarchy.time.sleep"):
            dump = hierarchy_snapshot(self.settings_adb(["1"] * 25), baseline)
            dump(self.Client(self.AVAILABLE, b"<x/>"))
            self.assertFalse(dump.restored_after_exit())
        empty = hierarchy_snapshot(self.settings_adb([]), baseline)
        with self.assertRaisesRegex(SessionError, "empty document"):
            empty(self.Client(self.AVAILABLE, b""))
        # A failed dump is still marked attempted, so the restore is checked.
        failed = hierarchy_snapshot(self.settings_adb([]), baseline)
        with self.assertRaises(ProtocolError):
            failed(self.Client(self.AVAILABLE, ProtocolError("timeout", "t")))
        self.assertTrue(failed.attempted)

    def test_allow_hierarchy_refuses_when_accessibility_is_in_use(self):
        with patch("smoke_serve.resolve_artifact", return_value=(Path("carlo"), {})):
            for values in [["0", "svc/.Service"], ["1", "null"]]:
                with self.assertRaisesRegex(SessionError, "accessibility service"):
                    run(self.settings_adb(values), Path("manifest.json"), False, True)

    class Session:
        """A serve session that records the order of its lifecycle steps."""

        def __init__(self, events, exit_code=0):
            self.events, self.exit_code = events, exit_code
            self.evidence = {"session_id_length": 32, "transport": "filesystem",
                             "socket_mode": "600"}
            self.forward_removed = None

        def start(self):
            return {"session": SESSION}

        def connect(self):
            return None

        def track(self, client):
            return client

        def finish(self):
            self.events.append("finish")
            return self.exit_code, b""

        def socket_exists(self):
            return False

        def close(self):
            self.events.append("close")
            self.forward_removed = True

    class Snapshots:
        def __init__(self, events, restored=True):
            self.events, self.restored, self.attempted = events, restored, False

        def __call__(self, client):
            self.attempted = True
            self.events += ["dump", "dump"]
            return {"hierarchy_frame": "hierarchy", "hierarchy_dumps": 2,
                    "hierarchy_bytes": [10, 11]}

        def restored_after_exit(self):
            self.events.append("restored")
            return self.restored

    def serve_complete(self, snapshots, events, exercise=None, exit_code=0):
        def complete(client, session, api, abi, dump):
            return exercise(dump) if exercise else dump(client)

        with patch("smoke_serve.ServeSession", lambda *_: self.Session(events, exit_code)), \
                patch("smoke_serve.ControlClient", lambda _: object()), \
                patch.dict("smoke_serve.CASES", {"complete": (complete, True)}):
            return serve_case(None, Path("carlo"), {"android_api": 36, "abi": "arm64-v8a"},
                              "complete", snapshots)

    def test_consecutive_dumps_are_followed_by_a_restore_check_after_exit(self):
        events = []
        result = self.serve_complete(self.Snapshots(events), events)
        # Both dumps share the serve session; the setting is read only after
        # the agent has finished and its session was closed.
        self.assertEqual(events, ["dump", "dump", "finish", "close", "restored"])
        self.assertEqual((result["hierarchy_dumps"], result["hierarchy_bytes"]), (2, [10, 11]))
        self.assertIs(result["accessibility_enabled_restored"], True)
        events = []
        with self.assertRaisesRegex(SessionError, "not restored after serve exited"):
            self.serve_complete(self.Snapshots(events, restored=False), events)
        self.assertEqual(events[-1], "restored")

    def test_restore_is_checked_even_when_the_session_fails_after_a_dump(self):
        events = []
        snapshots = self.Snapshots(events)

        def fail_after_dump(dump):
            dump(None)
            raise ProtocolError("closed", "agent went away")

        with self.assertRaises(ProtocolError):
            self.serve_complete(snapshots, events, fail_after_dump)
        self.assertEqual(events, ["dump", "dump", "close", "restored"])
        # No dump, no registration: nothing to check.
        events = []
        result = self.serve_complete(self.Snapshots(events), events, lambda dump: {})
        self.assertEqual(events, ["finish", "close"])
        self.assertNotIn("accessibility_enabled_restored", result)


class CleanupTests(unittest.TestCase):
    def setUp(self):
        self.binary = Path(tempfile.gettempdir()) / "carlo-not-pushed"
        self.device = {"android_api": 34, "abi": "x86_64"}

    def test_refused_connection_is_an_exposure_failure_and_everything_is_released(self):
        adb = FakeAdb()
        with patch.object(serve_session.socket, "create_connection",
                          side_effect=ConnectionRefusedError(111, "refused")):
            with self.assertRaises(ExposureError) as raised:
                serve_case(adb, self.binary, self.device, "complete")
        self.assertIn("ConnectionRefusedError errno 111", raised.exception.reason)
        self.assertNotIn(SESSION, str(raised.exception))
        self.assertTrue(adb.forward_removed() and adb.cleaned() and adb.process.killed)
        self.assertTrue(adb.pipes_closed())
        launch = next(call for call in adb.calls if call[:3] == ["sh", "-c", LAUNCH])
        self.assertEqual(launch[-3:], ["serve", "--session-dir", adb.directory])
        forward = next(call for call in adb.calls if call[:2] == ["forward", "tcp:0"])
        self.assertEqual(forward[2], f"localfilesystem:{adb.directory}/control.sock")

    def test_forward_that_reaches_nothing_reports_eof_and_is_never_worked_around(self):
        adb, (ours, theirs) = FakeAdb(), socket.socketpair()
        theirs.close()
        with patch.object(serve_session.socket, "create_connection", return_value=ours):
            result, failure = run_with(adb)
        self.assertIsInstance(failure, ExposureError)
        self.assertEqual(result["filesystem_socket_exposed"], False)
        self.assertIn(result["exposure_failure"].split()[0], {"EOF", "BrokenPipeError"})
        self.assertEqual(result["cases"], [])
        self.assertEqual(sum(call[:2] == ["forward", "tcp:0"] for call in adb.calls), 1)
        self.assertFalse(any("localabstract" in str(call) for call in adb.calls))
        self.assertTrue(adb.forward_removed() and adb.cleaned())

    def test_abstract_transport_is_forwarded_by_name_without_file_checks(self):
        adb = FakeAdb(output=ready_line(transport="abstract", socket=ABSTRACT),
                      socket_file=False)
        with patch.object(serve_session.socket, "create_connection",
                          side_effect=ConnectionRefusedError(111, "refused")):
            with self.assertRaises(ExposureError) as raised:
                serve_case(adb, self.binary, self.device, "complete")
        self.assertEqual(raised.exception.transport, "abstract")
        self.assertNotIn(ABSTRACT, str(raised.exception))
        forward = next(call for call in adb.calls if call[:2] == ["forward", "tcp:0"])
        self.assertEqual(forward, ["forward", "tcp:0", "localabstract:" + ABSTRACT])
        self.assertFalse(any(call[0] == "stat" or call[:2] == ["test", "-S"]
                             for call in adb.calls))
        self.assertTrue(adb.forward_removed() and adb.cleaned())
        # An abstract ready line with a socket file present is contradictory.
        adb = FakeAdb(output=ready_line(transport="abstract", socket=ABSTRACT))
        with self.assertRaisesRegex(SessionError, "left a socket file"):
            serve_case(adb, self.binary, self.device, "complete")
        self.assertFalse(any(call[:2] == ["forward", "tcp:0"] for call in adb.calls))
        self.assertTrue(adb.cleaned())

    def test_agent_that_dies_before_ready_reports_exit_code_and_sanitised_stderr(self):
        stderr = (b"carlo: serve: create control socket: Permission denied (os error 13)\n"
                  + SESSION.encode() + b" \x1b[31m\xff " + b"A1" * 20 + b"\n")
        adb = FakeAdb(output=b"", exit_code=74, stderr=stderr)
        with self.assertRaises(SessionError) as raised:
            serve_case(adb, self.binary, self.device, "complete")
        message = str(raised.exception)
        self.assertIn("exited before printing its ready line", message)
        self.assertIn("agent exit code 74", message)
        self.assertIn("Permission denied (os error 13)", message)
        self.assertNotIn(SESSION, message)
        self.assertNotIn("\x1b", message)
        self.assertIn("<redacted>", message)
        self.assertTrue(adb.cleaned())
        self.assertLessEqual(len(sanitise(b"x" * 100000)), 400)
        self.assertEqual(sanitise(b"a\nb\tc " + ABSTRACT.encode()), "a b c carlo-<redacted>")
        silent = FakeAdb(output=b"", exit_code=0)
        with self.assertRaisesRegex(SessionError, "exit code 0; stderr: <empty>"):
            serve_case(silent, self.binary, self.device, "complete")

    def test_failures_before_the_forward_remove_nothing_they_did_not_create(self):
        for adb in [FakeAdb(output=b""), FakeAdb(output=b"garbage\n"),
                    FakeAdb(forward="error\n"), FakeAdb(push_fails=True)]:
            with self.assertRaises((SessionError, subprocess.CalledProcessError)):
                serve_case(adb, self.binary, self.device, "complete")
            self.assertFalse(any(call[:2] == ["forward", "--remove"] for call in adb.calls))
            self.assertTrue(adb.cleaned())
        adb = FakeAdb(collision=True)
        with self.assertRaises(subprocess.CalledProcessError):
            serve_case(adb, self.binary, self.device, "complete")
        self.assertFalse(adb.cleaned())
        self.assertIsNone(adb.process)


def run_with(adb):
    with tempfile.TemporaryDirectory() as root:
        binary = Path(root) / "carlo"
        binary.write_bytes(b"fixture")
        manifest = Path(root) / "manifest.json"
        manifest.write_text(json.dumps({
            "schema_version": 1, "android_api": 29,
            "artifacts": [{"target": "x86_64-linux-android", "file": "carlo",
                           "sha256": hashlib.sha256(b"fixture").hexdigest()}]}))
        return run(adb, manifest, False)


@unittest.skipUnless(HOST_BINARY.is_file(), "host binary not built (cargo build)")
class HostBinaryTests(unittest.TestCase):
    """The smoke's own exercises against the real agent, without ADB."""

    def start(self):
        root = tempfile.TemporaryDirectory()
        self.addCleanup(root.cleanup)
        os.chmod(root.name, 0o700)
        process = subprocess.Popen(
            [str(HOST_BINARY), "serve", "--session-dir", root.name],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0,
        )
        self.addCleanup(lambda: (process.kill() if process.poll() is None else None,
                                 process.wait(10), process.stdout.close(),
                                 process.stderr.close()))
        ready = parse_ready(read_line(process.stdout, 10), root.name)
        connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        connection.connect(ready["socket"])
        client = ControlClient(connection)
        self.addCleanup(client.close)
        return process, client, ready

    def test_complete_session_exits_zero(self):
        process, client, ready = self.start()
        result = exercise_complete(client, ready["session"], None, None)
        # Off the pinned device only the protocol itself is available.
        self.assertEqual(result["capabilities_available"], 1)
        self.assertEqual(result["video_without_channel"], "video_channel_absent")
        self.assertEqual(result["reserved_unavailable"], 2)
        self.assertEqual(
            (result["binder_hierarchy_available"], result["hierarchy"]), (False, "unavailable")
        )
        self.assertEqual(process.wait(10), 0)
        self.assertEqual(process.stderr.read(), b"")
        self.assertFalse(os.path.exists(ready["socket"]))

    def test_rejections_close_and_exit_non_zero_without_leaking_the_session(self):
        for exercise in (exercise_wrong_session, exercise_bad_magic):
            process, client, ready = self.start()
            result = exercise(client, ready["session"])
            self.assertTrue(result["closed"])
            self.assertEqual(process.wait(10), 74)
            stderr = process.stderr.read()
            self.assertIn(result["rejected"].encode(), stderr)
            self.assertNotIn(ready["session"].encode(), stderr)
            self.assertFalse(os.path.exists(ready["socket"]))

    def test_client_reports_closure_and_mismatched_ids(self):
        process, client, ready = self.start()
        client.send(encode(CONTROL, 99, b'{"command":"ping"}'))
        self.assertEqual(client.receive(), (99, {"ok": False, "error": {"code": "handshake_required"}}))
        self.assertIsNone(client.receive())
        with self.assertRaises((ProtocolError, OSError)):
            client.call({"command": "ping"})
        self.assertEqual(process.wait(10), 74)


class ControlClientTests(unittest.TestCase):
    def test_call_checks_the_echoed_id_kind_and_payload_shape(self):
        for reply, code in [(encode(CONTROL, 5, b"{}"), "invalid_response"),
                            (encode(CONTROL, 1, b"[]"), "invalid_response"),
                            (encode(2, 1, b"<x/>"), "unexpected_kind"), (b"", "closed")]:
            ours, theirs = socket.socketpair()
            client = ControlClient(ours, timeout=2)
            theirs.sendall(reply)
            theirs.shutdown(socket.SHUT_WR)
            with self.assertRaises(ProtocolError) as raised:
                client.call({"command": "ping"})
            self.assertEqual(raised.exception.code, code)
            sent = read_frame(theirs.makefile("rb"))
            self.assertEqual((sent.kind, sent.request_id), (CONTROL, 1))
            self.assertEqual(json.loads(sent.payload), {"command": "ping"})
            client.close()
            theirs.close()


if __name__ == "__main__":
    unittest.main()
