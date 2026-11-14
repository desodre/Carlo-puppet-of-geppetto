# Protocol v1 framing — #33 / #34

Status: framing only. Transport, sessions, handshake and commands (#30–#32, #35)
are not implemented; `serve` still returns 69. Implemented by `src/protocol.rs`
(agent) and `tools/carlo_protocol.py` (host), both checked against the shared
vectors in `tests/fixtures/protocol_frames.json`.

## Header

Fixed 16 bytes. Every multi-byte integer is **big-endian** (network order).

| Offset | Size | Field | Rule |
|---:|---:|---|---|
| 0 | 4 | magic | ASCII `CRLO` |
| 4 | 1 | version | `1` |
| 5 | 1 | kind | see below |
| 6 | 2 | reserved | must be zero |
| 8 | 4 | request ID | echoed by the response; `0` when there is no request |
| 12 | 4 | length | payload bytes that follow |

| Kind | Payload | Limit |
|---:|---|---:|
| 1 control | UTF-8 JSON | 64 KiB |
| 2 hierarchy | raw XML bytes | 8 MiB |
| 3 video | raw encoded bytes | 8 MiB |

Binary payloads are never Base64. The magic and this 16-byte layout do not change
between versions, so a version rejection is readable by any peer.

## Reading rules

The header is validated in order — magic, version, reserved, length against the
kind's limit — before a single payload byte is read or allocated. The payload
buffer is reserved fallibly for exactly the declared length. Reads may arrive in
any fragmentation, including one byte at a time and interrupted calls.

| Condition | Code | Response | Stream afterwards |
|---|---|---|---|
| EOF before the first header byte | — | none; clean close | closed |
| EOF inside header or payload | `truncated_frame` | none | close |
| Transport error or deadline mid-frame | `io` | none | close |
| Wrong magic | `bad_magic` | rejection, request ID 0 | close |
| Other version | `unsupported_version` | rejection with `supported_version` | close |
| Reserved bytes not zero | `reserved_bits` | rejection, request ID 0 | close |
| Length above the limit | `frame_too_large` | rejection with `limit` | close |
| Buffer cannot be reserved | `out_of_memory` | rejection, request ID 0 | close |
| Unknown kind, length ≤ 8 MiB | `unknown_kind` | rejection, same request ID | **continues** |

An unknown kind is the only recoverable case: its payload is discarded without
allocation and the next frame is read normally. Every other error leaves the
stream desynchronised, so the reader sends the rejection (if any) and closes.

A rejection is a control frame: `{"error":{"code":"<code>"}}`, plus
`supported_version` or `limit` where listed. Unknown *commands* inside a valid
control frame belong to the command layer (#32) and are not defined here.

## Validation

Host only; no device or platform call is involved. Executed on 2026-11-14:

- `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`
  (host, `x86_64-linux-android`, `aarch64-linux-android`): passed.
- `cargo test --locked`: 27 library + 9 CLI tests passed, 5 of them for framing:
  shared vectors decoded one byte per read with interruptions and re-encoded
  byte-identically; oversized lengths rejected after consuming exactly 16 bytes;
  EOF at every cut position; unknown kind skipped; transport error and redacted
  `Debug`.
- `python3 -m unittest discover -s tools -p 'test_*.py'`: 26 passed, 3 for the
  host codec against the same vectors.
- `python3 tools/build_android.py` with NDK 28.2.13676358: both ABIs built.

Limitations: no socket, deadline or peer-credential handling yet; the codec has
not run on a device because nothing in the binary calls it. Vectors are synthetic.
