# Real display mirrored into the encoder — #26 / #27

Date: 2026-11-21. Branch `codex/spike/display-capture-route-26`.

## Result and scope

Carlo, running as `shell` with no Java/JNI/DEX/APK, mirrored the real default
display into its AMediaCodec H.264 encoder and the host decoded the stream. The
route is one private transaction, `IDisplayManager.createVirtualDisplay` with flag
`AUTO_MIRROR` only and the encoder input surface, released with
`releaseVirtualDisplay`. `screen_capture` is true for this proof;
`video_available` stays false because there is no streaming protocol yet.

This is **not** stage acceptance for #3: the profile is not the API 34 reference
and no fixture content was recognised (see Limits).

## Contract and profile

Pinned in `docs/compat/samsung-a16x-api36-display.json` from the device's own
`framework.jar` and `services.jar`, read with `dexdump` and never executed. This
vendor shifts the AOSP transaction codes by six (create 27, release 30; AOSP 16
uses 21 and 24), so the gate requires the exact fingerprint
`samsung/a16xub/a16x:16/BP4A.251205.006/A166MUBSADZJA:user/release-keys`, API 36,
arm64-v8a and UID 2000. Any other profile returns `unsupported_profile` without a
Binder call. One authorized physical device was used.

```sh
python3 tools/build_android.py
python3 tools/android_session.py --serial <serial> --allow-physical \
  --manifest dist/manifest.json --command display-mirror-probe --timeout 40
python3 tools/build_accessibility_tests.py --target aarch64-linux-android
python3 tools/android_session.py --serial <serial> --allow-physical \
  --manifest dist/native-tests/manifest.json --command video-tests --timeout 300
```

The host woke the screen first (`input keyevent KEYCODE_WAKEUP`); a display that
is off produces no frames.

## Probe

Display id 6 created and released; callbacks `onResumed` 1, `onStopped` 1,
rejected 0. Codec `c2.exynos.h264.encoder`, 360×640. Five packets, 52 820 bytes:
1 SPS, 1 PPS, 1 IDR, 2 non-IDR, end of stream, non-decreasing PTS. 5.3 s
elapsed, exit 0, session directory removed. The screen was idle, so only three
frames were composed in the five-second window.

## Host decode

`display-mirror-dump` returned 45 232 bytes. `ffprobe` read 3 H.264 High
yuv420p frames at 360×640. Luma per frame: minimum 11–13, mean 118.1, maximum
243–246, so the frames are neither blank nor the synthetic pattern. **The stream
holds real screen contents: it was deleted after these statistics and no frame
was viewed or stored.**

## Resources and cleanup

Three consecutive runs of the three native tests passed (3 passed, 0 failed
each). The mirror cycle returned to the floor of 18 descriptors and 16 threads
after idle. The thread floor is 16 instead of 12 because the process Binder pool
now runs for the process lifetime. No `carlo-mirror` display was left in
`dumpsys display` after the probe, the dump or the test runs.

## Checks

`cargo fmt --all -- --check`; Clippy on host and both Android targets;
`cargo test --locked` (42 library + 10 CLI); 30 Python tests; `build_android.py`
for both ABIs. All passed.

## Limits

- One exact Samsung build. Not executed on the API 34 x86_64 reference emulator
  that #28 names; the contract must be pinned again per build.
- No fixture: content was checked by statistics only, so "fixture content
  recognisable" in #3 is unproven. The fixture has no visual marker yet.
- Secure windows are blank (`shell` lacks `CAPTURE_SECURE_VIDEO_OUTPUT`); this was
  read from the server code, not observed.
- Rotation, resize and screen-off transitions were not exercised (P6). The
  360×640 target letterboxes the 1080×2340 panel.
- Permission-denied, missing-display and incompatible-format paths (#29) are
  typed and host-tested but were not provoked on the device.
- The server emits a Knox Zero-Trust event naming the calling package on every
  creation; any policy reaction to it is unknown.
- The synchronous transaction is not cancellable; it is bounded only by the host
  session, after which the server's death link destroys the display.
- Native String8/String16/Binder encodings are assumed from AOSP libbinder; the
  run succeeding is the only device-side confirmation.
- `smoke_accessibility.py` was not re-run after the Binder pool moved to
  `binder::pool`; the reference emulator was not started.
