"""Host framing codec against the vectors shared with the Rust agent."""

import io
import json
import unittest
from pathlib import Path

from carlo_protocol import CONTROL, VIDEO, Frame, ProtocolError, encode, read_frame

FIXTURES = Path(__file__).resolve().parent.parent / "tests/fixtures/protocol_frames.json"


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


if __name__ == "__main__":
    unittest.main()
