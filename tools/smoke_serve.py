#!/usr/bin/env python3
"""Prove `carlo serve` on an authorized device: private socket, ADB exposure, handshake.

Three fresh sessions: a complete one (hello, ping, capabilities, the video
commands without a video channel, unknown frame kind, shutdown), one with a
wrong session identifier and one with a bad magic. The video channel itself is
proven by `smoke_video_stream.py`.
Where `binder_hierarchy` is not available, `hierarchy.dump` must answer
`unavailable`. Where it is, two consecutive snapshots are taken in the same
session only with `--allow-hierarchy`: the run refuses when an accessibility
service is enabled, records only the frame kind, dump count and byte counts (the
XML is screen contents and is never kept or printed) and checks that
`accessibility_enabled` is restored once the agent has exited and unregistered.
The agent chooses the transport and says so on its ready line: a socket file,
exposed with `adb forward tcp:0 localfilesystem:<path>`, or (only when the
platform denies creating that file) an abstract socket, exposed with
`localabstract:<name>`. Either passes; the summary records which one ran. The
session identifier and the abstract name are never printed or stored.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
from pathlib import Path

from android_session import ROOT, Adb, SessionError, resolve_artifact
from carlo_protocol import CONTROL, HEADER, MAGIC, VERSION, ControlClient, ProtocolError
from serve_session import (
    ExposureError, ServeSession, error_code, first_call, forward_target, parse_forward_port,
    parse_ready, read_line, read_ready, require, sanitise, validate_hello,
)
from smoke_hierarchy import accessibility_state, refuse_if_accessibility_in_use, restored

__all__ = [
    "ExposureError", "forward_target", "parse_forward_port", "parse_ready", "read_line",
    "read_ready", "sanitise", "validate_hello",
]

RESERVED = ("display.info", "stats")
# What `hierarchy.dump` proved in the complete session.
HIERARCHY_OUTCOMES = ("unavailable", "not_requested", "dumped")
# Consecutive dumps in one serve session: the second reuses the registration.
HIERARCHY_DUMPS = 2


def unknown_kind_frame(request_id: int) -> bytes:
    return HEADER.pack(MAGIC, VERSION, 200, 0, request_id, 5) + b"later"


class HierarchySnapshots:
    """Consecutive `hierarchy.dump`s in one serve session, keeping only byte counts.

    The agent registers once, on the first dump, and keeps that registration
    until it exits, so `accessibility_enabled` is checked only after the agent
    has exited (`restored_after_exit`), never between dumps.
    """

    def __init__(self, adb: Adb, baseline: dict):
        self.adb, self.baseline = adb, baseline
        # Set before the first request: even a failed dump may have registered.
        self.attempted = False

    def __call__(self, client) -> dict:
        self.attempted = True
        sizes = [len(client.hierarchy_dump()) for _ in range(HIERARCHY_DUMPS)]
        require(all(size > 0 for size in sizes), "hierarchy.dump answered an empty document")
        return {"hierarchy_frame": "hierarchy", "hierarchy_dumps": len(sizes),
                "hierarchy_bytes": sizes}

    def restored_after_exit(self) -> bool:
        return restored(self.adb, self.baseline)


def hierarchy_snapshot(adb: Adb, baseline: dict) -> HierarchySnapshots:
    return HierarchySnapshots(adb, baseline)


def exercise_complete(client, session, api, abi, dump=None) -> dict:
    """hello, ping, capabilities, hierarchy.dump, video commands with no video channel,
    recovery, shutdown. `dump` takes the snapshot when the owner allowed it."""
    hello = first_call(client, {"command": "hello", "protocol": VERSION, "session": session})
    count = validate_hello(hello, api, abi)
    available = {entry["name"] for entry in hello["capabilities"] if entry["available"]}
    require("serve_protocol" in available, "serve_protocol is not reported available")
    hierarchy_available = "binder_hierarchy" in available
    if not hierarchy_available:
        require(
            error_code(client.call({"command": "hierarchy.dump"})) == "unavailable",
            "hierarchy.dump was not refused as unavailable without binder_hierarchy",
        )
        hierarchy = {"hierarchy": "unavailable"}
    elif dump is None:
        hierarchy = {"hierarchy": "not_requested"}
    else:
        hierarchy = {"hierarchy": "dumped", **dump(client)}
    require(client.call({"command": "ping"}) == {"ok": True}, "ping failed")
    # No video connection is opened in this session: the channel stays pending
    # (until its deadline) and the video commands answer without one.
    channels = client.call({"command": "channels"})
    require(
        channels.get("ok") is True and channels.get("video") in ("pending", "absent"),
        "channels does not report a missing video channel",
    )
    require(
        error_code(client.call({"command": "video.start"})) == "video_channel_absent",
        "video.start without a video channel was not refused as video_channel_absent",
    )
    require(
        error_code(client.call({"command": "video.stop"})) == "not_streaming",
        "video.stop without a stream was not refused as not_streaming",
    )
    listed = client.call({"command": "capabilities"})
    require(
        listed == {"ok": True, "capabilities": hello["capabilities"]},
        "capabilities command disagrees with hello",
    )
    require(
        error_code(client.call({"command": "hello", "protocol": VERSION, "session": session}))
        == "already_initialized",
        "second hello was not refused",
    )
    unavailable = sum(
        error_code(client.call({"command": name})) == "unavailable" for name in RESERVED
    )
    require(unavailable == len(RESERVED), "a reserved command was not reported unavailable")
    require(
        error_code(client.call({"command": "carlo-smoke-unknown"})) == "unknown_command",
        "unknown command was not refused",
    )
    client.send(unknown_kind_frame(4242))
    require(
        client.receive() == (4242, {"error": {"code": "unknown_kind"}}),
        "unknown frame kind was not rejected with its request ID",
    )
    require(client.call({"command": "ping"}) == {"ok": True}, "no recovery after unknown kind")
    require(client.call({"command": "shutdown"}) == {"ok": True}, "shutdown failed")
    require(client.receive() is None, "connection stayed open after shutdown")
    return {
        "hello_ok": True,
        "capabilities": count,
        "capabilities_available": len(available),
        "serve_protocol_available": True,
        "binder_hierarchy_available": hierarchy_available,
        **hierarchy,
        "video_without_channel": "video_channel_absent",
        "reserved_unavailable": unavailable,
        "unknown_kind_recovered": True,
        "requests": client.next_id - 1,
    }


def exercise_wrong_session(client, session, *_) -> dict:
    wrong = "0" * 32 if session != "0" * 32 else "1" * 32
    response = first_call(client, {"command": "hello", "protocol": VERSION, "session": wrong})
    require(
        response == {"ok": False, "error": {"code": "invalid_session"}},
        "wrong session identifier was not refused as invalid_session",
    )
    require(client.receive() is None, "connection stayed open after invalid_session")
    return {"rejected": "invalid_session", "closed": True}


def exercise_bad_magic(client, *_) -> dict:
    try:
        client.send(HEADER.pack(b"HTTP", VERSION, CONTROL, 0, 7, 0))
        response = client.receive()
    except OSError as error:
        raise ExposureError(type(error).__name__ + f" errno {error.errno}") from None
    if response is None:
        raise ExposureError("EOF")
    require(
        response == (0, {"error": {"code": "bad_magic"}}),
        "bad magic was not answered with a rejection frame",
    )
    require(client.receive() is None, "connection stayed open after bad magic")
    return {"rejected": "bad_magic", "closed": True}


CASES = {
    "complete": (exercise_complete, True),
    "wrong_session": (exercise_wrong_session, False),
    "bad_magic": (exercise_bad_magic, False),
}


def serve_case(adb: Adb, binary: Path, device: dict, name: str, dump=None) -> dict:
    """Run one case in a fresh private session; always remove the forward and directory."""
    exercise, clean_exit = CASES[name]
    session = ServeSession(adb, binary)
    result = {"case": name, "forward_removed": None}
    try:
        ready = session.start()
        result.update(session.evidence)
        client = session.track(ControlClient(session.connect()))
        result.update(
            exercise(client, ready["session"], device["android_api"], device["abi"], dump))
        result["exit_code"], stderr = session.finish()
        result["stderr_bytes"] = len(stderr)
        require(
            (result["exit_code"] == 0) == clean_exit,
            "agent exit code does not match the outcome of the session",
        )
        result["socket_present_after_exit"] = session.socket_exists()
        require(not result["socket_present_after_exit"], "socket path survived the agent")
    except ExposureError as error:
        error.transport = session.evidence.get("transport")
        raise
    finally:
        try:
            session.close()
        finally:
            result["forward_removed"] = session.forward_removed
            # The registration lives as long as the agent: only once it has
            # exited (or been stopped) can the setting be back. Checked on every
            # path that dumped; a failure above still propagates first.
            if dump is not None and dump.attempted:
                result["accessibility_enabled_restored"] = dump.restored_after_exit()
    result["cleanup_removed"] = True
    require(result["forward_removed"] is True, "adb forward was not removed")
    if "accessibility_enabled_restored" in result:
        require(result["accessibility_enabled_restored"],
                "accessibility_enabled was not restored after serve exited")
    return result


def validate_summary(summary: dict) -> dict:
    """The evidence a passing run must contain; counters only, no identifiers."""
    transport = summary.get("transport")
    require(transport in ("filesystem", "abstract"), "summary names no proven transport")
    require(
        summary.get("filesystem_socket_exposed") is (transport == "filesystem"),
        "filesystem exposure flag disagrees with the transport that ran",
    )
    listed = summary.get("cases", [])
    cases = {case["case"]: case for case in listed}
    require(
        set(cases) == set(CASES) and len(listed) == len(CASES),
        "summary does not cover every case exactly once",
    )
    for name, case in cases.items():
        require(
            case.get("session_id_length") == 32
            and case.get("transport") == transport
            and case.get("socket_mode") == ("600" if transport == "filesystem" else None)
            and case.get("forward_removed") is True
            and case.get("cleanup_removed") is True
            and case.get("socket_present_after_exit") is False
            and (case.get("exit_code") == 0) == CASES[name][1],
            f"case {name} lacks lifecycle evidence",
        )
    complete = cases["complete"]
    require(
        complete.get("hello_ok") is True
        and complete.get("capabilities", 0) >= 1
        and complete.get("serve_protocol_available") is True
        and complete.get("video_without_channel") == "video_channel_absent"
        and complete.get("reserved_unavailable") == len(RESERVED)
        and complete.get("unknown_kind_recovered") is True,
        "complete session lacks handshake or recovery evidence",
    )
    hierarchy = complete.get("hierarchy")
    available = complete.get("binder_hierarchy_available")
    require(
        isinstance(available, bool) and hierarchy in HIERARCHY_OUTCOMES
        and (hierarchy == "unavailable") is (not available),
        "hierarchy.dump outcome disagrees with the binder_hierarchy capability",
    )
    if hierarchy == "dumped":
        sizes = complete.get("hierarchy_bytes")
        require(
            complete.get("hierarchy_frame") == "hierarchy"
            and complete.get("hierarchy_dumps") == HIERARCHY_DUMPS
            and isinstance(sizes, list) and len(sizes) == HIERARCHY_DUMPS
            and all(isinstance(size, int) and not isinstance(size, bool) and size > 0
                    for size in sizes)
            and complete.get("accessibility_enabled_restored") is True,
            "hierarchy snapshots lack frame, count, sizes or post-exit restore evidence",
        )
    else:
        require(
            not {"hierarchy_frame", "hierarchy_dumps", "hierarchy_bytes",
                 "accessibility_enabled_restored"} & set(complete),
            "hierarchy evidence recorded without a snapshot",
        )
    require(
        cases["wrong_session"].get("rejected") == "invalid_session"
        and cases["bad_magic"].get("rejected") == "bad_magic",
        "rejection paths were not proven",
    )
    text = json.dumps(summary)
    require(re.search(r"[0-9a-f]{32}", text) is None, "summary contains an identifier")
    return summary


def run(adb: Adb, manifest: Path, allow_physical: bool,
        allow_hierarchy: bool = False) -> tuple[dict, SessionError | None]:
    """Run every case; an exposure failure is recorded and returned, never worked around."""
    binary, device = resolve_artifact(adb, manifest, allow_physical)
    dump = None
    if allow_hierarchy:
        baseline = accessibility_state(adb)
        refuse_if_accessibility_in_use(baseline)
        dump = hierarchy_snapshot(adb, baseline)
    summary = {
        "emulator": device["emulator"],
        "android_api": device["android_api"],
        "abi": device["abi"],
        "transport": None,
        "filesystem_socket_exposed": None,
        "cases": [],
    }
    try:
        for name in CASES:
            result = serve_case(adb, binary, device, name, dump if name == "complete" else None)
            summary["cases"].append(result)
            if summary["transport"] is None:
                summary["transport"] = result["transport"]
                # False when the agent fell back: the socket file was never created.
                summary["filesystem_socket_exposed"] = result["transport"] == "filesystem"
    except ExposureError as error:
        summary["transport"] = summary["transport"] or error.transport
        summary["filesystem_socket_exposed"] = False
        summary["exposure_failure"] = error.reason
        return summary, error
    return validate_summary(summary), None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--manifest", type=Path, default=ROOT / "dist/manifest.json")
    parser.add_argument("--output", type=Path, default=ROOT / "results/serve")
    parser.add_argument("--allow-physical", action="store_true",
                        help="the owner authorized this physical device for this run")
    parser.add_argument("--allow-hierarchy", action="store_true",
                        help="take two consecutive hierarchy.dump snapshots in one session "
                             "where binder_hierarchy is available (refused while an "
                             "accessibility service is enabled)")
    args = parser.parse_args()
    manifest = args.manifest.resolve()
    summary, failure = run(Adb(args.serial), manifest, args.allow_physical,
                           args.allow_hierarchy)
    summary["agent_manifest"] = json.loads(manifest.read_text())
    args.output.mkdir(parents=True, exist_ok=True)
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    if failure is not None:
        raise failure
    print(f"Serve handshake proven through adb forward ({summary['transport']} socket); "
          "sessions cleaned up.")


if __name__ == "__main__":
    try:
        main()
    except (SessionError, ProtocolError, subprocess.SubprocessError, OSError, ValueError,
            KeyError) as error:
        raise SystemExit(f"carlo-serve-smoke: {error}") from error
