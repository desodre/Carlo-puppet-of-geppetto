#!/usr/bin/env python3
"""Prove `carlo serve` on an authorized device: private socket, ADB exposure, handshake.

Three fresh sessions: a complete one (hello, ping, capabilities, unknown frame
kind, shutdown), one with a wrong session identifier and one with a bad magic.
The agent chooses the transport and says so on its ready line: a socket file,
exposed with `adb forward tcp:0 localfilesystem:<path>`, or (only when the
platform denies creating that file) an abstract socket, exposed with
`localabstract:<name>`. Either passes; the summary records which one ran. The
session identifier and the abstract name are never printed or stored.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import select
import socket
import subprocess
import time
from pathlib import Path

from android_session import (
    LAUNCH, ROOT, Adb, SessionError, cleanup, create_directory, deploy, resolve_artifact,
)
from carlo_protocol import CONTROL, HEADER, MAGIC, VERSION, ControlClient, ProtocolError

SOCKET_NAME = "control.sock"
READY_KEYS = {"ready", "schema_version", "protocol", "transport", "socket", "session"}
ABSTRACT_NAME = re.compile(r"carlo-[0-9a-f]{32}")
STDERR_TAIL = 400
RESERVED = ("hierarchy.dump", "display.info", "video.start", "video.stop", "stats")
LIMITS = {"control_bytes": 64 * 1024, "payload_bytes": 8 * 1024 * 1024}


class ExposureError(SessionError):
    """The forwarded port did not reach the agent's control socket."""

    def __init__(self, reason: str):
        super().__init__("adb forward did not reach the control socket: " + reason)
        self.reason, self.transport = reason, None


def read_line(stream, timeout: float, limit: int = 4096) -> bytes:
    """Read one line from an unbuffered pipe within `timeout` and `limit` bytes."""
    deadline, data, descriptor = time.monotonic() + timeout, bytearray(), stream.fileno()
    while not data.endswith(b"\n"):
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([descriptor], [], [], remaining)[0]:
            raise SessionError("agent did not print its ready line in time")
        # One byte at a time: nothing after the newline is consumed.
        chunk = os.read(descriptor, 1)
        if not chunk:
            raise SessionError("agent exited before printing its ready line")
        data += chunk
        if len(data) > limit:
            raise SessionError("ready line exceeds its size bound")
    return bytes(data)


def parse_ready(line: bytes, directory: str) -> dict:
    """Validate the single ready line. The error text never contains the line."""
    try:
        ready = json.loads(line)
    except ValueError:
        raise SessionError("ready line is not JSON") from None
    if not (
        isinstance(ready, dict)
        and set(ready) == READY_KEYS
        and ready["ready"] is True
        and ready["schema_version"] == 1
        and ready["protocol"] == VERSION
        and isinstance(ready["session"], str)
        and re.fullmatch(r"[0-9a-f]{32}", ready["session"])
        and isinstance(ready["socket"], str)
        and (
            ready["socket"] == f"{directory}/{SOCKET_NAME}"
            if ready["transport"] == "filesystem"
            # Strict: this name is placed on an adb command line.
            else ready["transport"] == "abstract"
            and ABSTRACT_NAME.fullmatch(ready["socket"]) is not None
            and ready["socket"] != "carlo-" + ready["session"]
        )
    ):
        raise SessionError("ready line does not match the serve contract")
    return ready


def forward_target(ready: dict) -> str:
    """The adb forward remote spec for a ready line accepted by `parse_ready`."""
    scheme = {"filesystem": "localfilesystem:", "abstract": "localabstract:"}
    return scheme[ready["transport"]] + ready["socket"]


def sanitise(stderr: bytes, secrets: tuple[str, ...] = ()) -> str:
    """A bounded, printable tail of agent diagnostics with identifiers removed."""
    text = stderr[-4 * STDERR_TAIL:].decode("ascii", errors="replace")
    for secret in secrets:
        text = text.replace(secret, "<redacted>")
    text = re.sub(r"[0-9a-fA-F]{32,}", "<redacted>", text)
    text = "".join(char if " " <= char <= "~" else " " for char in text)
    return " ".join(text.split())[-STDERR_TAIL:]


def read_ready(process, directory: str, timeout: float = 15) -> dict:
    """Read and validate the ready line; a failure says how the agent ended."""
    try:
        return parse_ready(read_line(process.stdout, timeout), directory)
    except SessionError as error:
        try:
            code = process.wait(timeout=5)
            tail = sanitise(process.stderr.read(4096) or b"")
            detail = f"agent exit code {code}; stderr: {tail or '<empty>'}"
        except subprocess.TimeoutExpired:
            detail = "agent still running"
        raise SessionError(f"{error} ({detail})") from None


def parse_forward_port(stdout: str) -> int:
    """`adb forward tcp:0 ...` prints the port it allocated."""
    if re.fullmatch(r"[1-9][0-9]{0,4}\n?", stdout) is None or int(stdout) > 65535:
        raise SessionError("adb forward did not report an allocated TCP port")
    return int(stdout)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SessionError(message)


def error_code(response: dict) -> str | None:
    error = response.get("error")
    return error.get("code") if response.get("ok") is False and isinstance(error, dict) else None


def validate_hello(response: dict, api: int | None, abi: str | None) -> int:
    """Check the hello response against what the device itself reports."""
    device, capabilities = response.get("device"), response.get("capabilities")
    require(
        response.get("ok") is True
        and response.get("protocol") == VERSION
        and isinstance(response.get("agent"), dict)
        and response["agent"].get("name") == "carlo"
        and isinstance(response["agent"].get("version"), str)
        and response.get("limits") == LIMITS,
        "hello response lacks protocol, agent or limits",
    )
    require(
        isinstance(device, dict)
        and device.get("android_api") == api
        and device.get("abi") == abi
        and isinstance(device.get("architecture"), str),
        "hello device fields disagree with the device properties",
    )
    require(
        isinstance(capabilities, list)
        and len(capabilities) >= 1
        and all(
            isinstance(entry, dict)
            and isinstance(entry.get("name"), str)
            and isinstance(entry.get("available"), bool)
            and isinstance(entry.get("reason"), str)
            for entry in capabilities
        ),
        "hello capabilities are not a list of name/available/reason",
    )
    return len(capabilities)


def first_call(client: ControlClient, message: dict) -> dict:
    """The first exchange also proves the forward reached the socket."""
    try:
        return client.call(message)
    except ProtocolError as error:
        if error.code != "closed":
            raise
        raise ExposureError("EOF") from None
    except OSError as error:
        raise ExposureError(type(error).__name__ + f" errno {error.errno}") from None


def unknown_kind_frame(request_id: int) -> bytes:
    return HEADER.pack(MAGIC, VERSION, 200, 0, request_id, 5) + b"later"


def exercise_complete(client, session, api, abi, socket_exists) -> dict:
    """hello, ping, capabilities, recovery from an unknown kind, shutdown."""
    hello = first_call(client, {"command": "hello", "protocol": VERSION, "session": session})
    count = validate_hello(hello, api, abi)
    # The agent removes the path as soon as it accepts its single client.
    removed = not socket_exists()
    require(removed, "socket path still present after the client was accepted")
    require(client.call({"command": "ping"}) == {"ok": True}, "ping failed")
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
        "capabilities_available": sum(entry["available"] for entry in hello["capabilities"]),
        "socket_removed_after_accept": removed,
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


def serve_case(adb: Adb, binary: Path, device: dict, name: str) -> dict:
    """Run one case in a fresh private session; always remove the forward and directory."""
    exercise, clean_exit = CASES[name]
    directory = create_directory(adb)
    path = f"{directory}/{SOCKET_NAME}"
    process = port = client = None
    result = {"case": name, "forward_removed": None}

    def path_test(flag: str) -> bool:
        return adb.shell(["test", flag, path], check=False).returncode == 0

    def socket_exists() -> bool:
        return path_test("-e")

    try:
        deploy(adb, binary, directory)
        process = adb.popen(
            ["sh", "-c", LAUNCH, "carlo-launch", directory, directory + "/carlo",
             "serve", "--session-dir", directory]
        )
        ready = read_ready(process, directory)
        session = ready["session"]
        result["session_id_length"] = len(session)
        result["transport"] = ready["transport"]
        if ready["transport"] == "filesystem":
            mode = adb.shell(["stat", "-c", "%a", path]).stdout.strip()
            require(
                mode == "600" and path_test("-S"), "control socket is not a 0600 socket file"
            )
            result["socket_mode"] = "600"
        else:
            # Abstract: no permission bits to check, and no file may exist.
            require(not socket_exists(), "abstract transport left a socket file")
            result["socket_mode"] = None
        port = parse_forward_port(adb.run(["forward", "tcp:0", forward_target(ready)]).stdout)
        try:
            client = ControlClient(socket.create_connection(("127.0.0.1", port), timeout=10))
        except OSError as error:
            raise ExposureError(type(error).__name__ + f" errno {error.errno}") from None
        result.update(
            exercise(client, session, device["android_api"], device["abi"], socket_exists)
        )
        result["exit_code"] = process.wait(timeout=15)
        stderr = process.stderr.read(4096)
        require(session.encode() not in stderr, "session identifier reached stderr")
        result["stderr_bytes"] = len(stderr)
        require(
            (result["exit_code"] == 0) == clean_exit,
            "agent exit code does not match the outcome of the session",
        )
        result["socket_present_after_exit"] = socket_exists()
        require(not result["socket_present_after_exit"], "socket path survived the agent")
    except ExposureError as error:
        error.transport = result.get("transport")
        raise
    finally:
        if client is not None:
            client.close()
        try:
            if port is not None:
                removed = adb.run(["forward", "--remove", f"tcp:{port}"], check=False)
                result["forward_removed"] = removed.returncode == 0
        finally:
            try:
                if process is not None:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=10)
                    process.stdout.close()
                    process.stderr.close()
            finally:
                cleanup(adb, directory)
    result["cleanup_removed"] = True
    require(result["forward_removed"] is True, "adb forward was not removed")
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
        and complete.get("capabilities_available") == 0
        and complete.get("socket_removed_after_accept") is True
        and complete.get("reserved_unavailable") == len(RESERVED)
        and complete.get("unknown_kind_recovered") is True,
        "complete session lacks handshake or recovery evidence",
    )
    require(
        cases["wrong_session"].get("rejected") == "invalid_session"
        and cases["bad_magic"].get("rejected") == "bad_magic",
        "rejection paths were not proven",
    )
    text = json.dumps(summary)
    require(re.search(r"[0-9a-f]{32}", text) is None, "summary contains an identifier")
    return summary


def run(adb: Adb, manifest: Path, allow_physical: bool) -> tuple[dict, SessionError | None]:
    """Run every case; an exposure failure is recorded and returned, never worked around."""
    binary, device = resolve_artifact(adb, manifest, allow_physical)
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
            result = serve_case(adb, binary, device, name)
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
    args = parser.parse_args()
    manifest = args.manifest.resolve()
    summary, failure = run(Adb(args.serial), manifest, args.allow_physical)
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
