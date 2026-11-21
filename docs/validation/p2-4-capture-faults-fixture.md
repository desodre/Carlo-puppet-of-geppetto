# Capture failures, dependencies and fixture content — #28 / #29

Date: 2026-11-21. Branch `codex/spike/api34-display-route-28`.

## Result and scope

On the pinned Samsung build (API 36, arm64-v8a, UID shell, exact fingerprint in
`docs/compat/samsung-a16x-api36-display.json`), one authorized physical device:

- the three capture failures are separate typed classes and each was provoked;
- the mirrored stream of the controlled fixture was decoded on the host and the
  fixture's stripe marker was located in it;
- the runtime dependencies of the route are recorded in `docs/DEPENDENCIES.md`.

**The API 34 x86_64 reference emulator was not executed** and stays
`unsupported_profile`: no emulator is run on the maintainer's machine, and the
contract cannot be assumed from this build (AOSP 14 has another
`VirtualDisplayConfig` layout and other transaction codes). #28 is therefore
delivered as "only the demonstrated route", which is the Samsung one.

```sh
python3 tools/build_android.py
python3 tools/android_session.py --serial <serial> --allow-physical \
  --manifest dist/manifest.json --command display-fault-probe --timeout 60
python3 tools/build_accessibility_tests.py --target aarch64-linux-android
python3 tools/android_session.py --serial <serial> --allow-physical \
  --manifest dist/native-tests/manifest.json --command video-tests --timeout 300
python3 tools/build_node_fixture.py   # needs JDK 21
python3 tools/smoke_display_mirror.py --serial <serial> --allow-physical \
  --manifest dist/manifest.json --fixture dist/node-fixture/fixture.apk --output <dir>
```

## Failure classes (`display-fault-probe`, exit 0)

| Session | Provocation | Class returned | Transactions list/create/release | Display created |
|---|---|---|---|---|
| permission | `AUTO_MIRROR\|SECURE` (shell lacks `CAPTURE_SECURE_VIDEO_OUTPUT`) | `permission_denied` | 1 / 1 / 1 | no |
| display_absent | target id = highest listed + 1000 | `display_absent` | 1 / 0 / 0 | no |
| unsupported_format | encoder configured 0×0 (`configure` status -10000) | `unsupported_format` | 0 / 0 / 0 | no |

The service does not reject an unknown `displayIdToMirror` (it mirrors the
default display), so Carlo lists displays with `getDisplayIds` (code 8 on this
build, read-only) before every create and refuses an absent id itself. The
device listed one display. The release in the permission session is the guard's,
armed before the create; it returned no error.

The normal `display-mirror-probe` was re-run after the listing was added:
display created and released, 1 SPS / 1 PPS / 1 IDR / 9 non-IDR, 55 061 bytes,
end of stream, exit 0.

## Fixture content (`smoke_display_mirror.py`, exit 0)

The smoke installed the fixture, launched it, mirrored the display, decoded the
dump on the host and uninstalled the fixture. One H.264 High 360×640 frame was
decoded (the screen was static) and it contained the marker: both bands found,
forward above reversed, columns 32–328 (the 1080×2340 panel pillarboxed into
360×640), stripe width 37 px, maximum per-stripe mean colour error 1.4 of 255.
The stream and decoded pixels were deleted by the smoke; the summary stores
counters and statistics only and the device serial was removed from the copy
kept here.

## Resources and cleanup

Two consecutive runs of the four native tests passed (4 passed, 0 failed each),
including the fault sessions; every test returned to 18 descriptors and its
thread floor after idle. No `carlo-mirror` display and no fixture package were
left on the device.

## Checks

`cargo fmt --all -- --check`; Clippy on host and both Android targets;
`cargo test --locked` (46 library + 10 CLI); 40 Python tests; `build_android.py`
for both ABIs. All passed.

## Limits

- One exact Samsung build; API 34 reference not executed (see above).
- `smoke_accessibility.py` / `smoke_node_decoder.py` were not run against the new
  fixture layout or the shared Binder pool: both need the reference emulator.
  The marker is a background drawable and adds no accessibility node, but that
  is unverified on a device.
- A `SecurityException` from the listing call would also be classed
  `permission_denied`; the probe tells the cases apart by transaction counts.
- A codec `start` failure for a non-format reason would be classed
  `unsupported_format`.
- Only one frame was decoded in the fixture run; motion, rotation and secure
  windows were not exercised.
