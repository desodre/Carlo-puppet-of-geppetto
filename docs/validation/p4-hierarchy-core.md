# Complete hierarchy snapshot — #36–#43 (core, without `serve`)

Date: 2026-11-21. Branch `codex/feat/hierarchy-36`. XML contract:
`docs/hierarchy.md`. Binder contract: `docs/compat/samsung-a16x-api36-accessibility.json`.

## Result and scope

`hierarchy-probe` (counters only) and `hierarchy-dump` (XML) take a complete
accessibility snapshot on the pinned Samsung build: all windows of all displays,
every node, 16 attributes, uiautomator-compatible XML. The Carlo XML matched the
device's own `uiautomator dump` on the controlled fixture, and the fixture's
text advanced after one tap.

Not in this increment: the hierarchy through `serve`, and `rotation` (reported
as unknown; #40 is therefore partial). **Stage #5 is not complete**: the 51
parity targets named by the stage are not in this repository, and keyboard,
dialog and window-switch scenarios were not exercised.

## Session on a physical device

One authorized physical device (API 36, arm64-v8a, UID shell, SELinux
enforcing). The contract was read from the device's own `framework.jar` and
`services.jar`. Registration uses flags exactly 1, so enabled accessibility
services are not suppressed; the device had none enabled. While a session
lasts the system reports accessibility as enabled to apps; **after every run
`accessibility_enabled` was read back as 0**, and the smoke fails if it is not
restored.

```sh
python3 tools/build_android.py
python3 tools/android_session.py --serial <serial> --allow-physical \
  --manifest dist/manifest.json --command hierarchy-probe --timeout 30
python3 tools/smoke_hierarchy.py --serial <serial> --allow-physical \
  --manifest dist/manifest.json --fixture dist/node-fixture/fixture.apk --output <dir>
python3 tools/build_accessibility_tests.py --target aarch64-linux-android
python3 tools/android_session.py --serial <serial> --allow-physical \
  --manifest dist/native-tests/manifest.json --command hierarchy-tests --timeout 300
```

## Probe on an idle screen

Five consecutive runs, identical counters: 3 windows on display 0, 190 nodes,
depth 13, 13 batched requests, no single-node fallback, no duplicate, one
attempt, 72 kB of XML, 162–188 ms per snapshot. One `getWindows` poll was
needed each time (see below).

## Fixture smoke

- Carlo: 3 windows, 62 nodes, document valid against the contract, fixture root
  found with the expected resource id, package, clickable and enabled.
- Reference: `uiautomator dump` of the same screen, 6 nodes (active window).
- **Parity: 6 of 6 reference nodes matched, 0 attribute mismatches, 0 index
  differences, 0 Carlo-only nodes in that window.** The reference carries two
  attributes Carlo does not emit (`drawing-order`, `hint`), ignored by design.
- Update after touch: one `input tap` on the fixture, then a new snapshot showed
  the state counter advanced.
- The fixture was uninstalled, the device-side reference file removed, and both
  XML documents deleted; only counters are stored. `uiautomator` registers its
  own automation session, with the same transient effect on the device.

## What the device corrected

- **Bundle layout.** On Android 16 a non-empty `Bundle` is followed by one extra
  boolean (`mHasIntent`). The first run left 4 unread bytes on every node with
  extras; the cause was confirmed in the bytecode of the device's
  `BaseBundle.writeToParcelInner` and the decoder now reads it.
- **Window list readiness.** Window tracking starts at registration, and the
  first `getWindows` returned an empty list. An empty list is now polled for up
  to 2 s before it is reported as an empty screen.
- The inferred `Region` and locales layouts of the window list decoded
  correctly as pinned.

## Resources

Native test `hierarchy-tests`: 1 passed. After two warm-up snapshots, eight
further snapshot cycles held exactly 8 descriptors and 6 threads. The video
regression (`video-tests`, 4 passed) is unaffected by the shared Binder pool.

## Checks

`cargo fmt --all -- --check`; Clippy on host and both Android targets;
`cargo test --locked` (125 library, 11 CLI, 18 serve); 141 Python tests;
`build_android.py` for both ABIs. All passed.

## Limits

- One exact Samsung build. The API 34 reference profile keeps its existing
  single-node proofs and answers `unsupported_profile` for the hierarchy; the
  reference emulator was not run, so the unchanged API 34 commands were verified
  on the host only (byte-identical output there).
- Parity was measured on a 6-node fixture window. Screens with spanned text,
  touch delegates, extra rendering info or leashed nodes fail typed
  (`unsupported_node`); how common that is on real apps is unmeasured beyond the
  idle home screen, which had none.
- Invisible nodes are emitted (uiautomator prunes them); a flat list with
  thousands of items would cost one request per 50 siblings and could hit the
  10 s deadline.
- `rotation` is unknown and display size is derived from window bounds.
- Not exercised: a window disappearing mid-snapshot, the retry path, the
  truncation limits and forced death during a session on the device (host tests
  cover them with a scripted transport).
