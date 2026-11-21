"""Video stream smoke: packet checks, a scripted agent, mocks and the host binary.

No device is used. The scripted agent speaks the documented protocol over socket
pairs with synthetic bytes, so the whole stream exercise runs on the host; the
host-binary tests (only when `target/debug/carlo` exists) cover what the real
agent can do off the pinned device: attachment, refusals and `unavailable`.
"""

import copy
import json
import os
import socket
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch

import serve_session
import smoke_video_stream
from android_session import ROOT, SessionError
from carlo_protocol import (
    CONFIG, CONTROL, END, MEDIA, VIDEO, ControlClient, VideoChannel, VideoPacket, encode,
    encode_video_packet, read_frame,
)
from smoke_video_stream import (
    CASES, PacketReader, StreamCheck, decode, exercise_no_video_channel, exercise_stream,
    exercise_wrong_session_attach, serve_case, validate_start, validate_summary,
)
from serve_session import parse_ready, read_line
from test_smoke_serve import SESSION, FakeAdb, hello_response

HOST_BINARY = ROOT / "target/debug/carlo"
ACCEPTED = {"width": 360, "height": 640, "fps": 15, "bitrate": 1_500_000,
            "key_frame_interval_seconds": 1}
SPS_PPS = bytes.fromhex("0000000167420000000168ce")
FAST = {"seconds": 0.3, "end_timeout": 1.0}


def packet(kind, generation=1, pts=0, key=False, data=b"\x00\x00\x01\x41", size=(360, 640)):
    data = b"" if kind == END else data
    return VideoPacket(kind, key, generation, pts if kind == MEDIA else 0, *size, data)


class ScriptedAgent:
    """A minimal agent for both channels. `fault` selects one contract violation."""

    def __init__(self, fault: str | None = None, ping_delay: float = 0.0):
        self.fault, self.ping_delay = fault, ping_delay
        self.control, control_peer = socket.socketpair()
        self.video, video_peer = socket.socketpair()
        self.control_peer, self.video_peer = control_peer, video_peer
        self.attached, self.generation, self.stop = False, 0, threading.Event()
        self.sender, self.counters = None, [0, 0, 0]
        self.write_lock = threading.Lock()
        self.threads = [threading.Thread(target=self.serve_control, daemon=True),
                        threading.Thread(target=self.serve_attach, daemon=True)]
        for thread in self.threads:
            thread.start()

    def open_video(self) -> VideoChannel:
        return VideoChannel(self.video_peer, timeout=5)

    def close(self):
        self.stop.set()
        for connection in (self.control, self.video, self.control_peer, self.video_peer):
            try:
                connection.close()
            except OSError:
                pass

    def serve_attach(self):
        try:
            frame = read_frame(self.video.makefile("rb"))
        except (OSError, ValueError):
            return
        if frame is None:
            return
        request = json.loads(frame.payload)
        if request.get("session") != SESSION:
            reply = {"ok": False, "error": {"code": "invalid_session"}}
            self.video.sendall(encode(CONTROL, frame.request_id, json.dumps(reply).encode()))
            self.video.shutdown(socket.SHUT_RDWR)
            return
        self.attached = True
        with self.write_lock:
            self.video.sendall(encode(CONTROL, frame.request_id, b'{"ok":true,"channel":"video"}'))

    def send(self, request_id, item: VideoPacket, count=True):
        with self.write_lock:
            self.video.sendall(encode(VIDEO, request_id, encode_video_packet(item)))
        if count and item.type != END:
            self.counters[0] += 1
            self.counters[1] += len(item.data)
            self.counters[2] += item.key_frame

    def stream(self, request_id, generation):
        try:
            self.script(request_id, generation)
        except OSError:
            pass  # the test closed the sockets after a deliberate failure

    def script(self, request_id, generation):
        fault = self.fault if generation == 1 else None
        size = (352, 640) if fault == "size" else (360, 640)
        wire_generation = generation + 5 if fault == "generation" else generation
        if fault != "no_config":
            self.send(request_id, packet(CONFIG, wire_generation, data=SPS_PPS, size=size))
        index = 0
        while not self.stop.is_set():
            key = index == 0 and fault != "no_key"
            pts = 1000 - index if fault == "pts" else index * 1000
            data = b"\x00\x00\x00\x01" + (b"\x65" if key else b"\x41") + bytes(20)
            identifier = request_id + 1 if fault == "request_id" else request_id
            self.send(identifier, packet(MEDIA, wire_generation, pts, key, data, size))
            index += 1
            time.sleep(0.01)
        if fault != "no_end":
            self.send(request_id, packet(END, wire_generation, size=size))
        if fault == "after_end":
            self.send(request_id, packet(MEDIA, wire_generation, 9_000_000, False), count=False)

    def respond(self, frame) -> dict | None:
        request = json.loads(frame.payload)
        command = request.get("command")
        failure = lambda code: {"ok": False, "error": {"code": code}}  # noqa: E731
        if command == "hello":
            return hello_response(android_api=None, abi=None)
        if command == "ping":
            time.sleep(self.ping_delay)
            return {"ok": True}
        if command == "channels":
            return {"ok": True, "video": "attached" if self.attached else "pending"}
        if command == "video.start":
            if not self.attached:
                return failure("video_channel_absent")
            if self.sender is not None:
                return failure("already_streaming")
            if self.fault == "unavailable":
                return {"ok": False,
                        "error": {"code": "unavailable", "reason": "unsupported_profile"}}
            self.generation += 1
            self.stop.clear()
            self.counters = [0, 0, 0]
            self.sender = threading.Thread(
                target=self.stream, args=(frame.request_id, self.generation), daemon=True)
            self.sender.start()
            return {"ok": True, "generation": self.generation, "display": 0,
                    "codec": "scripted.avc", "accepted": dict(ACCEPTED)}
        if command == "video.stop":
            if self.sender is None:
                return failure("not_streaming")
            self.stop.set()
            self.sender.join()
            self.sender = None
            packets, size, keys = self.counters
            if self.fault == "counters" and self.generation == 1:
                packets += 1
            return {"ok": True, "generation": self.generation, "reason": "stopped",
                    "packets": packets, "bytes": size, "key_frames": keys}
        if command == "shutdown":
            return None
        return failure("unknown_command")

    def serve_control(self):
        reader = self.control.makefile("rb")
        try:
            while (frame := read_frame(reader)) is not None:
                reply = self.respond(frame)
                body = {"ok": True} if reply is None else reply
                self.control.sendall(encode(CONTROL, frame.request_id, json.dumps(body).encode()))
                if reply is None:
                    self.control.shutdown(socket.SHUT_RDWR)
                    self.video.shutdown(socket.SHUT_RDWR)
                    return
        except (OSError, ValueError):
            pass


def fake_decoder(stream: bytes, accepted: dict) -> dict:
    assert stream.startswith(SPS_PPS) and b"\x00\x00\x00\x01\x65" in stream
    return {"codec_name": "h264", "width": accepted["width"], "height": accepted["height"],
            "frames": 3}


class ScriptedAgentTests(unittest.TestCase):
    def run_stream(self, fault=None, **options):
        agent = ScriptedAgent(fault, **{key: options.pop(key) for key in ["ping_delay"]
                                        if key in options})
        self.addCleanup(agent.close)
        control = ControlClient(agent.control_peer, timeout=5)
        settings = {**FAST, "decoder": fake_decoder, **options}
        return exercise_stream(control, agent.open_video, SESSION, None, None, **settings)

    def test_two_generations_are_streamed_checked_and_decoded(self):
        result = self.run_stream()
        self.assertEqual(result["accepted"], ACCEPTED)
        first, second = result["generations"]
        self.assertEqual((first["generation"], second["generation"]), (1, 2))
        for entry in (first, second):
            self.assertEqual(entry["config_packets"], 1)
            self.assertEqual(entry["key_frames"], 1)
            self.assertGreaterEqual(entry["media_packets"], 2)
            self.assertTrue(entry["end_packet"])
            self.assertGreaterEqual(entry["pings"], 1)
            self.assertLessEqual(entry["first_pts_us"], entry["last_pts_us"])
        self.assertEqual(result["host_decode"]["frames"], 3)
        self.assertNotIn(SESSION, json.dumps(result))

    def test_every_contract_violation_fails_the_exercise(self):
        for fault, message in [
            ("no_config", "before the config packet"),
            ("no_key", "not a key frame"),
            ("pts", "PTS went backwards"),
            ("generation", "another stream generation"),
            ("size", "dimensions differ"),
            ("request_id", "video.start ID"),
            ("no_end", "no end packet"),
            ("counters", "counters differ"),
            ("unavailable", "not the pinned mirror profile"),
        ]:
            with self.subTest(fault):
                with self.assertRaisesRegex(SessionError, message):
                    self.run_stream(fault)

    def test_slow_control_answers_fail_the_independence_check(self):
        with self.assertRaisesRegex(SessionError, "slow ping"):
            self.run_stream(ping_delay=0.2, ping_bound=0.05)

    def test_failed_host_decode_fails_the_exercise(self):
        def failing(stream, accepted):
            raise SessionError("host decoder disagrees with codec, size or frame count")

        with self.assertRaisesRegex(SessionError, "host decoder disagrees"):
            self.run_stream(decoder=failing)

    def test_binding_cases_against_the_scripted_agent(self):
        agent = ScriptedAgent()
        self.addCleanup(agent.close)
        control = ControlClient(agent.control_peer, timeout=5)
        result = exercise_no_video_channel(control, agent.open_video, SESSION, None, None)
        self.assertEqual(result, {"video_start": "video_channel_absent"})
        agent = ScriptedAgent()
        self.addCleanup(agent.close)
        control = ControlClient(agent.control_peer, timeout=5)
        result = exercise_wrong_session_attach(control, agent.open_video, SESSION, None, None)
        self.assertEqual(result["rejected"], "invalid_session")
        self.assertTrue(result["control_survived"])


class StreamCheckTests(unittest.TestCase):
    def check(self, keep=False):
        return StreamCheck(1, 9, ACCEPTED, keep)

    def test_order_generation_size_and_pts_are_enforced(self):
        check = self.check(keep=True)
        check.feed(9, packet(CONFIG, data=SPS_PPS))
        check.feed(9, packet(MEDIA, pts=5, key=True, data=b"\x00\x00\x01\x65"))
        check.feed(9, packet(MEDIA, pts=5))
        check.feed(9, packet(CONFIG, data=SPS_PPS))
        check.feed(9, packet(MEDIA, pts=9))
        check.feed(9, packet(END))
        self.assertEqual(check.statistics(), {
            "generation": 1, "config_packets": 2, "media_packets": 3, "key_frames": 1,
            "bytes": 2 * len(SPS_PPS) + 12, "first_pts_us": 5, "last_pts_us": 9,
            "end_packet": True})
        self.assertEqual(len(check.data), check.bytes)
        with self.assertRaisesRegex(SessionError, "followed the end packet"):
            check.feed(9, packet(MEDIA, pts=10))
        for build, message in [
            (lambda c: c.feed(9, packet(MEDIA, key=True)), "before the config"),
            (lambda c: (c.feed(9, packet(CONFIG, data=SPS_PPS)), c.feed(9, packet(MEDIA))),
             "not a key frame"),
            (lambda c: c.feed(8, packet(CONFIG, data=SPS_PPS)), "video.start ID"),
            (lambda c: c.feed(9, packet(CONFIG, 2, data=SPS_PPS)), "another stream generation"),
            (lambda c: c.feed(9, packet(END, size=(640, 360))), "dimensions differ"),
            (lambda c: (c.feed(9, packet(CONFIG, data=SPS_PPS)),
                        c.feed(9, packet(MEDIA, pts=4, key=True)),
                        c.feed(9, packet(MEDIA, pts=3))), "PTS went backwards"),
        ]:
            with self.assertRaisesRegex(SessionError, message):
                build(self.check())
        # Without `keep` nothing of the stream is retained.
        discard = self.check()
        discard.feed(9, packet(CONFIG, data=SPS_PPS))
        self.assertIsNone(discard.data)
        with patch.object(smoke_video_stream, "MAX_KEPT_BYTES", 8):
            with self.assertRaisesRegex(SessionError, "exceeds its bound"):
                self.check(keep=True).feed(9, packet(CONFIG, data=SPS_PPS))

    def test_start_response_must_match_the_contract_and_the_request(self):
        request = smoke_video_stream.DEFAULT_REQUEST
        good = {"ok": True, "generation": 1, "display": 0, "codec": None,
                "accepted": dict(ACCEPTED)}
        self.assertEqual(validate_start(good, 1, request), ACCEPTED)
        for change in [{"generation": 2}, {"display": 1}, {"codec": 7}, {"accepted": None},
                       {"accepted": {**ACCEPTED, "width": 720}},
                       {"accepted": {**ACCEPTED, "fps": 30}},
                       {"accepted": {**ACCEPTED, "extra": 1}},
                       {"accepted": {**ACCEPTED, "bitrate": True}}]:
            with self.assertRaises(SessionError, msg=change):
                validate_start({**good, **change}, 1, request)
        with self.assertRaisesRegex(SessionError, "failed with permission_denied"):
            validate_start({"ok": False, "error": {"code": "permission_denied"}}, 1, request)

    def test_reader_reports_close_and_channel_failures(self):
        ours, theirs = socket.socketpair()
        reader = PacketReader(VideoChannel(ours, timeout=2))
        reader.start()
        self.assertIs(reader.take(None), smoke_video_stream.queue.Empty)
        theirs.sendall(encode(VIDEO, 3, encode_video_packet(packet(END))))
        self.assertEqual(reader.take(2), (3, packet(END)))
        theirs.sendall(encode(VIDEO, 3, b"\x09" + bytes(23)))
        with self.assertRaisesRegex(SessionError, "ProtocolError"):
            reader.take(2)
        reader.join(2)
        ours.close()
        theirs.close()


def summary():
    def case(name, **fields):
        return {"case": name, "session_id_length": 32, "transport": "abstract",
                "socket_mode": None, "exit_code": 0, "forward_removed": True,
                "forward_listed_after": False, "directory_present_after": False,
                "socket_present_after_exit": False, "cleanup_removed": True, **fields}

    def generation(number):
        return {"generation": number, "config_packets": 1, "media_packets": 40, "key_frames": 3,
                "bytes": 90000, "first_pts_us": 1, "last_pts_us": 2_900_000, "end_packet": True,
                "codec": "c2.vendor.avc.encoder", "pings": 28, "ping_max_ms": 3.5}

    return {"emulator": False, "android_api": 36, "abi": "arm64-v8a", "cases": [
        case("stream", accepted=dict(ACCEPTED), generations=[generation(1), generation(2)],
             host_decode={"codec_name": "h264", "width": 360, "height": 640, "frames": 40},
             stream_deleted=True),
        case("no_video_channel", video_start="video_channel_absent"),
        case("wrong_session_attach", rejected="invalid_session", control_survived=True,
             video_start="video_channel_absent"),
    ]}


class SummaryTests(unittest.TestCase):
    def test_summary_requires_every_proof_and_carries_no_identifier(self):
        self.assertEqual(len(validate_summary(summary())["cases"]), len(CASES))
        for index, change in [
            (0, {"exit_code": 74}), (1, {"forward_removed": False}),
            (2, {"forward_listed_after": True}), (0, {"directory_present_after": True}),
            (1, {"socket_present_after_exit": True}), (2, {"cleanup_removed": False}),
            (0, {"session_id_length": 31}), (0, {"transport": "tcp"}),
            (0, {"stream_deleted": False}), (0, {"host_decode": None}),
            (0, {"host_decode": {"codec_name": "h264", "width": 640, "height": 360,
                                 "frames": 9}}),
            (0, {"host_decode": {"codec_name": "hevc", "width": 360, "height": 640,
                                 "frames": 9}}),
            (0, {"host_decode": {"codec_name": "h264", "width": 360, "height": 640,
                                 "frames": 0}}),
            (1, {"video_start": "unavailable"}), (2, {"rejected": None}),
            (2, {"control_survived": False}), (1, {"note": SESSION}),
        ]:
            broken = summary()
            broken["cases"][index].update(change)
            with self.assertRaises(SessionError, msg=change):
                validate_summary(broken)
        for change in [{"generation": 3}, {"config_packets": 0}, {"key_frames": 0},
                       {"media_packets": 0}, {"end_packet": False}, {"pings": 0},
                       {"ping_max_ms": 1000.5}]:
            for which in (0, 1):
                broken = summary()
                broken["cases"][0]["generations"][which].update(change)
                with self.assertRaises(SessionError, msg=change):
                    validate_summary(broken)
        for mutate in [lambda s: s["cases"].pop(),
                       lambda s: s["cases"].append(copy.deepcopy(s["cases"][1])),
                       lambda s: s["cases"][0]["generations"].pop()]:
            broken = summary()
            mutate(broken)
            with self.assertRaises(SessionError):
                validate_summary(broken)


class DecodeTests(unittest.TestCase):
    def run_decode(self, stdout=None, error=None):
        seen = {}

        def ffprobe(arguments, **kwargs):
            path = Path(arguments[-1])
            seen["path"], seen["bytes"] = path, path.read_bytes()
            seen["mode"] = path.parent.stat().st_mode & 0o777
            if error is not None:
                raise error
            return subprocess.CompletedProcess(arguments, 0, stdout, "")

        with patch.object(smoke_video_stream.subprocess, "run", side_effect=ffprobe):
            try:
                return decode(b"STREAM", ACCEPTED), seen
            finally:
                self.assertFalse(seen["path"].parent.exists(), "stream outlived the decode")
                self.assertEqual((seen["bytes"], seen["mode"]), (b"STREAM", 0o700))

    def test_stream_is_probed_from_a_private_directory_that_is_always_removed(self):
        facts = {"codec_name": "h264", "width": 360, "height": 640, "nb_read_frames": "7"}
        result, _ = self.run_decode(json.dumps({"streams": [facts]}))
        self.assertEqual(result, {"codec_name": "h264", "width": 360, "height": 640,
                                  "frames": 7})
        for change in [{"codec_name": "hevc"}, {"width": 640}, {"nb_read_frames": "0"}]:
            with self.assertRaises(SessionError, msg=change):
                self.run_decode(json.dumps({"streams": [{**facts, **change}]}))
        with self.assertRaises(subprocess.CalledProcessError):
            self.run_decode(error=subprocess.CalledProcessError(1, "ffprobe"))
        with self.assertRaises((KeyError, IndexError)):
            self.run_decode(json.dumps({"streams": []}))


class CleanupTests(unittest.TestCase):
    def test_unreachable_forward_fails_and_everything_is_released(self):
        adb = FakeAdb()
        binary = Path(tempfile.gettempdir()) / "carlo-not-pushed"
        device = {"android_api": 34, "abi": "x86_64"}
        with patch.object(serve_session.socket, "create_connection",
                          side_effect=ConnectionRefusedError(111, "refused")):
            for name in CASES:
                with self.assertRaises(serve_session.ExposureError) as raised:
                    serve_case(adb, binary, device, name)
                self.assertNotIn(SESSION, str(raised.exception))
                self.assertTrue(adb.forward_removed() and adb.cleaned() and adb.process.killed)
                self.assertTrue(adb.pipes_closed())
        self.assertEqual(sum(call[:2] == ["forward", "tcp:0"] for call in adb.calls), len(CASES))


@unittest.skipUnless(HOST_BINARY.is_file(), "host binary not built (cargo build)")
class HostBinaryTests(unittest.TestCase):
    """The smoke's exercises against the real agent over a Unix socket, without ADB."""

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

        def connect():
            connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            connection.connect(ready["socket"])
            return connection

        def open_video():
            channel = VideoChannel(connect())
            self.addCleanup(channel.close)
            return channel

        control = ControlClient(connect())
        self.addCleanup(control.close)
        return process, control, open_video, ready

    def finished(self, process, ready):
        self.assertEqual(process.wait(10), 0)
        self.assertEqual(process.stderr.read(), b"")
        self.assertFalse(os.path.exists(ready["socket"]))

    def test_video_start_without_a_channel_is_refused(self):
        process, control, open_video, ready = self.start()
        result = exercise_no_video_channel(control, open_video, ready["session"], None, None)
        self.assertEqual(result["video_start"], "video_channel_absent")
        self.finished(process, ready)

    def test_wrong_session_attach_is_refused_and_control_survives(self):
        process, control, open_video, ready = self.start()
        result = exercise_wrong_session_attach(
            control, open_video, ready["session"], None, None)
        self.assertEqual(result["rejected"], "invalid_session")
        self.finished(process, ready)

    def test_attached_stream_is_unavailable_off_the_pinned_profile(self):
        process, control, open_video, ready = self.start()
        with self.assertRaisesRegex(SessionError, "not the pinned mirror profile"):
            exercise_stream(control, open_video, ready["session"], None, None, **FAST)
        # The attachment itself worked and closed the listener; control goes on.
        self.assertFalse(os.path.exists(ready["socket"]))
        self.assertEqual(control.call({"command": "channels"}), {"ok": True, "video": "attached"})
        self.assertEqual(control.call({"command": "shutdown"}), {"ok": True})
        self.finished(process, ready)


if __name__ == "__main__":
    unittest.main()
