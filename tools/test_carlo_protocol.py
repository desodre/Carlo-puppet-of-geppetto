"""Host framing codec against the vectors shared with the Rust agent."""

import io
import json
import socket
import unittest
from pathlib import Path

from carlo_protocol import (
    CONTROL, END, HIERARCHY, MEDIA, VIDEO, ControlClient, Frame, ProtocolError, VideoChannel,
    VideoPacket,
    decode_video_packet, encode, encode_video_packet, read_frame,
)

FIXTURES = Path(__file__).resolve().parent.parent / "tests/fixtures/protocol_frames.json"
PACKETS = FIXTURES.with_name("video_packets.json")


class Trickle(io.RawIOBase):
    """Returns at most one byte per read and counts what was consumed."""

    def __init__(self, data: bytes):
        self.data, self.consumed = data, 0

    def read(self, size=-1):
        chunk = self.data[self.consumed : self.consumed + 1]
        self.consumed += len(chunk)
        return chunk


class ProtocolTests(unittest.TestCase):
    def test_shared_vectors_decode_fragmented_and_reencode_identically(self):
        frames = json.loads(FIXTURES.read_text(encoding="utf-8"))["frames"]
        self.assertGreaterEqual(len(frames), 12)
        for case in frames:
            with self.subTest(case["name"]):
                data = bytes.fromhex(case["hex"])
                if "error" in case:
                    with self.assertRaises(ProtocolError) as raised:
                        read_frame(Trickle(data))
                    self.assertEqual(raised.exception.code, case["error"])
                    self.assertEqual(raised.exception.recoverable, case["recoverable"])
                    if case["recoverable"]:
                        self.assertEqual(
                            raised.exception.request_id, case["rejection"]["request_id"]
                        )
                else:
                    frame = read_frame(Trickle(data))
                    expected = Frame(
                        case["kind"], case["request_id"], bytes.fromhex(case["payload_hex"])
                    )
                    self.assertEqual(frame, expected)
                    self.assertEqual(encode(*frame), data)

    def test_oversized_length_reads_only_the_header(self):
        stream = Trickle(encode(VIDEO, 1, b"")[:12] + b"\xff\xff\xff\xff" + b"unread")
        with self.assertRaises(ProtocolError) as raised:
            read_frame(stream)
        self.assertEqual(raised.exception.code, "frame_too_large")
        self.assertEqual(stream.consumed, 16)
        with self.assertRaises(ProtocolError):
            encode(CONTROL, 1, bytes(64 * 1024 + 1))

    def test_unknown_kind_is_skipped_and_clean_eof_is_none(self):
        skipped = bytearray(encode(CONTROL, 5, b"later"))
        skipped[5] = 200
        stream = io.BytesIO(bytes(skipped) + encode(VIDEO, 8, b"\x00\x00\x01"))
        with self.assertRaises(ProtocolError) as raised:
            read_frame(stream)
        self.assertTrue(raised.exception.recoverable)
        self.assertEqual(read_frame(stream), Frame(VIDEO, 8, b"\x00\x00\x01"))
        self.assertIsNone(read_frame(stream))

    def test_shared_video_packet_vectors_decode_and_reencode_identically(self):
        packets = json.loads(PACKETS.read_text(encoding="utf-8"))["packets"]
        self.assertGreaterEqual(len(packets), 12)
        for case in packets:
            with self.subTest(case["name"]):
                payload = bytes.fromhex(case["payload_hex"])
                if "error" in case:
                    with self.assertRaises(ProtocolError) as raised:
                        decode_video_packet(payload)
                    self.assertEqual(raised.exception.code, case["error"])
                    continue
                packet = decode_video_packet(payload)
                self.assertEqual(packet, VideoPacket(
                    case["type"], case["key_frame"], case["generation"], case["pts_us"],
                    case["width"], case["height"], bytes.fromhex(case["data_hex"])))
                self.assertEqual(encode_video_packet(packet), payload)

    def test_video_channel_attaches_then_only_reads_packets(self):
        ours, theirs = socket.socketpair()
        channel = VideoChannel(ours, timeout=2)
        media = VideoPacket(MEDIA, True, 3, 40, 360, 640, b"\x00\x00\x01\x65")
        end = VideoPacket(END, False, 3, 0, 360, 640, b"")
        theirs.sendall(encode(CONTROL, 9, b'{"ok":true,"channel":"video"}')
                       + encode(VIDEO, 21, encode_video_packet(media))
                       + encode(VIDEO, 21, encode_video_packet(end))
                       + encode(CONTROL, 0, b"{}"))
        self.assertEqual(channel.attach("ab" * 16, request_id=9),
                         {"ok": True, "channel": "video"})
        sent = read_frame(theirs.makefile("rb"))
        self.assertEqual((sent.kind, sent.request_id), (CONTROL, 9))
        self.assertEqual(json.loads(sent.payload),
                         {"command": "attach", "channel": "video", "session": "ab" * 16})
        self.assertEqual(channel.receive(), (21, media))
        self.assertEqual(channel.receive(), (21, end))
        with self.assertRaises(ProtocolError) as raised:
            channel.receive()
        self.assertEqual(raised.exception.code, "unexpected_kind")
        theirs.close()
        self.assertIsNone(channel.receive())
        channel.close()
        # A refused attach is the agent closing, or answering with another ID.
        ours, theirs = socket.socketpair()
        channel = VideoChannel(ours, timeout=2)
        theirs.shutdown(socket.SHUT_WR)
        self.assertIsNone(channel.attach("ab" * 16))
        channel.close()
        theirs.close()
        ours, theirs = socket.socketpair()
        channel = VideoChannel(ours, timeout=2)
        theirs.sendall(encode(CONTROL, 2, b"{}"))
        with self.assertRaises(ProtocolError):
            channel.attach("ab" * 16, request_id=1)
        channel.close()
        theirs.close()

    def test_hierarchy_dump_returns_raw_xml_or_raises_the_stable_code(self):
        ours, theirs = socket.socketpair()
        client = ControlClient(ours, timeout=2)
        reader = theirs.makefile("rb")
        xml = '<?xml version="1.0"?><hierarchy><node text="café"/></hierarchy>'
        theirs.sendall(encode(HIERARCHY, 1, xml.encode()))
        self.assertEqual(client.hierarchy_dump(), xml.encode())
        sent = read_frame(reader)
        self.assertEqual((sent.kind, sent.request_id), (CONTROL, 1))
        self.assertEqual(json.loads(sent.payload), {"command": "hierarchy.dump"})
        # A control answer is the agent's typed failure; the connection stays usable.
        for request_id, code in enumerate(("unavailable", "conflict", "timeout"), start=2):
            body = {"ok": False, "error": {"code": code}}
            theirs.sendall(encode(CONTROL, request_id, json.dumps(body).encode()))
            with self.assertRaises(ProtocolError) as raised:
                client.hierarchy_dump()
            self.assertEqual(raised.exception.code, code)
            self.assertTrue(raised.exception.recoverable)
            self.assertEqual(raised.exception.request_id, request_id)
            self.assertEqual(read_frame(reader).request_id, request_id)
        # Anything else is a contract violation, never returned as a document.
        for frame in (
            encode(CONTROL, 5, b'{"ok":true}'),
            encode(CONTROL, 6, b"[1]"),
            encode(CONTROL, 7, b"not json"),
            encode(HIERARCHY, 99, b"<hierarchy/>"),
            encode(HIERARCHY, 9, b"<a>\xff</a>"),
            encode(VIDEO, 10, b""),
        ):
            theirs.sendall(frame)
            with self.assertRaises(ProtocolError) as raised:
                client.hierarchy_dump()
            self.assertIn(raised.exception.code, ("invalid_response", "unexpected_kind"))
            self.assertNotIn("<", str(raised.exception))
        theirs.shutdown(socket.SHUT_WR)
        with self.assertRaises(ProtocolError) as raised:
            client.hierarchy_dump()
        self.assertEqual(raised.exception.code, "closed")
        reader.close()
        theirs.close()
        client.close()


if __name__ == "__main__":
    unittest.main()
