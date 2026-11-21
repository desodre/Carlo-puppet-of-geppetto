"""Host-side validation of the hierarchy smoke without a device.

The device, the agent session and uiautomator are scripted; documents are synthetic.
"""

import hashlib
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import smoke_hierarchy
from android_session import SessionError
from hierarchy_parity import parse
from smoke_hierarchy import (
    PACKAGE, ROOT_ID, accessibility_state, fixture_state, lookup, parse_setting,
    refuse_if_accessibility_in_use, run, tap_centre, validate_probe, whitelist,
)
from test_hierarchy_parity import carlo, node, reference

SERIAL = "SERIAL-SENTINEL-0001"
SECRET = "ZQX_SCREEN_SENTINEL"
SESSION = "/data/local/tmp/carlo-" + "ab" * 16


def fixture_window(counter, with_invisible=True, **root_fields):
    children = [node(class_="android.widget.Button", text=SECRET, clickable="true",
                     bounds="[0,80][1080,2400]")]
    if with_invisible:
        children.insert(0, node(class_="android.view.ViewStub", bounds="[0,0][0,0]"))
    fields = {"class_": "android.widget.FrameLayout", "package": PACKAGE,
              "resource_id": ROOT_ID, "text": f"CARLO_STATE_{counter}",
              "content_desc": "Carlo ação 🪵 <&>", "clickable": "true",
              "bounds": "[0,0][1080,2400]"}
    fields.update(root_fields)
    return node(children, **fields)


def status_bar():
    return node([node(class_="android.widget.TextView", text=SECRET + "_clock")],
                package="com.android.systemui", bounds="[0,0][1080,80]")


def carlo_document(counter, snapshot=None, **root_fields):
    document = carlo(status_bar(), fixture_window(counter, **root_fields))
    stamp = str(counter if snapshot is None else snapshot).encode()
    return document.replace(b'snapshot-id="7"', b'snapshot-id="' + stamp + b'"').replace(
        b'start-ms="10"', b'start-ms="' + stamp + b'000"')


PROBE = {
    "schema_version": 1, "command": "hierarchy-probe", "hierarchy_available": True,
    "device": {"fingerprint": SECRET, "api": 34},
    "outcome": {"error": None, "windows": 2, "nodes": 5, "elapsed_ms": 41.5,
                "truncated": False, "top_package": PACKAGE, "ids": [SECRET],
                "per_window": [{"nodes": 2, "title": SECRET}, {"nodes": 3, "title": SECRET}]},
}


class Device:
    """Scripted adb: package state, settings, dumpsys markers and uiautomator."""

    def __init__(self, **options):
        self.options = options
        self.calls = []
        self.installed = options.get("preinstalled", False)
        self.taps = []
        self.files = set()
        self.directories = set()
        self.reference_runs = 0
        self.enabled_reads = 0
        self.serial = SERIAL

    def result(self, stdout="", returncode=0, stderr=""):
        return SimpleNamespace(stdout=stdout, stderr=stderr, returncode=returncode)

    def run(self, args, **kwargs):
        self.calls.append(("run", list(args)))
        if args[0] == "install":
            if self.options.get("install_fails"):
                raise subprocess.CalledProcessError(1, "adb install")
            self.installed = True
        elif args[0] == "uninstall":
            self.installed = False if not self.options.get("uninstall_fails") else self.installed
        elif args[:2] == ["exec-out", "cat"]:
            data = self.options.get("reference", lambda device: reference(
                fixture_window(len(device.taps), with_invisible=False)))(self)
            return self.result(data if args[2] in self.files else b"")
        return self.result()

    def shell(self, args, **kwargs):
        self.calls.append(("shell", list(args)))
        options = self.options
        if args[:2] == ["getprop", "ro.kernel.qemu"]:
            return self.result("1\n" if options.get("emulator", True) else "\n")
        if args[:2] == ["getprop", "ro.build.version.sdk"]:
            return self.result("34\n")
        if args[:2] == ["getprop", "ro.product.cpu.abi"]:
            return self.result("x86_64\n")
        if args[:2] == ["pm", "path"]:
            return self.result("package:/data/app/x/base.apk\n" if self.installed else "")
        if args[:3] == ["settings", "get", "secure"]:
            if args[3] == "enabled_accessibility_services":
                return self.result(options.get("services", "null") + "\n")
            self.enabled_reads += 1
            drift = options.get("enabled_drifts_after")
            if drift is not None and self.enabled_reads > drift:
                return self.result("1\n")
            return self.result(options.get("enabled", "0") + "\n")
        if args[:2] == ["dumpsys", "accessibility"]:
            marker = "Ui Automation[x]" if options.get("automation_active") else ""
            return self.result("services:{}\n" + marker)
        if args[:2] == ["dumpsys", "activity"]:
            if options.get("never_resumed") or not self.installed:
                return self.result("  ResumedActivity: ActivityRecord{1 u0 other.app/.Main t1}\n")
            return self.result(
                f"  topResumedActivity=ActivityRecord{{1 u0 {PACKAGE}/.FixtureActivity t9}}\n")
        if args[:2] == ["input", "tap"]:
            self.taps.append((int(args[2]), int(args[3])))
            return self.result()
        if args[:2] == ["uiautomator", "dump"]:
            self.reference_runs += 1
            if self.reference_runs <= options.get("reference_failures", 0):
                return self.result("ERROR: could not get idle state.\n")
            self.files.add(args[2])
            return self.result("UI hierchary dumped to: " + args[2] + "\n")
        if args[:2] == ["rm", "-f"]:
            if not options.get("reference_sticks"):
                self.files.discard(args[2])
            return self.result()
        if args[0] == "sh" and "test ! -e" in args[2]:
            return self.result(returncode=1 if args[4] in self.files else 0)
        if args[:2] == ["ls", "/data/local/tmp"]:
            names = [path.rsplit("/", 1)[1] for path in sorted(self.directories | self.files)]
            return self.result("\n".join(["other-tool", *names]) + "\n")
        return self.result()

    def shell_calls(self, *prefix):
        return [args for kind, args in self.calls
                if kind == "shell" and args[:len(prefix)] == list(prefix)]


class Agent:
    """Scripted run_session: the probe report and one dump per call."""

    def __init__(self, device, **options):
        self.device, self.options, self.commands = device, options, []

    def __call__(self, adb, manifest, command, timeout, allow_physical, binary_output=None):
        self.commands.append(command)
        if self.options.get("leak_directory") == command:
            self.device.directories.add(SESSION)
        base = {"serial": SERIAL, "emulator": True, "android_api": 34, "abi": "x86_64",
                "session_directory": SESSION, "cleanup_removed": True, "exit_code": 0}
        if command == "hierarchy-probe":
            report = self.options.get("probe", PROBE)
            base.update(stdout=json.dumps(report), stderr=SECRET,
                        exit_code=self.options.get("probe_exit", 0))
            return base
        assert command == "hierarchy-dump" and binary_output is not None
        dumps = self.commands.count("hierarchy-dump")
        if dumps <= self.options.get("dump_refusals", 0):
            binary_output.write_bytes(b"")
            base.update(exit_code=69, stdout_bytes=0, stdout_sha256="0" * 64, stderr=SECRET)
            return base
        document = self.options.get("document", lambda device, dumps: carlo_document(
            len(device.taps)))(self.device, dumps)
        binary_output.write_bytes(document)
        base.update(stdout_bytes=len(document), stderr="",
                    stdout_sha256=hashlib.sha256(document).hexdigest())
        return base


class Pure(unittest.TestCase):
    def test_settings_parsing(self):
        self.assertIsNone(parse_setting("null\n"))
        self.assertIsNone(parse_setting("\n"))
        self.assertIsNone(parse_setting(None))
        self.assertEqual(parse_setting(" 1\r\n"), "1")
        self.assertEqual(accessibility_state(Device()), {"enabled": "0", "services_present": False})
        state = accessibility_state(Device(enabled="1", services="com.x/.Service:" + SECRET))
        self.assertEqual(state, {"enabled": "1", "services_present": True})
        self.assertNotIn(SECRET, json.dumps(state))
        self.assertEqual(accessibility_state(Device(enabled="null", services="")),
                         {"enabled": None, "services_present": False})

    def test_refusal_when_accessibility_is_in_use(self):
        refuse_if_accessibility_in_use({"enabled": "0", "services_present": False})
        refuse_if_accessibility_in_use({"enabled": None, "services_present": False})
        for state in ({"enabled": "1", "services_present": True},
                      {"enabled": "0", "services_present": True},
                      {"enabled": "1", "services_present": False}):
            with self.assertRaisesRegex(SessionError, "accessibility service is enabled"):
                refuse_if_accessibility_in_use(state)

    def test_probe_counters_are_whitelisted_by_type(self):
        kept = validate_probe({"exit_code": 0, "cleanup_removed": True,
                               "stdout": json.dumps(PROBE)})
        self.assertEqual(kept, {
            "schema_version": 1, "hierarchy_available": True, "device": {"api": 34},
            "outcome": {"error": None, "windows": 2, "nodes": 5, "elapsed_ms": 41.5,
                        "truncated": False, "ids": [],
                        "per_window": [{"nodes": 2}, {"nodes": 3}]}})
        self.assertNotIn(SECRET, json.dumps(kept))

    def test_whitelist_keeps_only_the_error_class_string(self):
        self.assertEqual(whitelist({"error": {"kind": "query_rejected", "detail": "x y"}}),
                         {"error": {"kind": "query_rejected"}})
        self.assertEqual(whitelist({"error": "timeout", "kind": "timeout"}), {"error": "timeout"})
        self.assertEqual(whitelist({"error": SECRET + " leaked text"}), {})
        self.assertEqual(whitelist({"Some.Package/Name": 3, "ok_key": 3, 7: 1}), {"ok_key": 3})
        self.assertEqual(whitelist({"a": [1, "x", True, {"b": "y", "c": 2.5}, None]}),
                         {"a": [1, True, {"c": 2.5}, None]})

    def test_probe_requirements(self):
        def session(report, exit_code=0, removed=True):
            return {"exit_code": exit_code, "cleanup_removed": removed,
                    "stdout": json.dumps(report)}
        flat = {"error": None, "windows": 1, "nodes": 1}
        self.assertEqual(validate_probe(session(flat)), flat)
        failures = [session(flat, exit_code=69), session(flat, removed=False),
                    session({**flat, "error": {"kind": "timeout"}}),
                    session({"windows": 1, "nodes": 1}), session({**flat, "nodes": 0}),
                    session({**flat, "windows": 0}), session({**flat, "nodes": True}),
                    session({"error": None, "nodes": 1}), session([flat]),
                    {"exit_code": 0, "cleanup_removed": True, "stdout": SECRET}]
        for bad in failures:
            with self.assertRaises(SessionError) as raised:
                validate_probe(bad)
            self.assertNotIn(SECRET, str(raised.exception))
        self.assertIs(lookup({"a": [{"b": {"nodes": 4}}]}, "nodes"), 4)

    def test_fixture_node_check(self):
        def state(document):
            return fixture_state(parse(document, "carlo")[0])
        self.assertEqual(state(carlo_document(12)),
                         {"counter": 12, "bounds": "[0,0][1080,2400]"})
        wrong = [{"text": "CARLO_STATE_"}, {"text": "xCARLO_STATE_1"}, {"text": "CARLO_STATE_1 "},
                 {"clickable": "false"}, {"enabled": "false"}, {"package": "other.app"},
                 {"resource_id": "other.app:id/fixture_root"}]
        for fields in wrong:
            with self.assertRaises(SessionError) as raised:
                state(carlo(status_bar(), fixture_window(1, **fields)))
            self.assertNotIn("CARLO_STATE", str(raised.exception))
        with self.assertRaises(SessionError):
            state(carlo(status_bar()))
        # The fixture root may sit below a window root, in any window.
        nested = carlo(node([fixture_window(3)], package=PACKAGE), status_bar())
        self.assertEqual(state(nested)["counter"], 3)

    def test_tap_centre(self):
        self.assertEqual(tap_centre("[0,0][1080,2400]"), (540, 1200))
        self.assertEqual(tap_centre("[10,21][13,24]"), (11, 22))
        self.assertEqual(tap_centre("[-10,0][30,9]"), (10, 4))
        for bad in ("[0,0][0,10]", "[0,0][10,0]", "[5,5][1,9]", "0,0,1,1", ""):
            with self.assertRaises(SessionError):
                tap_centre(bad)


class Flow(unittest.TestCase):
    def setUp(self):
        patcher = mock.patch.object(smoke_hierarchy.time, "sleep")
        self.sleep = patcher.start()
        self.addCleanup(patcher.stop)
        holder = tempfile.TemporaryDirectory()
        self.addCleanup(holder.cleanup)
        self.base = Path(holder.name)
        self.fixture = self.base / "fixture.apk"
        self.fixture.write_bytes(b"apk bytes")
        digest = hashlib.sha256(b"apk bytes").hexdigest()
        (self.base / "manifest.json").write_text(json.dumps({"package": PACKAGE, "sha256": digest}))
        self.scratch = self.base / "scratch"
        self.written = []

    def make_scratch(self):
        self.scratch.mkdir(mode=0o755)
        return self.scratch

    def execute(self, device, agent=None, physical=False):
        agent = agent or Agent(device)
        original = agent.__call__

        def recording(*args, **kwargs):
            report = original(*args, **kwargs)
            if len(args) > 5 or kwargs.get("binary_output"):
                path = args[5] if len(args) > 5 else kwargs["binary_output"]
                self.written.append((path, path.parent.stat().st_mode & 0o777))
            return report
        return run(device, self.base / "agent.json", self.fixture, physical,
                   session=recording, make_scratch=self.make_scratch)

    def assert_cleaned(self, device, installed=True):
        self.assertFalse(self.scratch.exists())
        self.assertFalse(device.installed)
        self.assertEqual(device.files, set())
        if installed:
            self.assertIn(("run", ["uninstall", PACKAGE]), device.calls)
        for path, mode in self.written:
            self.assertFalse(path.exists())
            self.assertEqual(mode, 0o700)

    def fails(self, pattern, device, agent=None, **kwargs):
        with self.assertRaisesRegex(SessionError, pattern) as raised:
            self.execute(device, agent, **kwargs)
        self.assertNotIn(SECRET, str(raised.exception))
        self.assertNotIn("CARLO_STATE", str(raised.exception))
        return raised.exception

    def test_success(self):
        device = Device()
        agent = Agent(device)
        summary = self.execute(device, agent)
        self.assert_cleaned(device)
        self.assertEqual(agent.commands, ["hierarchy-probe", "hierarchy-dump", "hierarchy-dump"])
        self.assertEqual(device.taps, [(540, 1200)])
        self.assertEqual(len(self.written), 2)
        self.assertEqual((summary["emulator"], summary["android_api"], summary["abi"]),
                         (True, 34, "x86_64"))
        self.assertIs(summary["ui_contents_recorded"], False)
        self.assertIs(summary["accessibility_enabled_restored"], True)
        self.assertEqual(summary["accessibility_restoration_checks"], 4)
        self.assertIs(summary["accessibility_enabled_before"], False)
        self.assertIs(summary["accessibility_services_present_before"], False)
        self.assertEqual(summary["probe"]["outcome"]["nodes"], 5)
        parity = summary["parity"]
        self.assertTrue(parity["parity"] and parity["carlo_valid"])
        self.assertEqual((parity["reference_nodes"], parity["matched_nodes"],
                          parity["carlo_only_nodes"]), (2, 2, 1))
        self.assertEqual(summary["update_after_touch"],
                         {"text_counter_advanced_by_one": True, "snapshot_id_changed": True,
                          "start_ms_changed": True, "dump_attempts": 1})
        self.assertTrue(summary["fixture_removed"] and summary["dumps_deleted"]
                        and summary["session_directories_removed"])
        dumped = json.dumps(summary)
        for secret in (SERIAL, SECRET, "CARLO_STATE", ROOT_ID, "1080", "android.widget",
                       "carlo-ref-", SESSION):
            self.assertNotIn(secret, dumped)
        # Reference file: unique owned name, removed and confirmed on the device.
        dumps = device.shell_calls("uiautomator", "dump")
        self.assertEqual(len(dumps), 1)
        self.assertRegex(dumps[0][2], r"^/data/local/tmp/carlo-ref-[0-9a-f]{32}\.xml$")
        self.assertEqual(device.shell_calls("rm", "-f"), [["rm", "-f", dumps[0][2]]])

    def test_agent_and_uiautomator_never_overlap_and_settle(self):
        device = Device()
        order = []
        agent = Agent(device)
        inner_shell = device.shell

        def shell(args, **kwargs):
            if args[:2] in (["uiautomator", "dump"], ["dumpsys", "accessibility"],
                            ["input", "tap"]):
                order.append(args[0] + " " + args[1])
            return inner_shell(args, **kwargs)
        device.shell = shell
        inner_call = agent.__call__

        def session(*args, **kwargs):
            order.append(args[2])
            return inner_call(*args, **kwargs)
        run(device, self.base / "agent.json", self.fixture, False, session=session,
            make_scratch=self.make_scratch)
        self.assertEqual(order, [
            "dumpsys accessibility", "hierarchy-probe",
            "dumpsys accessibility", "hierarchy-dump",
            "dumpsys accessibility", "uiautomator dump", "input tap",
            "dumpsys accessibility", "hierarchy-dump"])
        self.assertGreaterEqual(
            [call.args[0] for call in self.sleep.call_args_list].count(smoke_hierarchy.SETTLE), 3)

    def test_physical_device_needs_explicit_authorization(self):
        device = Device(emulator=False)
        self.fails("--allow-physical", device)
        self.assertNotIn(("run", ["uninstall", PACKAGE]), device.calls)
        self.assertEqual(device.shell_calls("settings"), [])
        summary = self.execute(Device(emulator=False), physical=True)
        self.assertIs(summary["emulator"], False)

    def test_existing_fixture_package_is_never_replaced_or_removed(self):
        device = Device(preinstalled=True)
        self.fails("already exists", device)
        self.assertTrue(device.installed)
        self.assertFalse(any(kind == "run" for kind, _ in device.calls))

    def test_fixture_checksum_mismatch(self):
        self.fixture.write_bytes(b"tampered")
        device = Device()
        self.fails("checksum mismatch", device)
        self.assertFalse(any(kind == "run" for kind, _ in device.calls))

    def test_refuses_before_install_when_accessibility_services_are_enabled(self):
        for options in ({"services": "com.user/.ScreenReader"}, {"enabled": "1"},
                        {"enabled": "1", "services": "com.user/.ScreenReader"}):
            device = Device(**options)
            agent = Agent(device)
            error = self.fails("accessibility service is enabled", device, agent)
            self.assertNotIn("ScreenReader", str(error))
            self.assertFalse(any(kind == "run" for kind, _ in device.calls))
            self.assertEqual(agent.commands, [])
            self.assertEqual(device.shell_calls("input"), [])
            self.assertFalse(self.scratch.exists())

    def test_refuses_existing_automation_registration(self):
        device = Device(automation_active=True)
        agent = Agent(device)
        self.fails("existing automation registration", device, agent)
        self.assertEqual(agent.commands, [])
        self.assert_cleaned(device)

    def test_cleanup_when_install_fails(self):
        device = Device(install_fails=True)
        with self.assertRaises(subprocess.CalledProcessError):
            self.execute(device)
        self.assert_cleaned(device)

    def test_cleanup_when_fixture_never_resumes(self):
        device = Device(never_resumed=True)
        agent = Agent(device)
        self.fails("did not resume", device, agent)
        self.assertEqual(agent.commands, [])
        self.assert_cleaned(device)

    def test_cleanup_when_probe_fails(self):
        for options in ({"probe_exit": 69},
                        {"probe": {"outcome": {"error": {"kind": "timeout"}, "nodes": 0,
                                               "windows": 0}}}):
            device = Device()
            agent = Agent(device, **options)
            self.fails("hierarchy probe", device, agent)
            self.assertEqual(agent.commands, ["hierarchy-probe"])
            self.assert_cleaned(device)

    def test_cleanup_when_dump_fails(self):
        device = Device()
        agent = Agent(device, dump_refusals=99)
        self.fails("dump failed or produced no document", device, agent)
        self.assertEqual(agent.commands.count("hierarchy-dump"), smoke_hierarchy.DUMP_ATTEMPTS)
        self.assertEqual(len(self.written), smoke_hierarchy.DUMP_ATTEMPTS)
        self.assert_cleaned(device)
        self.assertEqual(device.shell_calls("uiautomator"), [])

    def test_transient_dump_refusal_is_retried(self):
        device = Device()
        agent = Agent(device, dump_refusals=1)
        summary = self.execute(device, agent)
        self.assertEqual(summary["dump"]["attempts"], 2)
        self.assertEqual(summary["accessibility_restoration_checks"], 5)
        self.assert_cleaned(device)

    def test_cleanup_when_dump_violates_the_contract(self):
        device = Device()
        broken = lambda device, dumps: carlo_document(0).replace(b'checked="false"',
                                                                 b'checked="no"', 1)
        self.fails("violates its contract: invalid_boolean", device,
                   Agent(device, document=broken))
        self.assert_cleaned(device)
        for document in (b"<!DOCTYPE x><hierarchy/>", b"not xml " + SECRET.encode()):
            device = Device()
            self.fails("Carlo dump refused", device,
                       Agent(device, document=lambda device, dumps: document))
            self.assert_cleaned(device)

    def test_cleanup_when_fixture_root_is_missing(self):
        device = Device()
        other = lambda device, dumps: carlo(status_bar())
        self.fails("fixture root not found", device, Agent(device, document=other))
        self.assert_cleaned(device)
        self.assertEqual(device.taps, [])

    def test_reference_is_retried_once_then_fails(self):
        device = Device(reference_failures=1)
        summary = self.execute(device)
        self.assertEqual(summary["reference"]["attempts"], 2)
        self.assertEqual(len(device.shell_calls("rm", "-f")), 2)
        self.assert_cleaned(device)

        device = Device(reference_failures=2)
        self.fails("did not produce a reference dump", device)
        self.assertEqual(len(device.shell_calls("uiautomator", "dump")), 2)
        self.assertEqual(len(device.shell_calls("rm", "-f")), 2)
        self.assertEqual(device.taps, [])
        self.assert_cleaned(device)

    def test_empty_reference_root_is_retried(self):
        runs = []

        def empty_then_good(device):
            runs.append(1)
            if len(runs) == 1:
                return b'<hierarchy rotation="0"></hierarchy>'
            return reference(fixture_window(0, with_invisible=False))
        device = Device(reference=empty_then_good)
        self.assertEqual(self.execute(device)["reference"]["attempts"], 2)
        self.assert_cleaned(device)

    def test_unsupported_reference_form_fails_clearly(self):
        windows = (b'<hierarchy rotation="0"><display id="0"><window><hierarchy><node/>'
                   b"</hierarchy></window></display></hierarchy>")
        device = Device(reference=lambda device: windows)
        self.fails("reference_windows_form_unsupported", device)
        self.assert_cleaned(device)

    def test_cleanup_when_parity_fails(self):
        differing = lambda device: reference(
            fixture_window(0, with_invisible=False, content_desc=SECRET))
        device = Device(reference=differing)
        error = self.fails("disagree", device)
        self.assertIn("content-desc", str(error))
        self.assertEqual(device.taps, [])
        self.assert_cleaned(device)

    def test_cleanup_when_state_does_not_advance(self):
        device = Device()
        frozen = lambda device, dumps: carlo_document(0, snapshot=dumps)
        self.fails("did not advance by one", device, Agent(device, document=frozen))
        self.assertEqual(len(device.taps), 1)
        self.assert_cleaned(device)

        device = Device()
        jumped = lambda device, dumps: carlo_document(2 * len(device.taps), snapshot=dumps)
        self.fails("did not advance by one", device, Agent(device, document=jumped))
        self.assert_cleaned(device)

    def test_cleanup_when_second_dump_is_the_same_snapshot(self):
        device = Device()
        stale = lambda device, dumps: carlo_document(len(device.taps), snapshot=7)
        self.fails("first snapshot's identity", device, Agent(device, document=stale))
        self.assert_cleaned(device)

    def test_fails_when_accessibility_enabled_is_not_restored(self):
        # Reads: run() refusal check, verify() baseline, then the checks after each run.
        for drift, commands, pattern in ((2, ["hierarchy-probe"], "after the probe"),
                                         (3, ["hierarchy-probe", "hierarchy-dump"],
                                          "after the agent"),
                                         (4, ["hierarchy-probe", "hierarchy-dump"],
                                          "after uiautomator")):
            device = Device(enabled_drifts_after=drift)
            agent = Agent(device)
            self.fails("not restored " + pattern, device, agent)
            self.assertEqual(agent.commands, commands)
            self.assert_cleaned(device)
            self.assertGreaterEqual(device.enabled_reads, drift + smoke_hierarchy.RESTORE_POLLS)

    def test_fails_when_device_reference_file_survives(self):
        device = Device(reference_sticks=True)
        self.fails("reference dump was not removed", device)
        self.assertFalse(self.scratch.exists())
        self.assertFalse(device.installed)

    def test_fails_when_a_session_directory_is_left(self):
        device = Device()
        self.fails("session directory was left", device,
                   Agent(device, leak_directory="hierarchy-dump"))
        self.assertFalse(self.scratch.exists())
        self.assertFalse(device.installed)

    def test_fails_when_fixture_cannot_be_removed(self):
        device = Device(uninstall_fails=True)
        self.fails("fixture package was not removed", device)
        self.assertFalse(self.scratch.exists())

    def test_agent_timeout_error_still_cleans_up(self):
        device = Device()

        def session(adb, manifest, command, *rest):
            if command == "hierarchy-dump":
                raise SessionError("deployment or cleanup failed")
            return Agent(device)(adb, manifest, command, *rest)
        with self.assertRaisesRegex(SessionError, "deployment or cleanup failed"):
            run(device, self.base / "agent.json", self.fixture, False, session=session,
                make_scratch=self.make_scratch)
        self.assert_cleaned(device)


if __name__ == "__main__":
    unittest.main()
