"""One `carlo serve` process on an authorized device, shared by the serve smokes.

Ready-line parsing, the single `adb forward`, bounded reads, sanitised
diagnostics and the lifecycle that always removes the forward, the process and
the session directory. The session identifier and an abstract socket name are
never printed or stored by anything here.
"""

from __future__ import annotations

import json
import os
import re
import select
import socket
import subprocess
import time
from pathlib import Path

from android_session import LAUNCH, Adb, SessionError, cleanup, create_directory, deploy
from carlo_protocol import VERSION, ControlClient, ProtocolError

SOCKET_NAME = "control.sock"
READY_KEYS = {"ready", "schema_version", "protocol", "transport", "socket", "session"}
ABSTRACT_NAME = re.compile(r"carlo-[0-9a-f]{32}")
STDERR_TAIL = 400
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


class ServeSession:
    """A fresh private session directory with one agent and one adb forward.

    Ownership starts when the constructor returns: `close` then removes the
    forward (if one was created), the agent process and the directory, on every
    path. Both channels connect through the same forwarded port.
    """

    def __init__(self, adb: Adb, binary: Path):
        self.adb, self.binary = adb, binary
        self.directory = create_directory(adb)
        self.path = f"{self.directory}/{SOCKET_NAME}"
        self.process = self.port = self.forward_removed = None
        self.closers: list = []
        # Lifecycle facts for a summary; never an identifier.
        self.evidence: dict = {}

    def path_test(self, flag: str) -> bool:
        return self.adb.shell(["test", flag, self.path], check=False).returncode == 0

    def socket_exists(self) -> bool:
        return self.path_test("-e")

    def start(self) -> dict:
        """Deploy, launch, validate the ready line and expose the socket once."""
        deploy(self.adb, self.binary, self.directory)
        self.process = self.adb.popen(
            ["sh", "-c", LAUNCH, "carlo-launch", self.directory, self.directory + "/carlo",
             "serve", "--session-dir", self.directory]
        )
        ready = read_ready(self.process, self.directory)
        self.session = ready["session"]
        self.evidence["session_id_length"] = len(self.session)
        self.evidence["transport"] = ready["transport"]
        if ready["transport"] == "filesystem":
            mode = self.adb.shell(["stat", "-c", "%a", self.path]).stdout.strip()
            require(
                mode == "600" and self.path_test("-S"),
                "control socket is not a 0600 socket file",
            )
            self.evidence["socket_mode"] = "600"
        else:
            # Abstract: no permission bits to check, and no file may exist.
            require(not self.socket_exists(), "abstract transport left a socket file")
            self.evidence["socket_mode"] = None
        self.port = parse_forward_port(
            self.adb.run(["forward", "tcp:0", forward_target(ready)]).stdout
        )
        return ready

    def connect(self) -> socket.socket:
        """A new connection through the forward; the caller wraps and tracks it."""
        try:
            return socket.create_connection(("127.0.0.1", self.port), timeout=10)
        except OSError as error:
            raise ExposureError(type(error).__name__ + f" errno {error.errno}") from None

    def track(self, client):
        """Close `client` when the session closes."""
        self.closers.append(client)
        return client

    def finish(self, timeout: float = 15) -> tuple[int, bytes]:
        """Wait for the agent to exit: (exit code, stderr), never with the identifier."""
        code = self.process.wait(timeout=timeout)
        stderr = self.process.stderr.read(4096)
        require(self.session.encode() not in stderr, "session identifier reached stderr")
        return code, stderr

    def close(self) -> None:
        for client in self.closers:
            try:
                client.close()
            except OSError:
                pass
        try:
            if self.port is not None:
                removed = self.adb.run(
                    ["forward", "--remove", f"tcp:{self.port}"], check=False
                )
                self.forward_removed = removed.returncode == 0
        finally:
            try:
                if self.process is not None:
                    if self.process.poll() is None:
                        self.process.kill()
                    self.process.wait(timeout=10)
                    self.process.stdout.close()
                    self.process.stderr.close()
            finally:
                cleanup(self.adb, self.directory)
