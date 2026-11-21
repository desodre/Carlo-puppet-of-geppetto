# Video channel, session binding and `video.start` — #30 / #35 / #44 / #46 / #51

Date: 2026-11-21. Branch `codex/feat/video-channel-30`. Contract:
`docs/protocol.md`.

## Result and scope

`carlo serve` now has two independent channels bound to one session. After the
control `hello`, one more connection may attach as the video channel with the
same session identifier inside a 10 s window; otherwise the session continues
control-only. `video.start` mirrors the main display (display 0) into the
encoder and streams documented packets on the video channel; `video.stop` ends
the stream and releases the display and codec. The control channel kept
answering while video flowed.

`display_capture` and `h264_stream` are reported available only when the exact
mirror profile gate passes; every other profile stays `false`.

## Device run

One authorized physical device, pinned build (API 36, arm64-v8a, UID shell,
SELinux enforcing), abstract transport, both channels through one
`adb forward tcp:0 localabstract:<name>` port. The host woke the screen first.

```sh
python3 tools/build_android.py
python3 tools/smoke_video_stream.py --serial <serial> --allow-physical \
  --manifest dist/manifest.json --output <dir>
```

Three consecutive runs passed. Figures below are from the first.

| Generation | Config | Media | Key frames | Bytes | End packet | Pings answered | Slowest ping |
|---:|---:|---:|---:|---:|---|---:|---:|
| 1 | 1 | 2 | 1 | 31 071 | yes | 30 | 5.9 ms |
| 2 | 1 | 5 | 1 | 28 450 | yes | 10 | 5.2 ms |

Accepted configuration 360×640, 15 fps, 1.5 Mbit/s, key interval 1 s; codec
`c2.exynos.h264.encoder`. In each generation the config packet came first, the
first media packet was a key frame, PTS was non-decreasing and the generation
and dimensions were consistent. `ffprobe` decoded generation 1 as H.264
360×640. The screen was idle, hence few frames. **The stream is real screen
content: it was held in a private temporary location and deleted; only counters
are stored.**

Other sessions in the same smoke:

| Case | Result |
|---|---|
| no video channel | `video.start` → `video_channel_absent`; agent exit 0 |
| wrong-session attach | `invalid_session` on that connection only; control kept working; agent exit 0 |

No port forward, session directory, `carlo serve` process or `carlo-mirror`
display was left on the device.

## Regression on the device

After the mirror session was refactored for streaming: `display-mirror-probe`
and `display-fault-probe` exit 0 with the same classes, `tools/smoke_serve.py`
passed, and the four native tests passed (4 passed, 0 failed), each returning
to 18 descriptors.

## Checks

`cargo fmt --all -- --check`; Clippy on host and both Android targets;
`cargo test --locked` (82 library, 10 CLI, 18 serve); 70 Python tests;
`build_android.py` for both ABIs. All passed. Packet encode/decode is checked
on both sides against `tests/fixtures/video_packets.json`.

## Limits

- One exact Samsung build; API 34 reference not executed.
- Only display 0; another id answers `unsupported_display`. Only the `default`
  encoder preference.
- Not exercised on the device: a stalled video consumer (`consumer_timeout`),
  forced death during a stream, many generations in one process (descriptor and
  thread cycles inside `serve` are unmeasured), the attach deadline, and the
  filesystem transport.
- `video.stop` can wait out one in-flight 2 s packet write; while `video.start`
  or `video.stop` is in progress other control requests wait up to its deadline.
- The 360×640 target letterboxes the 1080×2340 panel; rotation and resize are
  not handled (P6). Slow-consumer queue policy is #48.
- `video.stop` exists because a stream needs it; #50 (hierarchy stays available
  after stop) is not claimed.
