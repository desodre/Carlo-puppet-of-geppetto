"""Host codec and channel clients for Carlo protocol v1; mirrors src/protocol.rs.

Header (16 bytes, big-endian): magic `CRLO`, version, kind, two reserved zero
bytes, request ID (u32) and payload length (u32). The header is validated before
any payload byte is read.

A video-kind payload starts with a 24-byte big-endian packet header: type (1
config, 2 media, 3 end), flags (bit 0 key frame), two reserved zero bytes,
stream generation (u32), PTS in microseconds (i64), width and height (u16 each)
and four reserved zero bytes; the encoder bytes follow.
"""

import json
import socket
import struct
from typing import BinaryIO, NamedTuple

MAGIC = b"CRLO"
VERSION = 1
HEADER = struct.Struct(">4sBBHII")
CONTROL, HIERARCHY, VIDEO = 1, 2, 3
BULK_LIMIT = 8 * 1024 * 1024
LIMITS = {CONTROL: 64 * 1024, HIERARCHY: BULK_LIMIT, VIDEO: BULK_LIMIT}
VIDEO_HEADER = struct.Struct(">BBHIqHHI")
CONFIG, MEDIA, END = 1, 2, 3
KEY_FRAME = 1


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


class VideoPacket(NamedTuple):
    type: int
    key_frame: bool
    generation: int
    pts_us: int
    width: int
    height: int
    data: bytes


def decode_video_packet(payload: bytes) -> VideoPacket:
    """Split and validate the payload of one video-kind frame."""
    if len(payload) < VIDEO_HEADER.size:
        raise ProtocolError("truncated_packet", f"{len(payload)} of {VIDEO_HEADER.size} bytes")
    kind, flags, reserved, generation, pts_us, width, height, tail = VIDEO_HEADER.unpack_from(
        payload
    )
    data = payload[VIDEO_HEADER.size:]
    if kind not in (CONFIG, MEDIA, END):
        raise ProtocolError("unknown_packet_type", f"type {kind}")
    if flags & ~KEY_FRAME or reserved or tail:
        raise ProtocolError("packet_reserved_bits", "reserved packet bits are not zero")
    key_frame = bool(flags & KEY_FRAME)
    if (
        generation == 0
        or (kind != MEDIA and (key_frame or pts_us != 0))
        or (kind == END) != (not data)
    ):
        raise ProtocolError("invalid_packet", "fields contradict the packet type")
    return VideoPacket(kind, key_frame, generation, pts_us, width, height, data)


def encode_video_packet(packet: VideoPacket) -> bytes:
    """The video-kind payload for `packet`; the inverse of `decode_video_packet`."""
    flags = KEY_FRAME if packet.key_frame else 0
    return VIDEO_HEADER.pack(
        packet.type, flags, 0, packet.generation, packet.pts_us, packet.width, packet.height, 0
    ) + packet.data


class VideoChannel:
    """The host end of the video channel: one attach request, then packets only.

    After a successful attach the client must never write again: the agent
    closes the channel on any byte it receives. Every wait is bounded by the
    socket timeout.
    """

    def __init__(self, connection: socket.socket, timeout: float = 10):
        connection.settimeout(timeout)
        self.connection, self.reader = connection, connection.makefile("rb")

    def attach(self, session: str, request_id: int = 1) -> dict | None:
        """Bind this connection to `session`; the agent's answer, None if it closed."""
        message = {"command": "attach", "channel": "video", "session": session}
        self.connection.sendall(encode(CONTROL, request_id, json.dumps(message).encode()))
        try:
            frame = read_frame(self.reader)
        except ConnectionResetError:
            return None
        if frame is None:
            return None
        if frame.kind != CONTROL or frame.request_id != request_id:
            raise ProtocolError("invalid_response", "attach answer is not the control response")
        response = json.loads(frame.payload)
        if not isinstance(response, dict):
            raise ProtocolError("invalid_response", "attach answer is not an object")
        return response

    def receive(self) -> tuple[int, VideoPacket] | None:
        """One packet as (request ID of its video.start, packet); None once closed."""
        try:
            frame = read_frame(self.reader)
        except ConnectionResetError:
            return None
        if frame is None:
            return None
        if frame.kind != VIDEO:
            raise ProtocolError("unexpected_kind", f"kind {frame.kind} on the video channel")
        return frame.request_id, decode_video_packet(frame.payload)

    def close(self) -> None:
        self.reader.close()
        self.connection.close()


class ControlClient:
    """Blocking request/response client for the control channel of `carlo serve`.

    Every wait is bounded by the socket timeout. Request IDs are assigned here and
    the echoed ID of each response is checked.
    """

    def __init__(self, connection: socket.socket, timeout: float = 10):
        connection.settimeout(timeout)
        self.connection, self.reader, self.next_id = connection, connection.makefile("rb"), 1

    def send(self, data: bytes) -> None:
        """Write raw bytes, for frames the codec itself refuses to build."""
        self.connection.sendall(data)

    def receive(self) -> tuple[int, dict] | None:
        """One control response as (request ID, JSON object); None once the agent closed."""
        try:
            frame = read_frame(self.reader)
        except ConnectionResetError:
            return None
        if frame is None:
            return None
        if frame.kind != CONTROL:
            raise ProtocolError("unexpected_kind", f"kind {frame.kind} on the control channel")
        message = json.loads(frame.payload)
        if not isinstance(message, dict):
            raise ProtocolError("invalid_response", "control payload is not an object")
        return frame.request_id, message

    def call(self, message: dict) -> dict:
        request_id, self.next_id = self.next_id, self.next_id + 1
        self.send(encode(CONTROL, request_id, json.dumps(message).encode()))
        response = self.receive()
        if response is None:
            raise ProtocolError("closed", "agent closed the connection before responding")
        if response[0] != request_id:
            raise ProtocolError("invalid_response", "response does not echo the request ID")
        return response[1]

    def hierarchy_dump(self) -> bytes:
        """One complete hierarchy snapshot as the raw UTF-8 XML bytes of a hierarchy frame.

        A control-frame answer is a failure and raises ProtocolError with the
        agent's stable code (`unavailable`, `conflict`, `timeout`, ...), marked
        recoverable because the connection stays usable. The document is screen
        contents: it is returned, never logged or put into an error.
        """
        request_id, self.next_id = self.next_id, self.next_id + 1
        self.send(encode(CONTROL, request_id, json.dumps({"command": "hierarchy.dump"}).encode()))
        try:
            frame = read_frame(self.reader)
        except ConnectionResetError:
            frame = None
        if frame is None:
            raise ProtocolError("closed", "agent closed the connection before responding")
        if frame.request_id != request_id:
            raise ProtocolError("invalid_response", "response does not echo the request ID")
        if frame.kind == HIERARCHY:
            try:
                frame.payload.decode("utf-8")
            except UnicodeDecodeError:
                raise ProtocolError("invalid_response", "hierarchy payload is not UTF-8") from None
            return frame.payload
        if frame.kind != CONTROL:
            raise ProtocolError("unexpected_kind", f"kind {frame.kind} on the control channel")
        try:
            message = json.loads(frame.payload)
        except ValueError:
            raise ProtocolError("invalid_response", "control payload is not JSON") from None
        if not isinstance(message, dict) or message.get("ok") is not False:
            raise ProtocolError("invalid_response", "answer is neither XML nor an error")
        error = message.get("error")
        code = error.get("code") if isinstance(error, dict) else None
        if not isinstance(code, str):
            raise ProtocolError("invalid_response", "answer is neither XML nor an error")
        raise ProtocolError(code, "hierarchy.dump failed", recoverable=True, request_id=request_id)

    def close(self) -> None:
        self.reader.close()
        self.connection.close()
