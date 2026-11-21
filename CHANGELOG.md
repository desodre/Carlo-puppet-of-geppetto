# Changelog

## Unreleased

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
