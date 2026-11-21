# Protocol v1 — framing (#33 / #34), control transport and handshake (#31 / #32)

Status: framing, one control channel and its handshake. `carlo serve` accepts one
client and answers `hello`, `ping`, `capabilities` and `shutdown`. No data command
exists: hierarchy, video and `stats` answer `unavailable`, there is no second
channel or channel binding (#30, #35), no reconnection and no second client.
Doctor's `serve_protocol` capability therefore stays `false`. Implemented by
`src/protocol.rs` and `src/serve/` (agent) and `tools/carlo_protocol.py` (host);
framing is checked against the shared vectors in
`tests/fixtures/protocol_frames.json`. Validated on the host; the one device
run so far ended at socket creation and motivated the abstract fallback, which
has not itself run on a device. See [Validation](#validation).

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
`supported_version` or `limit` where listed. It has no `ok` member, which
distinguishes it from a command response.

## Transport: `carlo serve --session-dir <dir>`

The host owns `<dir>`; Carlo never creates, widens or removes it.

| Step | Rule | Failure |
|---|---|---|
| Arguments | exactly `--session-dir <absolute path>` | exit 64 |
| Directory | exists, real directory (no final symlink), owned by the effective UID, `mode & 077 == 0` | exit 69, nothing created |
| Socket (preferred) | `<dir>/control.sock`, Unix stream, path ≤ 107 bytes, created with mode 0600 under umask 0177 (no chmod window) | exit 69; other bind errors exit 74 |
| Existing path | any existing `control.sock` (file, socket, symlink) is refused and left untouched; no fallback | exit 69 |
| Socket (fallback) | only when creating the file fails with permission denied (`EACCES`/`EPERM`): abstract-namespace name `carlo-<32 hex>` from 16 fresh `/dev/urandom` bytes, unrelated to the session identifier | exit 74 |
| Ready line | one JSON line on stdout, flushed; the only stdout data of the process | exit 74 |
| Accept | exactly one client within 30 s; then the listener is closed and the path removed before any byte is read | exit 74 |
| Peer | `SO_PEERCRED` UID must equal the agent's effective UID; on Android only, UID 0 is also accepted | closed unread, exit 74 |

Ready line:

```json
{"ready":true,"schema_version":1,"protocol":1,"transport":"filesystem","socket":"<dir>/control.sock","session":"<32 hex digits>"}
{"ready":true,"schema_version":1,"protocol":1,"transport":"abstract","socket":"carlo-<32 hex digits>","session":"<32 hex digits>"}
```

Exactly one of the two is printed. For `abstract`, `socket` is the name without
its leading NUL (`@` in some tools). The agent alone chooses the transport;
there is no option to select it. The directory is validated first in both cases:
it remains the host-owned place for the binary.

### Security model per transport

| Property | `filesystem` | `abstract` |
|---|---|---|
| Who can attempt to connect | only the owner (directory 0700, socket 0600) | any local process that learns or guesses the name |
| Name secrecy | not needed | 128 random bits, printed only on the ready line; not relied upon |
| Peer credential check before any read | mandatory | mandatory, and the only OS-level access control |
| Single accept, listener closed at once | yes; path unlinked | yes; the kernel frees the name on close, nothing to unlink |
| Session identifier in `hello` | required | required |
| Absolute handshake deadline | 30 s | 30 s |
| Name takeover | an existing path is refused | a live name cannot be bound twice (`EADDRINUSE` aborts) |

With `abstract`, another process of an accepted UID (same UID; root on
Android) can race the host for the single accept. It cannot authenticate
without the session identifier and the agent then exits non-zero, so the effect
is a failed session, never an attached one.

`session` is 128 bits read from `/dev/urandom` (a short read aborts before the
line is printed). It is a bearer secret for this one session: it appears on
stdout only, never on stderr or in any error, and must be presented in `hello`.

Peer rule: `adbd` opens the forwarded connection, as `shell` (2000) on user
builds, matching an agent started through `adb shell`. On userdebug/eng builds
after `adb root`, `adbd` is root while an agent may still be `shell`, so root is
accepted on Android. Off Android only the same UID is accepted. Architectures
other than x86_64 and aarch64 have no verified `SO_PEERCRED` constants and
refuse every peer.

The peer rule is identical for both transports.

Exposure from the host, chosen from `transport`, removed with
`adb forward --remove tcp:<port>`:

- `filesystem`: `adb forward tcp:0 localfilesystem:<dir>/control.sock`
- `abstract`: `adb forward tcp:0 localabstract:carlo-<hex>`

Why the fallback exists: on a Samsung API 36 user build (shell UID 2000,
SELinux enforcing, `u:r:shell:s0`), `bind` of the socket file in a shell-owned
0700 `shell_data_file` directory failed with `EACCES`. No AVC record was visible
to shell, so the SELinux cause (no `sock_file` create for that domain) is
inferred, not observed. There is no TCP fallback.

Deadlines and signals (constants, not configurable):

| Wait | Bound | Outcome |
|---|---:|---|
| Accept | 30 s | exit 74, socket removed |
| Accept → successful `hello` (absolute, not extended by any frame) | 30 s | close, exit 74 |
| Each complete request frame after `hello` | 30 s | close, exit 74 |
| Each response write | 10 s | close, exit 74 |
| SIGHUP, SIGINT, SIGTERM | observed within 50 ms | close, socket removed, exit 74 |

A deadline that expires mid-frame sends no rejection: the stream is not aligned.

## Control channel

Requests are control frames holding a JSON object `{"command":"<name>", ...}`.
Every response is a control frame with the request's ID, either
`{"ok":true, ...}` or `{"ok":false,"error":{"code":"<code>"}}`. Responses are
built from constants and agent data only; nothing sent by the client is echoed.

The first request must be `{"command":"hello","protocol":1,"session":"<hex>"}`.
`protocol` is checked before `session`; the session is compared in constant
time. Success:

```json
{"ok":true,"protocol":1,
 "agent":{"name":"carlo","version":"0.1.0"},
 "device":{"android_api":34,"abi":"x86_64","architecture":"x86_64"},
 "capabilities":[{"name":"serve_protocol","available":false,"reason":"..."}],
 "limits":{"control_bytes":65536,"payload_bytes":8388608}}
```

`android_api` and `abi` are `null` off Android. `capabilities` is the same list
`doctor` reports (name, available, reason); every entry is `false` today.

| Request | State | Response | Connection |
|---|---|---|---|
| valid `hello` | before handshake | hello object above | continues |
| `hello`, `protocol` missing or not integer `1` | before | `unsupported_protocol` with `"supported":[1]` | **close**, exit 74 |
| `hello`, `session` missing, mistyped or wrong | before | `invalid_session` | **close**, exit 74 |
| any other command | before | `handshake_required` | **close**, exit 74 |
| `hello` | after | `already_initialized` | continues |
| `ping` | after | `{"ok":true}` | continues |
| `capabilities` | after | `{"ok":true,"capabilities":[...]}` | continues |
| `shutdown` | after | `{"ok":true}` | **close**, exit 0 |
| `hierarchy.dump`, `display.info`, `video.start`, `video.stop`, `stats` | after | `unavailable` | continues |
| any other command name | after | `unknown_command` | continues |
| not UTF-8 JSON, not an object, `command` missing or not a string | any | `invalid_request` | continues |
| hierarchy or video frame | any | `unexpected_kind` | continues |
| framing error | any | per [Reading rules](#reading-rules) | close, exit 74 (unknown kind continues) |
| EOF on a frame boundary | after | — | clean close, exit 0 |
| EOF on a frame boundary | before | — | exit 74 |

Requests that continue before the handshake do not authenticate and do not
extend the handshake deadline. On every exit the socket path, if still present
and still the inode Carlo created, is removed.

## Validation

Host (Linux x86_64) and one authorized physical device (Samsung API 36 arm64,
shell, SELinux enforcing). Device evidence is in
`docs/validation/p3-serve-handshake.md`. Executed on the host on 2026-11-21:

- `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`
  (host, `x86_64-linux-android`, `aarch64-linux-android`): passed.
- `cargo test --locked`, three consecutive runs: 68 library, 10 CLI and 13 serve
  tests passed each time. The transport decision is unit-tested with injected
  bind results (only permission denied selects the fallback); the abstract
  listener is tested for name format and uniqueness, refused second bind,
  accept, peer credentials and refused connect after accept; and the whole
  fallback runs end to end against the real binary using a 0500 session
  directory, where the socket file cannot be created (skipped for root). Library: the framing tests above (shared vectors one
  byte per read, oversized lengths, EOF at every cut, unknown kind, transport
  error), the control state machine (every row of the table, no echo of client
  strings, request-ID echo), directory validation, peer rule, `SO_PEERCRED` on a
  socket pair, and typed accept/handshake/idle/write deadlines and stop flag
  with short test-only durations. `tests/serve.rs` runs the real binary with a
  0700 temp directory and a std client: full session, one-byte fragmentation,
  socket mode 0600 and removal after accept, every handshake rejection, five
  framing rejections before and after `hello`, unknown-kind recovery, EOF in
  each phase and mid-frame, refused directories (modes, missing, file, symlink,
  long path), pre-existing path, strict arguments, and SIGTERM/SIGINT/SIGHUP
  while waiting and with a client attached.
- `python3 -m unittest discover -s tools -p 'test_*.py'`: passed, including the
  host codec against the shared vectors and the serve smoke's exercises driven
  against the host binary over a Unix socket.
- `python3 tools/build_android.py` with NDK 28.2.13676358: both ABIs built.

Executed on the device: `tools/smoke_serve.py` passed three consecutive runs over
the abstract transport through `adb forward tcp:0 localabstract:`, so `adbd`'s
peer UID was accepted and Bionic's `SO_PEERCRED` works at runtime. Not executed:
the filesystem transport on any device (`localfilesystem:` forwarding is
unproven), the API 34 reference emulator, and a build where `adbd` runs as root. A peer with a different UID was not
produced on the host (the rule is unit-tested as a pure function). The real
30 s/10 s deadlines were not waited for. Framing vectors are synthetic.

Known limits: closing after a framing violation with unread client bytes makes
the client see a reset after the rejection instead of EOF. The directory check
and bind are separate system calls; the guarantee rests on the directory being
0700 and owned by the agent's user. The session identifier is visible to
whoever reads the agent's stdout.
