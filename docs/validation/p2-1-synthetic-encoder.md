# P2.1 — Synthetic H.264 encoder (#25)

Date: 2026-11-13. Branch `codex/spike/synthetic-h264-encoder-25`.

## Result and scope

A native shell process creates an H.264 encoder through `AMediaCodec`, obtains its
input Surface, renders 30 generated frames into it with EGL/GLES 2, drains every
output buffer and receives end of stream. The host decodes the dumped stream and
recognizes the generated pattern. No Java/JNI/DEX/APK, root or SELinux change.

**This is a synthetic source, not screen capture.** Frames are `glClear` calls
(color ramp plus a moving white bar); no display, layer or Surface of another
process is read. Reports state `source=synthetic_egl_gles`, `screen_capture=false`
and `video_available=false`; the doctor capability `h264_stream` stays unavailable.
Display mirroring (#26, #27) is unproven and not attempted here.

## Executed profile

| Item | Value |
|---|---|
| Device | Samsung SM-A166M, physical, `user` build, authorized by its owner for this run |
| Android | API 36, `arm64-v8a`, UID 2000, `u:r:shell:s0` |
| Toolchain | rustc 1.96.0, NDK 28.2.13676358, build floor API 29 |
| Encoder selected by the platform | `c2.exynos.h264.encoder` |
| Requested = effective | 360×640, 15 fps, 1.5 Mbit/s, key-frame interval 1 s, 30 frames |

The API 34 x86_64 reference emulator was **not** executed for this delivery, and
x86_64 was only cross-built and linted. `EXECUTED_PROFILES` therefore admits only
API 36 `aarch64` under shell; every other profile returns `unsupported_profile`.
The accessibility proofs keep their own API 34 fingerprint gate and were not rerun.

## Observed stream

| Measure | Agent report | Host (independent) |
|---|---|---|
| Output buffers / bytes | 32 / 17 543 | 17 543 bytes, SHA-256 `5632b2bb…57aa4c51` |
| Codec-config buffers | 1 | — |
| SPS / PPS | 1 / 1 | 1 / 1 |
| IDR / non-IDR | 2 / 28 | 2 / 28 |
| Coded frames | 30 of 30 submitted | `ffprobe`: 30 frames, `h264` High, 360×640, `yuv420p` |
| PTS | 0 … 1 933 333 µs, non-decreasing | — |
| End of stream | acknowledged | — |

Host pixel check on decoded RGB frames 0, 15 and 29: bar at x = 0, 157 and 304 is
white, background outside it is not, and the background ramps from blue to red.
Four probe sessions plus one dump session ran; every session directory was removed
and no agent process remained.

## Contracts and ownership

Only public NDK exports, checked against the pinned NDK headers
`media/NdkMediaCodec.h`, `media/NdkMediaFormat.h`, `android/native_window.h`,
`EGL/egl.h`, `EGL/eglext.h` and `GLES2/gl2.h`: 18 from `libmediandk.so`,
`ANativeWindow_release` from `libnativewindow.so`, 13 from `libEGL.so` and 7 from
`libGLESv2.so`. They are resolved with `dlopen`/`dlsym`, so the binary still has
only `libc`/`libdl` as `NEEDED` and starts where a library is missing. The table
is static: codec loopers and the EGL driver own threads, so libraries are never
unloaded. No Binder thread pool is started by Carlo for this probe.

- One RAII owner per handle: format, codec (stop before delete), input window,
  EGL display/context/surface. Drop order is EGL surface → window → codec.
- Each dequeued output index is released exactly once, also when accounting
  rejects its contents. Buffer offset/size are validated against the reported
  capacity before a slice is formed; the slice ends before release.
- Limits before any copy: 8 MiB per packet, 16 MiB per probe, 76 output buffers.
  A 10 s monotonic deadline is checked between calls. `eglSwapBuffers` and codec
  calls are not cancellable; the bounded host session is the backstop.
- Annex B accounting is safe Rust: start code required, forbidden-zero bit and
  truncated headers rejected, NAL types counted. Nothing is logged to stderr.
- `video-encode-dump` writes stream bytes only when the probe passed; otherwise
  stdout stays empty and the exit code is 69.

## Failures and resources

- Unsupported format (0×0): the native test gets a typed `codec` error from
  `configure`/`create_input_surface`/`start`, no output and no new descriptor.
- Missing library/symbol, absent encoder, EGL and GL failures have distinct error
  kinds with the operation name and numeric code. Permission denial was not
  observed on this profile and has no dedicated kind yet (#29).
- Resource cycles in one process, `[fds, threads]`: floor `[18, 12]`; five
  back-to-back sessions `[20,14] [22,14] [24,13] [26,13] [28,14]`; after the
  rejected format `[28, 13]`; after a 6 s idle period plus one session, twice,
  `[18, 12]` and `[18, 14]`.

Each session that produces output leaves two ashmem descriptors that are not
closed by `AMediaCodec_stop`/`AMediaCodec_delete`. Stage isolation showed codec
create/configure/start and EGL setup add none; they appear once the encoder emits
output, whether or not Carlo dequeues it. They are closed by the platform on the
next codec activity once about five seconds old: 36 → 18 descriptors after one
idle period, and a 40-session burst stayed 36 above the floor instead of 80. This
matches a time-evicted client cache in the platform media stack; the owning
component was not identified from source. It is bounded but real, and matters for
the P7 descriptor limits (#58): rapid start/stop does not return to the floor
immediately.

## Limitations

- One physical profile; hardware encoder chosen by the platform, software or
  other vendors unverified. No encoder selection or hardware/software report (#45).
- Fixed preset, no options, no protocol framing; the dump is a raw byte stream.
- No same-device decode and no latency, CPU or memory measurement.
- arm64 ran on 4 KiB pages; 16 KiB pages remain unvalidated (#65).

## Reproduction

With the authorized device connected:

```sh
export ANDROID_NDK_HOME=/path/to/android-sdk/ndk/28.2.13676358
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --all-targets --target x86_64-linux-android -- -D warnings
cargo clippy --locked --all-targets --target aarch64-linux-android -- -D warnings
cargo test --locked
python3 -m unittest discover -s tools -p 'test_*.py' -v
python3 tools/build_android.py
python3 tools/build_accessibility_tests.py --target aarch64-linux-android
python3 tools/smoke_video_encoder.py --serial <serial> --allow-physical \
  --tests-manifest dist/native-tests/manifest.json --output results/video-encoder
```

Executed: 28 Rust host library tests and 9 CLI tests, 27 Python tests, Clippy on
host and both Android targets, both release builds, and on the device four probe
sessions, one dump with host decode, and one native video test (about 21 s).

[Smoke summary](p2-1-synthetic-encoder/summary.json) and
[build manifest](p2-1-synthetic-encoder/build-manifest.json).
