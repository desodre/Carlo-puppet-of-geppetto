#!/usr/bin/env python3
"""Prove the control and video channels of `carlo serve` on an authorized device.

Three fresh sessions, each through one `adb forward`:

* `stream`: control `hello`, video `attach`, `channels`, `video.start`, then for a
  few seconds a `ping` every 100 ms that must be answered within one second
  while video packets are read on another thread; `video.stop` and its `end`
  packet; a second start/stop cycle with generation 2; `shutdown`. The first
  generation must start with a config packet followed by a key frame, keep one
  generation, the accepted dimensions and non-decreasing PTS, and decode on the
  host with ffprobe.
* `no_video_channel`: without a video connection `video.start` is refused.
* `wrong_session_attach`: a video connection presenting another identifier is
  refused and closed while control keeps answering.

The stream is real screen content: it is held in memory, written only to a
private temporary directory for ffprobe and always deleted. The summary holds
counters and statistics only; the session identifier is never printed or stored.
This script does not wake or unlock the screen and installs nothing.
"""

from __future__ import annotations

import argparse
import json
import queue
import re
import shutil
import subprocess
import tempfile
import threading
import time
from pathlib import Path

from android_session import ROOT, Adb, SessionError, resolve_artifact
from carlo_protocol import (
    CONFIG, END, MEDIA, VERSION, ControlClient, ProtocolError, VideoChannel, VideoPacket,
)
from serve_session import (
    ExposureError, ServeSession, error_code, first_call, require, validate_hello,
)

STREAM_SECONDS = 3.0
PING_INTERVAL = 0.1
PING_BOUND = 1.0
END_TIMEOUT = 15.0
# Generation 1 is kept for the host decode; far above what a few seconds produce.
MAX_KEPT_BYTES = 64 * 1024 * 1024
ACCEPTED_KEYS = {"width", "height", "fps", "bitrate", "key_frame_interval_seconds"}
DEFAULT_REQUEST = {"display": 0, "max_width": 360, "max_height": 640, "fps": 15,
                   "bitrate": 1_500_000, "encoder": "default"}


class PacketReader(threading.Thread):
    """Reads the video channel on its own thread, so that the control channel is
    timed independently of how fast packets arrive or are consumed."""

    def __init__(self, channel: VideoChannel):
        super().__init__(daemon=True)
        self.channel, self.items = channel, queue.Queue()

    def run(self) -> None:
        try:
            while True:
                item = self.channel.receive()
                self.items.put(item)
                if item is None:
                    return
        except (ProtocolError, OSError, ValueError) as error:
            self.items.put(error)

    def take(self, timeout: float | None):
        """Next (request ID, packet); None once the agent closed. `timeout` None polls."""
        try:
            item = self.items.get(timeout=timeout) if timeout else self.items.get_nowait()
        except queue.Empty:
            return queue.Empty
        if isinstance(item, Exception):
            raise SessionError(f"video channel failed: {type(item).__name__}") from None
        return item


class StreamCheck:
    """Order and consistency of one generation; keeps bytes only when asked."""

    def __init__(self, generation: int, request_id: int, accepted: dict, keep: bool):
        self.generation, self.request_id = generation, request_id
        self.size = (accepted["width"], accepted["height"])
        self.data = bytearray() if keep else None
        self.config_packets = self.media_packets = self.key_frames = self.bytes = 0
        self.first_pts_us = self.last_pts_us = None
        self.ended = False

    def feed(self, request_id: int, packet: VideoPacket) -> None:
        require(not self.ended, "a packet followed the end packet of its generation")
        require(request_id == self.request_id, "video frame does not carry its video.start ID")
        require(packet.generation == self.generation, "packet of another stream generation")
        require(
            (packet.width, packet.height) == self.size,
            "packet dimensions differ from the accepted configuration",
        )
        if packet.type == END:
            self.ended = True
            return
        if packet.type == CONFIG:
            self.config_packets += 1
        else:
            require(packet.type == MEDIA, "unknown packet type")
            require(self.config_packets >= 1, "media packet before the config packet")
            require(
                self.media_packets > 0 or packet.key_frame,
                "first media packet after the config is not a key frame",
            )
            require(
                self.last_pts_us is None or packet.pts_us >= self.last_pts_us,
                "PTS went backwards",
            )
            if self.first_pts_us is None:
                self.first_pts_us = packet.pts_us
            self.last_pts_us = packet.pts_us
            self.media_packets += 1
            self.key_frames += packet.key_frame
        self.bytes += len(packet.data)
        if self.data is not None:
            require(len(self.data) + len(packet.data) <= MAX_KEPT_BYTES, "stream exceeds its bound")
            self.data += packet.data

    def statistics(self) -> dict:
        return {
            "generation": self.generation, "config_packets": self.config_packets,
            "media_packets": self.media_packets, "key_frames": self.key_frames,
            "bytes": self.bytes, "first_pts_us": self.first_pts_us,
            "last_pts_us": self.last_pts_us, "end_packet": self.ended,
        }


def validate_start(response: dict, generation: int, request: dict) -> dict:
    """The accepted configuration of a successful `video.start`."""
    code = error_code(response)
    if code == "unavailable":
        raise SessionError(
            "video.start is unavailable on this device: it is not the pinned mirror profile"
        )
    require(code is None, f"video.start failed with {code}")
    accepted = response.get("accepted")
    require(
        response.get("ok") is True
        and response.get("generation") == generation
        and response.get("display") == 0
        and (response.get("codec") is None or isinstance(response.get("codec"), str))
        and isinstance(accepted, dict)
        and set(accepted) == ACCEPTED_KEYS
        and all(type(value) is int and value > 0 for value in accepted.values()),
        "video.start response does not match the contract",
    )
    require(
        accepted["width"] == request["max_width"] & ~1
        and accepted["height"] == request["max_height"] & ~1
        and accepted["fps"] == request["fps"]
        and accepted["bitrate"] == request["bitrate"],
        "accepted configuration differs from the request",
    )
    return accepted


def stream_cycle(control: ControlClient, reader: PacketReader, generation: int, *,
                 seconds: float, keep: bool, ping_bound: float = PING_BOUND,
                 end_timeout: float = END_TIMEOUT) -> tuple[dict, dict, bytes | None]:
    """One start/stop cycle: (statistics, accepted configuration, kept stream)."""
    response = control.call({"command": "video.start", **DEFAULT_REQUEST})
    accepted = validate_start(response, generation, DEFAULT_REQUEST)
    check = StreamCheck(generation, control.next_id - 1, accepted, keep)

    def drain(timeout: float | None) -> bool:
        item = reader.take(timeout)
        if item is queue.Empty:
            return False
        require(item is not None, "video channel closed before the end packet")
        check.feed(*item)
        return True

    pings, deadline = [], time.monotonic() + seconds
    while time.monotonic() < deadline:
        started = time.monotonic()
        require(control.call({"command": "ping"}) == {"ok": True}, "ping failed while streaming")
        elapsed = time.monotonic() - started
        require(elapsed <= ping_bound, "control waited on the video channel: slow ping")
        pings.append(elapsed)
        while drain(None):
            pass
        time.sleep(max(0.0, PING_INTERVAL - elapsed))
    require(
        error_code(control.call({"command": "video.start"})) == "already_streaming",
        "second video.start was not refused as already_streaming",
    )
    stopped = control.call({"command": "video.stop"})
    require(
        stopped.get("ok") is True and stopped.get("generation") == generation
        and stopped.get("reason") == "stopped",
        "video.stop did not report a stopped stream of this generation",
    )
    end_deadline = time.monotonic() + end_timeout
    while not check.ended:
        remaining = end_deadline - time.monotonic()
        require(remaining > 0 and drain(remaining), "no end packet after video.stop")
    require(
        check.config_packets >= 1 and check.media_packets >= 1 and check.key_frames >= 1,
        "stream lacks a config packet or a key frame",
    )
    require(
        stopped.get("packets") == check.config_packets + check.media_packets
        and stopped.get("bytes") == check.bytes
        and stopped.get("key_frames") == check.key_frames,
        "video.stop counters differ from the packets received",
    )
    statistics = check.statistics()
    statistics.update({
        "codec": response.get("codec"), "pings": len(pings),
        "ping_max_ms": round(max(pings) * 1000, 1) if pings else None,
    })
    return statistics, accepted, bytes(check.data) if keep else None


def decode(stream: bytes, accepted: dict) -> dict:
    """Decode the stream with ffprobe from a private directory that is always removed."""
    scratch = Path(tempfile.mkdtemp(prefix="carlo-stream-"))
    try:
        path = scratch / "stream.h264"
        path.write_bytes(stream)
        probe = subprocess.run(
            ["ffprobe", "-v", "error", "-count_frames", "-select_streams", "v:0",
             "-show_entries", "stream=codec_name,width,height,nb_read_frames", "-of", "json",
             str(path)],
            capture_output=True, text=True, timeout=60, check=True,
        )
        facts = json.loads(probe.stdout)["streams"][0]
        frames = int(facts["nb_read_frames"])
        require(
            facts["codec_name"] == "h264"
            and (facts["width"], facts["height"]) == (accepted["width"], accepted["height"])
            and frames >= 1,
            "host decoder disagrees with codec, size or frame count",
        )
        return {"codec_name": "h264", "width": facts["width"], "height": facts["height"],
                "frames": frames}
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
        if scratch.exists():
            raise SessionError("captured stream directory was not removed")


def hello(control: ControlClient, session: str, api, abi) -> None:
    response = first_call(control, {"command": "hello", "protocol": VERSION, "session": session})
    validate_hello(response, api, abi)


def channel_state(control: ControlClient) -> str:
    response = control.call({"command": "channels"})
    require(response.get("ok") is True, "channels failed")
    return response.get("video")


def shutdown(control: ControlClient) -> None:
    require(control.call({"command": "shutdown"}) == {"ok": True}, "shutdown failed")
    require(control.receive() is None, "control stayed open after shutdown")


def exercise_stream(control, open_video, session, api, abi, *, seconds=STREAM_SECONDS,
                    decoder=decode, **bounds) -> dict:
    hello(control, session, api, abi)
    require(channel_state(control) == "pending", "video channel is not pending after hello")
    video = open_video()
    require(
        video.attach(session) == {"ok": True, "channel": "video"},
        "video channel was not attached to the session",
    )
    require(channel_state(control) == "attached", "channels does not report the attachment")
    reader = PacketReader(video)
    reader.start()
    first, accepted, stream = stream_cycle(
        control, reader, 1, seconds=seconds, keep=True, **bounds)
    try:
        decoded = decoder(stream, accepted)
    finally:
        del stream
    require(channel_state(control) == "attached", "video channel did not survive video.stop")
    second, _, _ = stream_cycle(
        control, reader, 2, seconds=max(seconds / 3, 0.2), keep=False, **bounds)
    require(
        error_code(control.call({"command": "video.stop"})) == "not_streaming",
        "video.stop without a stream was not refused as not_streaming",
    )
    shutdown(control)
    require(reader.take(10) is None, "video channel stayed open after shutdown")
    reader.join(timeout=10)
    return {"accepted": accepted, "generations": [first, second], "host_decode": decoded,
            "stream_deleted": True}


def exercise_no_video_channel(control, _open_video, session, api, abi, **_) -> dict:
    hello(control, session, api, abi)
    state = channel_state(control)
    require(state in ("pending", "absent"), "a video channel exists without a connection")
    refused = error_code(control.call({"command": "video.start"}))
    require(refused == "video_channel_absent", "video.start was not refused without a channel")
    require(control.call({"command": "ping"}) == {"ok": True}, "ping failed")
    shutdown(control)
    return {"video_start": refused}


def exercise_wrong_session_attach(control, open_video, session, api, abi, **_) -> dict:
    hello(control, session, api, abi)
    wrong = "0" * 32 if session != "0" * 32 else "1" * 32
    video = open_video()
    answer = video.attach(wrong)
    require(
        answer == {"ok": False, "error": {"code": "invalid_session"}},
        "wrong session identifier was not refused on the video channel",
    )
    require(video.receive() is None, "refused video connection stayed open")
    require(control.call({"command": "ping"}) == {"ok": True}, "control did not survive")
    require(channel_state(control) == "pending", "a refused attachment changed the channel")
    refused = error_code(control.call({"command": "video.start"}))
    require(refused == "video_channel_absent", "video.start was not refused without a channel")
    shutdown(control)
    return {"rejected": "invalid_session", "control_survived": True, "video_start": refused}


CASES = {
    "stream": exercise_stream,
    "no_video_channel": exercise_no_video_channel,
    "wrong_session_attach": exercise_wrong_session_attach,
}


def serve_case(adb: Adb, binary: Path, device: dict, name: str, **options) -> dict:
    """Run one case in a fresh session; nothing of it may be left on the device."""
    session = ServeSession(adb, binary)
    result = {"case": name, "forward_removed": None}
    try:
        ready = session.start()
        result.update(session.evidence)
        control = session.track(ControlClient(session.connect()))

        def open_video() -> VideoChannel:
            return session.track(VideoChannel(session.connect()))

        result.update(CASES[name](
            control, open_video, ready["session"], device["android_api"], device["abi"],
            **options))
        result["exit_code"], stderr = session.finish()
        result["stderr_bytes"] = len(stderr)
        require(result["exit_code"] == 0, "agent did not exit 0 after shutdown")
        result["socket_present_after_exit"] = session.socket_exists()
        require(not result["socket_present_after_exit"], "socket path survived the agent")
    finally:
        try:
            session.close()
        finally:
            result["forward_removed"] = session.forward_removed
    require(result["forward_removed"] is True, "adb forward was not removed")
    listed = adb.run(["forward", "--list"]).stdout
    result["forward_listed_after"] = f"tcp:{session.port} " in listed
    result["directory_present_after"] = (
        adb.shell(["test", "-e", session.directory], check=False).returncode == 0
    )
    require(
        not result["forward_listed_after"] and not result["directory_present_after"],
        "a forward or the session directory was left behind",
    )
    # `cleanup` already failed the run if a session process survived.
    result["cleanup_removed"] = True
    return result


def validate_summary(summary: dict) -> dict:
    """The evidence a passing run must contain; counters only, no identifiers."""
    listed = summary.get("cases", [])
    cases = {case["case"]: case for case in listed}
    require(
        set(cases) == set(CASES) and len(listed) == len(CASES),
        "summary does not cover every case exactly once",
    )
    for name, case in cases.items():
        require(
            case.get("session_id_length") == 32
            and case.get("transport") in ("filesystem", "abstract")
            and case.get("exit_code") == 0
            and case.get("forward_removed") is True
            and case.get("forward_listed_after") is False
            and case.get("directory_present_after") is False
            and case.get("socket_present_after_exit") is False
            and case.get("cleanup_removed") is True,
            f"case {name} lacks lifecycle evidence",
        )
    stream = cases["stream"]
    generations = stream.get("generations", [])
    require(
        [entry.get("generation") for entry in generations] == [1, 2]
        and all(
            entry.get("config_packets", 0) >= 1 and entry.get("key_frames", 0) >= 1
            and entry.get("media_packets", 0) >= 1 and entry.get("end_packet") is True
            and entry.get("pings", 0) >= 1
            and entry.get("ping_max_ms", PING_BOUND * 1000 + 1) <= PING_BOUND * 1000
            for entry in generations
        ),
        "stream case lacks two complete generations with a responsive control channel",
    )
    decoded = stream.get("host_decode") or {}
    accepted = stream.get("accepted") or {}
    require(
        decoded.get("codec_name") == "h264" and decoded.get("frames", 0) >= 1
        and (decoded.get("width"), decoded.get("height"))
        == (accepted.get("width"), accepted.get("height"))
        and stream.get("stream_deleted") is True,
        "stream case lacks a host decode of the accepted size",
    )
    require(
        cases["no_video_channel"].get("video_start") == "video_channel_absent"
        and cases["wrong_session_attach"].get("rejected") == "invalid_session"
        and cases["wrong_session_attach"].get("control_survived") is True,
        "channel binding refusals were not proven",
    )
    require(
        re.search(r"[0-9a-f]{32}", json.dumps(summary)) is None,
        "summary contains an identifier",
    )
    return summary


def run(adb: Adb, manifest: Path, allow_physical: bool, **options) -> dict:
    binary, device = resolve_artifact(adb, manifest, allow_physical)
    summary = {
        "emulator": device["emulator"], "android_api": device["android_api"],
        "abi": device["abi"], "source": "display_mirror", "screen_capture": True,
        "ui_contents_recorded": False, "cases": [],
    }
    for name in CASES:
        summary["cases"].append(serve_case(adb, binary, device, name, **options))
    return validate_summary(summary)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--manifest", type=Path, default=ROOT / "dist/manifest.json")
    parser.add_argument("--output", type=Path, default=ROOT / "results/video-stream")
    parser.add_argument("--seconds", type=float, default=STREAM_SECONDS,
                        help="how long the first generation streams (default 3)")
    parser.add_argument("--allow-physical", action="store_true",
                        help="the owner authorized this physical device for this run")
    args = parser.parse_args()
    if not 0.5 <= args.seconds <= 60:
        parser.error("--seconds must be within 0.5 and 60")
    if shutil.which("ffprobe") is None:
        parser.error("host decode check requires ffprobe")
    manifest = args.manifest.resolve()
    summary = run(Adb(args.serial), manifest, args.allow_physical, seconds=args.seconds)
    summary["agent_manifest"] = json.loads(manifest.read_text())
    args.output.mkdir(parents=True, exist_ok=True)
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    stream = summary["cases"][0]
    print(f"Two video generations streamed over the video channel "
          f"({stream['host_decode']['frames']} frames decoded on the host) while control "
          "answered every ping; sessions cleaned up.")


if __name__ == "__main__":
    try:
        main()
    except (SessionError, ExposureError, ProtocolError, subprocess.SubprocessError, OSError,
            ValueError, KeyError) as error:
        raise SystemExit(f"carlo-video-stream-smoke: {error}") from error
