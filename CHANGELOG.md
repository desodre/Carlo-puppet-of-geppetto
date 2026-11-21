# Changelog

## Unreleased

- Add a complete accessibility hierarchy snapshot on a second pinned profile
  (#36, #37, #38, #39, #40, #41, #42, #43; refs #5). **Host-tested only: nothing
  in this entry was executed on a device.** `hierarchy-probe` reports counters,
  limits and the typed error as JSON without any UI content; `hierarchy-dump`
  prints a uiautomator-compatible XML document, or nothing and exit 69 unless
  the whole snapshot succeeded. Contract and limits: `docs/hierarchy.md`.
  - Profiles are data selected by exact fingerprint, API, architecture and shell
    UID: `reference_api34` (unchanged session and single-node proofs; hierarchy
    answers `unsupported_profile`) and `samsung_a16x_api36` (window list, batched
    node answers, 23-field service info), recorded in
    `docs/compat/samsung-a16x-api36-accessibility.json`; executed on that build
    with parity against `uiautomator dump` on the fixture. Any other device gets no Binder call.
  - Registration flags are asserted to be exactly 1 so the user's accessibility
    services are never suppressed.
  - A separate API 36 node layout (bits 0..49) next to the untouched API 34 one;
    typed-list callbacks, the invalid-window sentinel and the `getWindows` reply
    are decoded in safe Rust with field-naming errors.
  - The walk prefetches descendants depth first, deduplicates by window and node
    id, keeps no cache between snapshots, retries an inconsistent attempt at
    most twice and never returns a partial tree: `inconsistent_snapshot`,
    `truncated` (deadline 10 s, 10 000 nodes, depth 128, XML 4 MiB) and
    `unsupported_node` are typed errors. Rotation is reported as `unknown`.
  - The Binder pool is requested at one documented size (`pool::THREADS`, 4) by
    the automation session and the display mirror, so either may start it.
  - Tooling: `android_session.py` gains `hierarchy-probe`, `hierarchy-dump`
    (file-only output) and `hierarchy-tests` (one profile-gated snapshot through
    the test harness); `tools/test_hierarchy_xml.py` parses the golden document
    shared with the Rust writer test. `serve` does not offer the hierarchy yet.
- `serve` gains the video channel and stream control (#30, #35, #44, #46, #51).
  After the control `hello` the same listener admits exactly one more
  connection within 10 s: peer credentials first, then one
  `{"command":"attach","channel":"video","session":…}` frame within 5 s
  (constant-time comparison). A refused attempt closes only that connection;
  three refusals or the deadline close the listener and leave the session
  control-only. The socket path is now removed when the video channel attaches,
  when its window ends or at exit, no longer right after the control accept.
  New control commands: `channels`, `video.start` (display, maximum size, fps,
  bitrate, encoder preference; only display 0 and the default encoder; answers
  the accepted configuration, generation and codec name) and `video.stop`
  (reason and counters). Video packets are kind-3 frames with a documented
  24-byte header (type, key-frame flag, generation, PTS, dimensions): config
  before media, first media a key frame, `end` closing each generation. Stable
  error codes: `video_channel_absent`, `already_streaming`, `not_streaming`,
  `unsupported_display`, `unsupported_encoder`, `unavailable` (with
  `unsupported_profile`), `permission_denied`, `display_absent`,
  `unsupported_format`, `display_refused`, `timeout`, `internal`. Control runs on
  its own thread and never reads or writes the video socket; a packet write
  that misses its 2 s deadline ends the stream and closes the video channel
  only. Streaming works solely where the display-mirror profile gate passes
  (one pinned build); everywhere else `video.start` answers `unavailable`.
- The display mirror session is now one implementation driven through a
  consumer trait: the bounded probes and their reports are unchanged, and the
  stream uses the same request, release guard and drop order on a dedicated
  thread.
- Capabilities are computed: `serve_protocol` is available; `display_capture`
  and `h264_stream` are available only on the pinned mirror build and `false`
  with a reason on every other target; `binder_hierarchy` stays `false`.
  `doctor` text output now prints `available`/`unavailable` per capability.
- Tooling: `tools/carlo_protocol.py` decodes video packets and has a
  `VideoChannel` reader; shared vectors in `tests/fixtures/video_packets.json`;
  serve smoke helpers moved to `tools/serve_session.py`;
  `tools/smoke_video_stream.py` proves both channels, two stream generations,
  control responsiveness while streaming and a host decode. It passed three
  consecutive runs on the pinned build (#30, #35, #44, #46, #51).
- Implement `carlo serve --session-dir <dir>`: one control channel for exactly
  one client over `<dir>/control.sock` (#31, #32). The directory must be absolute,
  real, owned by the effective user and mode 0700; the socket is created 0600
  under a temporary umask, an existing path is refused, and the path is removed
  as soon as the client is accepted and on every exit. A 128-bit session
  identifier from `/dev/urandom` is printed on a single JSON ready line and must
  be presented in `hello`; the peer's `SO_PEERCRED` UID must equal the agent's
  (root also accepted on Android only). Handshake reports protocol, agent
  version, API/ABI, limits and the doctor capability list (all still `false`,
  `serve_protocol` included). `ping`, `capabilities` and `shutdown` work;
  `hierarchy.dump`, `display.info`, `video.start`, `video.stop` and `stats`
  answer `unavailable`. Accept, handshake, idle and write waits are bounded and
  SIGHUP/SIGINT/SIGTERM end the process with the socket removed. `serve` without
  a valid `--session-dir` is now an argument error (64) instead of 69. Validated
  on the host with Rust and Python clients.
- `serve` falls back to an abstract-namespace socket `carlo-<32 hex>` (fresh
  random name, unrelated to the session identifier) only when creating the
  socket file is denied, as observed on a Samsung API 36 user build (`EACCES`
  in a shell-owned 0700 directory; SELinux cause inferred). The ready line now
  carries `"transport":"filesystem"|"abstract"`. Peer-credential check, single
  accept, session identifier and handshake deadline are unchanged and are the
  whole access control for the abstract transport. `tools/smoke_serve.py`
  forwards with `localfilesystem:` or `localabstract:` accordingly, records the
  transport, and reports the agent's exit code and a sanitised stderr tail when
  no ready line arrives. Three consecutive smoke runs passed on that build over the abstract
  transport (#31, #32).
- Restore `pub mod protocol` in `src/lib.rs`: the declaration was lost in a
  merge, so the framing module and its tests were not being compiled.
- `tools/android_session.py`: drop the `serve` command (it now blocks) and split
  `resolve_artifact`, `create_directory` and `deploy` out of `run_session` for
  the serve smoke; add `ControlClient` to `tools/carlo_protocol.py`.
- Classify three display mirror failures apart from every other error, as
  `failure_class` in the outcome: `permission_denied` (service
  `SecurityException`), `display_absent` (target id missing from
  `IDisplayManager.getDisplayIds`, transaction 8 on the pinned build; decided by
  Carlo because the service would silently mirror the default display) and
  `unsupported_format` (encoder rejects the configuration before the display
  service is contacted). The mirror now lists displays before every create and
  reports `displays_listed` and per-session transaction counts. Add
  `display-fault-probe`, which provokes the three failures in isolated sessions
  under the same exact-build gate and passes only if no display was created.
  Executed on that build: each class was returned with the expected
  transaction counts and no display was created (#29).
- Give the fixture a striped visual marker and add `tools/smoke_display_mirror.py`,
  which finds it in the decoded mirror stream and keeps statistics only. It ran on
  the pinned build with `--allow-physical`; the fixture was uninstalled afterwards.
- Record the runtime dependencies of the capture route in `docs/DEPENDENCIES.md`
  (libraries, runtime-resolved symbols, service/transactions, permissions, host
  tools) with a profile table: only the Samsung a16x API 36 arm64 build is
  executed; the API 34 x86_64 reference emulator was not run and stays
  unsupported (#28).
- Add `display-mirror-probe`/`display-mirror-dump`: mirror the default display
  into the AMediaCodec H.264 encoder by sending one private
  `IDisplayManager.createVirtualDisplay` transaction (flags `AUTO_MIRROR` only)
  with the encoder input surface, drain the output with bounded deadlines and
  release the display under an RAII guard. Gated to one exact build
  (`samsung/a16xub/a16x:16/BP4A.251205.006/A166MUBSADZJA`, API 36 arm64, `shell`):
  any other profile reports `unsupported_profile` without a Binder call. Request
  order, String8 packing and reply handling are host-tested. Executed on that
  build: real display frames were encoded and decoded on the host. `video_available` stays
  `false`. Contract in `docs/compat/samsung-a16x-api36-display.json` (#26, #27).
- Move the process-owned Binder thread pool into `binder::pool`, shared by the
  automation session (1 kernel-spawned worker, unchanged) and the mirror session
  (4); a foreign or smaller pool is refused.

- Add `surface-parcel-probe`: flatten the input surface of a started,
  frameless encoder with the public `ANativeWindow_writeToParcel` into a local
  scratch parcel and report, with bounded reads, the `bque` magic and whether the
  producer Binder is present. Nothing is transacted, no display is touched and the
  surface name is reduced to its length. Same executed-profile gate as the encoder
  plus API 34; executed on API 36 arm64, where the producer is a local Binder (#27).
  Contract pinned in `docs/compat/surface-parcel.json`.

- Add `video-encode-probe`/`video-encode-dump`: H.264 from AMediaCodec with a
  Surface input fed by synthetic EGL/GLES frames, drained with bounded packet,
  stream and deadline limits. Public NDK exports are resolved at runtime. This is
  not display capture; `h264_stream` stays unavailable. Executed on API 36 arm64
  only; other profiles report `unsupported_profile` (#25).
- Add binary stdout to the session helper, a `video-tests` harness filter, an
  arm64 option for the native test build and `smoke_video_encoder.py`, which decodes
  the stream on the host and checks the generated pattern and resource cycles.

- Decode the pinned API 34 minimum AccessibilityNodeInfo layout with bounded safe
  Rust reads, Unicode, node/window IDs, children, bounds and boolean properties.
  Reject unknown masks, unsupported spans/layouts and Binder/FD-bearing parcels.
- Add explicit `node-read-probe` stdout data while keeping query diagnostics redacted;
  full hierarchy remains unavailable. Add controlled fixture text-change validation,
  parser mutation/truncation coverage and native NDK interop tests (#23).
- Document verified CI acceptance for host checks, both Android ABI checks/builds
  and native test harness artifacts (#78); device execution remains a separate check.

- Add protocol v1 framing: 16-byte big-endian header (magic, version, kind,
  request ID, length) for JSON control and raw XML/video payloads (#33).
- Validate length per kind before reading or allocating; define responses for
  fragmented reads, EOF, other versions, oversized and unknown frames (#34).
  Agent and host codecs share fixture vectors. Transport and `serve` stay unimplemented.

- Add reference-profile `node-query-probe`: one active-root request, isolated callback,
  interaction-ID correlation, bounded envelope/deadline and explicit null/rejection
  results. Node fields remain undecoded (#23); hierarchy capability stays unavailable.
- Cover stale/duplicate/expired callbacks, invalid NDK envelopes and repeated real
  queries with resource/lifecycle checks. Preserve native test reports on failure.

- Add the reference API 34 x86_64 accessibility session proof: versioned contracts,
  bounded Binder callbacks/pool, minimal ServiceInfo registration and connection init.
- Add idempotent unregister, safe conflict refusal, SIGINT/SIGTERM cleanup and
  forced-death recovery validation. Keep hierarchy explicitly unavailable.
- Add Android-native malformed-parcel/resource-cycle tests and reproducible
  lifecycle smoke tooling; build the reference test harness in CI.

- Add API 34 shell `binder-probe`: dynamically resolve four C Binder exports,
  lookup/ping accessibility, test a null control lookup and release strong refs
  before unloading the library. Report unsupported targets and failures as JSON.
- Add reproducible system/public-NDK symbol inventory and P1.1 source/decision
  evidence; keep hierarchy unavailable until registration and parcel proofs pass.

- Add issue lifecycle policy, labels, native parent/subissue tracking, linked-branch
  workflow, issue/PR templates, CI and parent acceptance automation.
- Add the initial `carlo` CLI: help, version and native environment diagnostics in
  text/JSON, typed errors and documented exit codes.
- Add pinned Rust 1.96.0/NDK r28c builds for x86_64 and arm64, ELF alignment checks,
  checksums and artifact manifests.
- Add bounded deployment into private Android session directories with checksum/ABI
  checks, physical-device opt-in and cleanup on success, error and timeout.
- Keep Binder hierarchy, screen capture, streaming and serve protocol explicitly
  unavailable pending their respective roadmap stages.
