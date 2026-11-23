# Protocol v1 — framing (#33 / #34), control transport and handshake (#31 / #32), video channel and stream (#30 / #35 / #44 / #46 / #51), hierarchy dump

Status: framing, a control channel with its handshake, and a video channel bound
to the same session. `carlo serve` serves one client over two connections of the
same Unix socket, both exposed through one `adb forward`: control (`hello`,
`ping`, `capabilities`, `channels`, `hierarchy.dump`, `video.start`,
`video.stop`, `shutdown`) and video (agent-to-client packets). There is no HTTP
and no WebSocket, no reconnection and no second client. `display.info` and
`stats` answer `unavailable`. [`hierarchy.dump`](#hierarchy-dump) answers a
document only where the exact hierarchy profile gate passes (Samsung a16x,
API 36, arm64, shell) and `unavailable` everywhere else; **over serve it is
host-tested only and was not executed on any device**.
The stream exists only where the display-mirror profile gate passes (one pinned
build: Samsung a16x, API 36, arm64, shell); on every other target `video.start`
answers `unavailable`. Not implemented: displays other than the main one,
rotation, reconfiguration of a running stream, statistics, hardware/software
encoder reporting, and any queueing policy for slow consumers beyond one write
deadline. Implemented by `src/protocol.rs`, `src/serve/` and `src/video/`
(agent) and `tools/carlo_protocol.py` (host); framing and video packets are
checked against the shared vectors in `tests/fixtures/protocol_frames.json` and
`tests/fixtures/video_packets.json`. The video channel and the stream ran
on the host and on the pinned device build. See [Validation](#validation).

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
| 3 video | a [video packet](#video-packets): 24-byte header and encoder bytes | 8 MiB |

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
| Accept | the control connection within 30 s. The listener then stays open only to admit the [video channel](#channels-and-binding); it is closed, and the path removed, when that channel attaches, when its window ends or at exit | exit 74 |
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
| Connections admitted | two at most: control, then one video attachment; listener closed at the latest 10 s after `hello`; path unlinked | same; the kernel frees the name on close, nothing to unlink |
| Session identifier in `hello` | required | required |
| Absolute handshake deadline | 30 s | 30 s |
| Name takeover | an existing path is refused | a live name cannot be bound twice (`EADDRINUSE` aborts) |

With `abstract`, another process of an accepted UID (same UID; root on
Android) can race the host for the control accept. It cannot authenticate
without the session identifier and the agent then exits non-zero, so the effect
is a failed session, never an attached one. The same process can use up the
three video attachment attempts: the effect is a session without video
(`"video":"absent"`), never a foreign attachment and never a disturbed control
channel.

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
| `hello` → attached video channel (listener lifetime) | 10 s | listener closed, `"video":"absent"`, control continues |
| Accepted video connection → complete `attach` frame (never past the 10 s above) | 5 s | that connection closed, control continues |
| `attach` response write | 50 ms | video channel closed, control continues |
| `video.start` → display created and encoder started | 5 s | `timeout`, control continues |
| Each video packet write | 2 s | stream ends (`consumer_timeout`), video channel closed, control continues |
| `video.stop` or teardown → stream thread finished (includes ≤ 3 s for the encoder's end of stream) | 8 s | `timeout` (stop) or the thread is abandoned at exit (teardown) |
| SIGHUP, SIGINT, SIGTERM | observed within 50 ms by the control loop (after a `video.start`/`video.stop` in progress) | stream stopped, both connections closed, socket removed, exit 74 |

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
`doctor` reports (name, available, reason): `serve_protocol` is `true`;
`display_capture` and `h264_stream` are `true` only on the pinned mirror build
and `false` with a reason everywhere else, the host and the API 34 reference
included; `binder_hierarchy` is `true` only where the exact hierarchy profile
gate passes and `false` with a reason everywhere else, the host and the API 34
reference included.

| Request | State | Response | Connection |
|---|---|---|---|
| valid `hello` | before handshake | hello object above | continues |
| `hello`, `protocol` missing or not integer `1` | before | `unsupported_protocol` with `"supported":[1]` | **close**, exit 74 |
| `hello`, `session` missing, mistyped or wrong | before | `invalid_session` | **close**, exit 74 |
| any other command | before | `handshake_required` | **close**, exit 74 |
| `hello` | after | `already_initialized` | continues |
| `ping` | after | `{"ok":true}` | continues |
| `capabilities` | after | `{"ok":true,"capabilities":[...]}` | continues |
| `shutdown` | after | `{"ok":true}` | **close** (stream stopped, video channel closed), exit 0 |
| `channels` | after | `{"ok":true,"video":"pending"\|"attached"\|"absent"\|"closed"}` | continues |
| `video.start`, `video.stop` | after | see [Stream control](#stream-control) | continues |
| `hierarchy.dump` | after | hierarchy frame with the XML, or a control error; see [Hierarchy dump](#hierarchy-dump) | continues |
| `display.info`, `stats` | after | `unavailable` | continues |
| any other command name | after | `unknown_command` | continues |
| not UTF-8 JSON, not an object, `command` missing or not a string | any | `invalid_request` | continues |
| hierarchy or video frame | any | `unexpected_kind` | continues |
| framing error | any | per [Reading rules](#reading-rules) | close, exit 74 (unknown kind continues) |
| EOF on a frame boundary | after | — | clean close, exit 0 |
| EOF on a frame boundary | before | — | exit 74 |

Requests that continue before the handshake do not authenticate and do not
extend the handshake deadline. On every exit the socket path, if still present
and still the inode Carlo created, is removed.

Control ending for any reason (`shutdown`, EOF, a deadline, a framing error, a
signal) ends the session: a running stream is stopped in order (display
released, encoder drained, `end` packet if the channel is still writable), the
video connection and the listener are closed, the socket path is removed, and
the exit code is the one of the control outcome. Nothing on the video side can
end control.

## Channels and binding

Two independent connections to the same socket (the same `adb forward` port)
form a session:

1. **Control** is the first connection and authenticates with `hello`.
2. **Video** is one further connection, admitted only after a successful
   `hello` and only until the listener closes.

| Step on a video connection | Rule | Failure |
|---|---|---|
| Admission window | from the `hello` response until the channel is attached, 10 s passed or three attempts were refused, whichever comes first; then the listener is closed for the rest of the session | later connects are refused by the OS |
| Peer | same `SO_PEERCRED` rule as control, checked before any byte is read | closed unread; counts as an attempt |
| First frame | within 5 s of the accept, and never past the 10 s window: a control frame `{"command":"attach","channel":"video","session":"<hex>"}`; the session is compared in constant time | answered if possible, then closed; counts as an attempt |
| Success | `{"ok":true,"channel":"video"}` with the request ID of the `attach`; `channels` reports `attached` no later than this response | — |

Refusal answers on the video connection (best effort, then close):
`invalid_session`, `attach_required` (another command), `unsupported_channel`,
`invalid_request`, `unexpected_kind`, or the framing rejection of
[Reading rules](#reading-rules). A refused or stalled attempt affects only its
own connection: control is never closed, delayed or informed, and the slot stays
open for another attempt within the same window. A second attachment is
impossible because the listener is closed before the first one is acknowledged.

`channels` values: `pending` (window open), `attached`, `absent` (window ended
without an attachment; permanent), `closed` (was attached; permanent).

After the attachment the video channel is **agent-to-client only**. Any byte
received from the client, an end of file, a transport error, or a packet write
that misses its deadline closes the video channel (`closed`); a running stream
ends with reason `channel_closed` or `consumer_timeout`. A missed write deadline
closes the channel because part of a frame may already be on the wire. Control
continues in every one of these cases; video cannot be re-attached in the same
session.

Threads: the control loop runs alone on the main thread and never reads or
writes the video connection. One thread owns the listener, performs the video
admission and then watches the video connection's read side; one thread per
stream runs the capture and is the only writer of packets. They share atomics
and in-process channels only, so a stalled or absent video peer cannot delay a
control response beyond the 50 ms slice in which waits are polled.

## Video packets

Every frame on the video channel after the attach response has kind 3. Its
request ID is the request ID of the `video.start` that started the stream. Its
payload is a fixed 24-byte big-endian header followed by data:

| Offset | Size | Field | Rule |
|---:|---:|---|---|
| 0 | 1 | packet type | `1` config, `2` media, `3` end |
| 1 | 1 | flags | bit 0: key frame (media only); other bits zero |
| 2 | 2 | reserved | zero |
| 4 | 4 | stream generation | u32; `1` for the first successful `video.start` of the process, `+1` for each later one |
| 8 | 8 | PTS | i64 microseconds, as reported by the encoder; `0` for config and end |
| 16 | 2 | width | u16, encoder output width |
| 18 | 2 | height | u16, encoder output height |
| 20 | 4 | reserved | zero |

| Type | Data |
|---|---|
| config | the codec configuration buffer (H.264 Annex B SPS/PPS) exactly as the encoder emitted it |
| media | one encoder output buffer (Annex B access unit); at most 8 MiB − 24 bytes |
| end | empty; the last packet of its generation |

Order within a generation: a config packet precedes the first media packet; the
first media packet is a key frame (IDR); media PTS do not decrease; `end` is
last. Concatenating the data of the config and media packets of one generation
yields a decodable Annex B stream. If the encoder emits a media buffer before
its configuration or a first frame that is not a key frame, nothing of it is
forwarded and the stream ends with reason `error` (`media_before_config`,
`first_frame_not_key`). An `end` packet is sent whenever the channel is still
in step: after `video.stop`, after such an error and at teardown; not after
`consumer_timeout` or `channel_closed`. Packets of one generation never follow
its `end`, and generations do not overlap. Packets may arrive before the
`video.start` response does, since the channels are independent.

A decoder rejects a payload shorter than 24 bytes (`truncated_packet`), an
unknown type (`unknown_packet_type`), a reserved bit or byte that is not zero
(`packet_reserved_bits`), and fields that contradict the type: key flag or PTS
outside media, data after end, empty config or media, generation zero
(`invalid_packet`).

## Stream control

`video.start` request; every field is optional and defaults to the value shown:

```json
{"command":"video.start","display":0,"max_width":360,"max_height":640,"fps":15,"bitrate":1500000,"encoder":"default"}
```

| Field | Accepted | Otherwise |
|---|---|---|
| `display` | integer `0` (the main display; #51) | another integer: `unsupported_display`; not an integer: `invalid_request` |
| `max_width`, `max_height` | integers 16..=1920; an odd value is rounded down to even. The encoder is configured with exactly these two values | `invalid_request` |
| `fps` | integer 1..=60 | `invalid_request` |
| `bitrate` | integer 100000..=20000000 (bits per second) | `invalid_request` |
| `encoder` | the string `"default"` | another string: `unsupported_encoder`; not a string: `invalid_request` |

`invalid_request` takes precedence over the two `unsupported_*` codes. Unknown
fields are ignored. No value sent by the client is echoed.

Checks, in this order, each with its own code:

| Condition | Response |
|---|---|
| request invalid | `invalid_request`, `unsupported_display` or `unsupported_encoder` |
| video channel not `attached` | `video_channel_absent` |
| a stream is running (or a timed-out start is still winding down) | `already_streaming` |
| this target is not the pinned mirror build | `{"ok":false,"error":{"code":"unavailable","reason":"unsupported_profile"}}`; nothing is opened and no Binder call is made |
| display or encoder could not be started | `permission_denied`, `display_absent`, `unsupported_format`, `display_refused`, `timeout` or `internal` |

Success is answered only after the virtual display was created and the encoder
started, at most 5 s after the request:

```json
{"ok":true,"generation":1,"display":0,"codec":"<encoder name or null>",
 "accepted":{"width":360,"height":640,"fps":15,"bitrate":1500000,"key_frame_interval_seconds":1}}
```

`accepted` is the configuration the encoder was configured with and accepted.
A failed start consumes no generation and puts nothing on the video channel.
Failure codes carry no exception text, status or identifier:
`permission_denied` (the display service refused), `display_absent` (the display
is not listed), `unsupported_format` (the encoder rejected the configuration),
`display_refused` (the service returned no display), `timeout` (the 5 s passed;
the late start is cancelled and writes nothing) and `internal` (anything else).

A stream runs until `video.stop`, the end of control, a packet write deadline,
the video channel closing, or a capture/encoder error. There is no frame cap.
When it ends, the virtual display is released first, the encoder is told to
finish and drained to its end of stream (≤ 3 s), `end` is sent if the channel is
in step, and everything is dropped on the stream's own thread.

`video.stop` ends a running stream, or reports one that already ended by
itself, once:

```json
{"ok":true,"generation":1,"reason":"stopped","packets":42,"bytes":183000,"key_frames":3}
```

| `reason` | Meaning | Video channel afterwards |
|---|---|---|
| `stopped` | ended by this `video.stop` | `attached`; a new `video.start` gets the next generation |
| `consumer_timeout` | a packet was not accepted by the client within 2 s | `closed` |
| `channel_closed` | the client sent something, closed, or the connection failed | `closed` |
| `error` | capture, encoder or packet-contract failure; `error_code` holds the stable code | `attached` |

`packets` counts the config and media packets written, `bytes` their data
bytes, `key_frames` the media packets flagged as key frames. With no stream to
stop or report: `{"ok":false,"error":{"code":"not_streaming"}}`. If the stream
does not finish within 8 s: `timeout`, and the stream stays tracked. A
`video.start` after a stream that ended by itself discards its unread report.

Security notes: the video channel carries screen contents, so it is admitted
only for a peer that passed the same kernel credential check as control and
presented the session identifier; the identifier is never written to stderr, a
response or a packet. The listener's exposure after `hello` is bounded by the
10 s window and three attempts. The agent never reads commands from the video
channel. Responses and stop reports contain counters and constants only.

## Hierarchy dump

`{"command":"hierarchy.dump"}` after the handshake takes one complete snapshot
(the contract, limits and XML are `docs/hierarchy.md`). Any other request field
is ignored.

- **Success:** one **hierarchy frame** (kind 2) whose request ID is the
  request's and whose payload is the raw UTF-8 XML document: never Base64, never
  wrapped in JSON, at most the 8 MiB frame limit (the snapshot itself stops at
  4 MiB). The document is real screen contents: the agent never logs it or puts
  it into an error, and clients must treat it the same way.
- **Failure:** one control frame `{"ok":false,"error":{"code":"..."}}` with
  only the code; no operation name, status or UI content reaches the client.

| Code | Cause |
|---|---|
| `unavailable` | not the pinned hierarchy profile (host, API 34 reference, any other build): no library is loaded and no Binder call is made |
| `conflict` | another automation session is registered |
| `windows_unavailable` | the window list answered null |
| `inconsistent_snapshot` | the tree changed during every bounded attempt |
| `truncated` | a node, depth, XML or deadline limit was reached, or the document exceeds the frame limit; no partial XML is sent |
| `unsupported_node` | a node used an encoding that is not pinned |
| `timeout` | registration or a node request timed out |
| `internal` | any other session, Binder or decoding failure |

The first `hierarchy.dump` of a serve session registers one UiAutomation
session (flags exactly 1, never suppressing the user's accessibility services)
and later dumps reuse it, skipping registration and the `init` wait. While it
is held, another automation client (`uiautomator`, a second agent) gets
`conflict`, and `accessibility_enabled` stays `1`. If the system revokes the
connection (`init` with a null connection) or a Binder call reports
`DEAD_OBJECT`, the stale session is unregistered and released and re-opened at
most once for that dump; a failed open keeps nothing, so the next dump tries
again. The registration is unregistered in the serve session's teardown, which
runs on every exit (shutdown, framing failure, signal), and by the source's
destructor otherwise. Snapshot ids and the geometry generation continue across
one serve session. The control loop waits for the snapshot; other control
requests and the reaction to a signal are delayed by up to its worst case:
about 5 s for registration (first dump or re-open only), plus the 10 s snapshot
deadline, plus one in-flight `getWindows` Binder call, which is synchronous and
cannot be cancelled (it is not bounded by the agent). No signal handler is
installed for the dump: SIGHUP, SIGINT and SIGTERM stay with serve's own
handlers and its stop flag cancels every snapshot wait at its next check; the
registration is then released by the teardown and the serve session ends as
interrupted (exit 74). The connection continues after either answer.

The host client is `ControlClient.hierarchy_dump()` in
`tools/carlo_protocol.py`: it returns the XML bytes and raises `ProtocolError`
with the agent's code (recoverable) on a control answer.

## Validation

**`hierarchy.dump` over serve is host-tested only.** On the host the real agent
is not the pinned build: `hierarchy.dump` answers `unavailable` and
`binder_hierarchy` is `false`. The success path, the hierarchy frame, the
request-ID echo, every error mapping, the frame-limit and UTF-8 refusals, and
the release of the source on shutdown, framing failure and signal are proven
through the production control loop with a fake source injected behind the
same trait the device source implements (test-only code), including
consecutive dumps on one source with teardown only at the end; the device
source, on the pinned build, reuses the snapshot walk of `hierarchy-dump` over
a persistent registration. The revocation bookkeeping of the callback slot and
the `DEAD_OBJECT`/revoked classification are unit-tested on the host; the
persistent session itself (lazy registration, reuse, re-open, unregister on
exit) is Android-only code that was **not built or executed** for this change.
Not executed: `hierarchy.dump` over serve on any device, consecutive snapshots
in one serve process, a revocation or dead peer while registered, a snapshot
concurrent with a running stream. `tools/smoke_serve.py` requires
`hierarchy.dump` to answer `unavailable` wherever `binder_hierarchy` is not
available; where it is, with `--allow-hierarchy` it refuses when an
accessibility service is enabled, takes two consecutive snapshots in the same
serve session, records only the frame kind, the dump count and the byte counts,
and checks that `accessibility_enabled` is restored after the agent has exited.

Host (Linux x86_64) and one authorized physical device (Samsung API 36 arm64,
shell, SELinux enforcing). Device evidence for the control channel is in
`docs/validation/p3-serve-handshake.md`.

**The video channel, `video.start`/`video.stop` and the packet stream have not
been executed on any device.** Everything below about them is host evidence:
the real agent on a host is not the pinned build and therefore proves
attachment, binding, deadlines and refusals but answers `unavailable` to
`video.start`; packet order, generations, counters, consumer timeout and
teardown are proven with a synthetic source injected behind the same trait the
display mirror implements (test-only code). The refactored mirror session was
cross-compiled for both ABIs but not run.

Executed on the host on 2026-11-21:

- `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`
  (host, `x86_64-linux-android`, `aarch64-linux-android`): passed.
- `cargo test --locked`, three consecutive runs: 82 library, 10 CLI and 18 serve
  tests passed each time.
  - Library, framing and control: the shared frame vectors one byte per read,
    oversized lengths, EOF at every cut, unknown kind, transport error; the
    control state machine (every row of the table, no echo of client strings,
    request-ID echo); directory validation, peer rule, `SO_PEERCRED` on a
    socket pair; typed accept/handshake/idle/write deadlines and stop flag with
    short test-only durations; the transport decision with injected bind
    results and the abstract listener (name format, refused second bind, a
    second queued connection, name released on drop).
  - Library, video: the shared video packet vectors (valid packets re-encoded
    byte for byte; truncated, unknown type, reserved bits and contradictory
    fields rejected); `video.start` validation (defaults, bounds, rounding,
    every ill-typed field, precedence); the attach frame rules. Through the
    production control loop, a real abstract listener and the synthetic
    source: two start/stop cycles with config → key frame → media → end,
    generations 1 and 2, the `video.start` request ID on every packet, counters
    equal to the packets received, `already_streaming`, `not_streaming`, and
    `ping` answered within 200 ms after each of 40 packets; a consumer that
    never reads (256 KiB frames) ends with `consumer_timeout` and a `closed`
    channel while `ping` stays prompt; a client byte, a client frame or a client
    close on the video channel closes only that channel (`channel_closed`);
    wrong session, wrong command, wrong channel, non-JSON and a bad magic are
    refused, two refusals leave the slot open, three close the listener; the
    attach deadline yields `absent`; a peer stalled inside its attach frame is
    cut at the handshake deadline and at the attach deadline without delaying
    control; every start failure code, `unavailable`/`unsupported_profile`, and
    a start slower than its deadline (`timeout`, nothing written, no generation
    consumed); encoder output without configuration or without a leading key
    frame (`error` with its code, `end` still sent); control EOF while
    streaming and while a video peer is stalled (stream stopped in order, both
    connections and the listener closed). Mirror: the probe consumer ends at
    the frame count or the capture deadline as before, the pinned create
    request is unchanged, failures map to stable codes, and the profile gate
    refuses every other target. Capabilities: host and API 34 reference report
    only `serve_protocol`.
  - `tests/serve.rs` runs the real binary with a 0700 temp directory and a std
    client: the earlier control cases (full session, one-byte fragmentation,
    handshake rejections, framing rejections, unknown-kind recovery, EOF in
    each phase, refused directories, pre-existing path, strict arguments,
    signals, abstract fallback) plus: socket mode 0600, present after `hello`
    and removed once the video channel attaches; attach over both transports;
    `channels`; `video.start` without a channel (`video_channel_absent`),
    with one (`unavailable`, `unsupported_profile`), with invalid fields, no
    echo of client strings; `video.stop` when idle (`not_streaming`); a client
    frame on the video channel closes it only; wrong-session and non-attach
    connections leave control working and the slot open; three refusals close
    the listener; one test waits for the real deadlines: a video peer stalled
    mid-attach is cut at 5 s and the window ends at 10 s with `absent` and the
    path removed, while `ping` is answered within 1 s throughout; SIGTERM with
    both channels attached and SIGHUP with a stalled video peer exit 74 within
    3 s with nothing left.
- `python3 -m unittest discover -s tools -p 'test_*.py'`: 70 tests passed,
  including the host codec against both vector files, the `VideoChannel`
  reader, the serve smoke's exercises against the host binary, and the video
  stream smoke against a scripted agent (two generations; each contract
  violation — missing config, first frame not key, PTS going backwards, wrong
  generation, size or request ID, missing `end`, wrong counters, slow `ping`,
  failed decode — fails the smoke) and against the host binary (no channel,
  wrong-session attach, `unavailable` after a real attach).
- `python3 -m compileall -q tools`: passed.
- `python3 tools/build_android.py` and
  `python3 tools/build_accessibility_tests.py --target aarch64-linux-android`
  with NDK 28.2.13676358: built.

Executed on the pinned device build (evidence in
`docs/validation/p3-video-channel.md`): the updated `tools/smoke_serve.py` and
`tools/smoke_video_stream.py` over the abstract transport, with both channels
through one `adb forward` port, two start/stop generations of the real mirror
session, config before a key frame, real PTS and ffprobe on the real stream.

Not executed: descriptor/thread cycles across many generations inside one serve
process; forced-death recovery during a stream; a stalled video consumer on the
device; the filesystem transport on any device; the API 34
reference emulator; a build where `adbd` runs as root. A peer with a different
UID was not produced on the host (the rule is unit-tested as a pure function).
The real 30 s/10 s control deadlines, the 5 s start, 8 s stop and 2 s packet
deadlines were not waited for (short test-only durations instead); only the
10 s attach window and its 5 s handshake were. Framing and packet vectors are
synthetic.

Known limits: closing after a framing violation with unread client bytes makes
the client see a reset after the rejection instead of EOF. The directory check
and bind are separate system calls; the guarantee rests on the directory being
0700 and owned by the agent's user. The session identifier is visible to
whoever reads the agent's stdout. A `video.start` or `video.stop` in progress
delays other control requests, and the reaction to a signal, by up to its own
deadline (5 s, 8 s): they wait for the capture pipeline. Only `video.stop` (and
teardown) can also wait for the video peer, and then for at most the one 2 s
packet write that was already in flight, after which the stream reports
`consumer_timeout`; no other control response depends on the video peer. A stream thread stuck in a platform call that cannot be cancelled is
abandoned at exit; the display service then destroys the display through its
death link. While the video window is open, connections of an accepted UID can
use up the three attempts and leave the session without video.
