"""Host codec for Carlo protocol v1 framing; mirrors src/protocol.rs.

Header (16 bytes, big-endian): magic `CRLO`, version, kind, two reserved zero
bytes, request ID (u32) and payload length (u32). The header is validated before
any payload byte is read.
"""

import struct
from typing import BinaryIO, NamedTuple

MAGIC = b"CRLO"
VERSION = 1
HEADER = struct.Struct(">4sBBHII")
CONTROL, HIERARCHY, VIDEO = 1, 2, 3
BULK_LIMIT = 8 * 1024 * 1024
LIMITS = {CONTROL: 64 * 1024, HIERARCHY: BULK_LIMIT, VIDEO: BULK_LIMIT}


class Frame(NamedTuple):
    kind: int
    request_id: int
    payload: bytes


class ProtocolError(Exception):
    """Framing failure; only a recoverable one leaves the stream usable."""

    def __init__(self, code: str, detail: str, *, recoverable: bool = False, request_id: int = 0):
        super().__init__(f"{code}: {detail}")
        self.code, self.recoverable, self.request_id = code, recoverable, request_id


def encode(kind: int, request_id: int, payload: bytes) -> bytes:
    limit = LIMITS[kind]
    if len(payload) > limit:
        raise ProtocolError("frame_too_large", f"{len(payload)} bytes, limit is {limit}")
    return HEADER.pack(MAGIC, VERSION, kind, 0, request_id, len(payload)) + payload


def _read(stream: BinaryIO, size: int) -> bytes:
    chunks, received = [], 0
    while received < size:
        chunk = stream.read(size - received)
        if not chunk:
            break
        chunks.append(chunk)
        received += len(chunk)
    return b"".join(chunks)


def _body(stream: BinaryIO, size: int) -> bytes:
    data = _read(stream, size)
    if len(data) < size:
        raise ProtocolError("truncated_frame", f"stream ended after {len(data)} of {size} bytes")
    return data


def read_frame(stream: BinaryIO) -> Frame | None:
    """Read one frame from a blocking stream; None on EOF at a frame boundary."""
    header = _read(stream, HEADER.size)
    if not header:
        return None
    if len(header) < HEADER.size:
        raise ProtocolError(
            "truncated_frame", f"stream ended after {len(header)} of {HEADER.size} bytes"
        )
    magic, version, kind, reserved, request_id, length = HEADER.unpack(header)
    if magic != MAGIC:
        raise ProtocolError("bad_magic", "frame magic mismatch")
    if version != VERSION:
        raise ProtocolError("unsupported_version", f"version {version}")
    if reserved:
        raise ProtocolError("reserved_bits", "reserved header bytes are not zero")
    limit = LIMITS.get(kind, BULK_LIMIT)
    if length > limit:
        raise ProtocolError("frame_too_large", f"{length} bytes, limit is {limit}")
    payload = _body(stream, length)
    if kind not in LIMITS:
        raise ProtocolError(
            "unknown_kind", f"kind {kind}", recoverable=True, request_id=request_id
        )
    return Frame(kind, request_id, payload)
