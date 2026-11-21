# Encoder surface as a Binder producer — #27 (first increment)

Date: 2026-11-21. Branch `codex/spike/display-capture-route-26`.

## Result and scope

The input surface of the AMediaCodec encoder can be flattened with the public NDK
call `ANativeWindow_writeToParcel` (libnativewindow, API 34) and its graphic
producer read back as a Binder object, with no cast of `ANativeWindow`, no private
symbol and no Java. This is the prerequisite for handing the surface to a display
service. **No display was touched and no private transaction was sent**;
`screen_capture` and `video_available` stay false. Contract:
`docs/compat/surface-parcel.json`.

## Executed profile

One authorized physical device, API 36, arm64-v8a, UID shell (2000),
`u:r:shell:s0`. Not executed on the API 34 x86_64 reference emulator, where this
was cross-built and linted only.

```sh
python3 tools/build_android.py
python3 tools/android_session.py --serial <serial> --allow-physical \
  --manifest dist/manifest.json --command surface-parcel-probe
python3 tools/build_accessibility_tests.py --target aarch64-linux-android
python3 tools/android_session.py --serial <serial> --allow-physical \
  --manifest dist/native-tests/manifest.json --command video-tests --timeout 240
```

## Probe

Codec `c2.exynos.h264.encoder`. Parcel of 72 bytes, fully consumed by the five
pinned fields: empty name, `isSingleBuffered` 0, magic `bque` matched, producer
present, control handle null. Exit 0, session directory removed.

**The producer is a local Binder (`producer_remote: false`).** The buffer queue
lives in Carlo's own process, so whoever receives it calls back into Carlo. The
display route therefore needs a running Binder thread pool, owned as in
`src/accessibility/`.

## Resources

Six consecutive runs of the two native tests passed (2 passed, 0 failed each).
Every sample of every run read 18 descriptors and 12 threads: floor, ten probe
cycles and both after-idle readings. Raw samples are in
`p2-3-surface-parcel/summary.json`.

Two platform behaviours were found and handled:

- A codec deleted after `createInputSurface` without `start` leaves one
  `C2NodeImpl` thread per session (12 after 12 cycles, not reclaimed after idle).
  The probe now starts the encoder and feeds it no frames; the thread no longer
  accumulates. The cause inside the platform was not investigated.
- Sampling descriptors immediately after a session occasionally saw up to five
  transient descriptors (2 of 7 runs before the change, in either test). Samples
  now take the lowest of five readings 100 ms apart, so a persistent leak still
  fails. The transient descriptors were not identified.

## Checks

`cargo fmt --all -- --check`; Clippy on host and both Android targets;
`cargo test --locked` (34 library + 9 CLI); 30 Python tests; `build_android.py`
for both ABIs. All passed.

## Limits

- Only the (API 36, aarch64) shell profile is executed; others return
  `unsupported_profile`.
- Whether a display service accepts this producer, and whether the encoder then
  receives frames, is unproven; that is the next step of #26/#27.
- `smoke_accessibility.py` was not run; accessibility code is unchanged.
