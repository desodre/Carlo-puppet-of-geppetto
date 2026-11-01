# P1.5 — Query and callback transport (#22)

Date: 2026-11-01. Branch: `codex/feat/node-query-22`. Integration prerequisite: PR #85.

## Result

The native shell process queries the active-window root through transaction 3 of
`IAccessibilityServiceConnection`, receives transaction 1 of the interaction
callback, matches its ID, and unregisters. No Java/JNI/DEX/APK agent, root or
SELinux change. Settings is the preinstalled controlled screen for this transport
proof, not the full P1/P4 text fixture.

Final CLI observation: `interaction_id=1`, `node_present=true`,
`opaque_node_bytes=304`, `fields_decoded=false`, exit 0, explicit unregister and
host directory removal. This is a **callback-envelope proof only**. The nullable
presence and trailing ID are validated against the pinned AIDL; the node bytes
between them are not copied, parsed, or semantically validated. A non-null response
must not be consumed as a decoded node. #23 must decode/validate fields and prove
changing fixture text. `hierarchy_available=false` throughout; P1 remains open.

## Contracts and ownership

[Machine-readable contracts and source hashes](../compat/api34-accessibility.json)
fix the existing Android 14 r1 base revision
`299fe6f5d6fc6f1af7c3411dcf4e5efdf7217368`; this is a reference contract, not exact
source provenance for Google's image. Runtime remains restricted to the exact
API 34 x86_64 fingerprint already proved in the session increment.

- Request: active window `2147483647`, root `-2147483650`, positive interaction ID,
  fresh callback Binder, flags 0 (no prefetch), native thread ID, null Bundle.
- Response: Java exception header and nullable package array. Null means rejected;
  window, permission and remote-state causes cannot be distinguished by that value.
  Package strings are unused and not allocated; reply bytes/count are bounded.
- Callback: nullable node + trailing interaction ID. Its private Binder capability
  is forwarded to the app; requiring system UID would reject legitimate results.
- Two additional production bindings: `AIBinder_incStrong` (paired RAII release)
  and `AParcel_setDataPosition` (aligned, bounds-checked envelope observation).
- One request at a time (`&mut Session`), no ID wrap, independent callback state,
  64 KiB parcel cap, 5 s monotonic deadline starting before dispatch. Wrong IDs,
  duplicates, expiry, cancellation and close cannot complete a subsequent request.
  Synchronous Binder still requires host timeout/kill if dispatch never returns.

## Executed validation

Reference runtime: disposable emulator `emulator-5590`, Android 14/API 34 x86_64,
UID 2000, SELinux enforcing, fingerprint recorded in [query.json](p1-node-query/query.json).
Rust 1.96.0 / NDK r28c 28.2.13676358. Both release ABIs cross-built; **arm64 was not
executed**. Final binary sizes: x86_64 533608 bytes, arm64 504816 bytes.

- fmt, host Clippy and Android Clippy for both ABIs passed.
- 25 host Rust tests, 21 Python tests, 21 Android Rust tests passed.
- Pure tests: mismatched/duplicate/late callbacks, cancellation, exact deadline,
  callback-before-delayed-dispatch timeout, null result, invalid envelopes and ID exhaustion.
- Native parcels: truncation, invalid presence/ID, null-with-extra-data, minimum
  opaque envelope and oversized parcel. Opaque payload validity is intentionally
  not asserted until #23.
- Real queries: root, missing-node null callback, invalid-window rejection,
  zero-budget timeout, cancellation, successful subsequent query, closed-session refusal.
- 20 consecutive queries: **9 FDs / 4 threads**, unchanged across all final samples.
  Separate 20 registration cycles also stayed at 9 FDs / 4 threads. Earlier exploratory
  invocation inherited fewer host descriptors (7/4); the invariant is per-process stability.
- Lifecycle: concurrent registration refused without unregistering owner; SIGINT,
  SIGTERM, forced death and host timeout all cleaned up; fresh sessions recovered;
  no ART mapped; no automation left registered.

[Native tests](p1-node-query/native-tests.json),
[lifecycle summary](p1-node-query/summary.json),
[release manifest](p1-node-query/build-manifest.json),
[test manifest](p1-node-query/test-build-manifest.json).

## Observed startup race

The first root query was rejected after successful init; cleanup still succeeded
([initial attempt](p1-node-query/initial-rejection.json)). Later queries succeeded
without any permission/flag changes. Availability after registration is asynchronous;
null rejection alone does not identify its precise cause. The native test retries
only initial setup rejection within a 5 s overall deadline (one retry in final run),
then asserts real queries without retry. The production probe deliberately exposes
`query_rejected`, exit 69. No arbitrary sleep or retry hides it in production.

## Reproduce

Use a disposable reference emulator; the harness refuses physical devices and an
existing automation registration. With the pinned toolchain/NDK environment:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --all-targets --target x86_64-linux-android -- -D warnings
cargo clippy --locked --all-targets --target aarch64-linux-android -- -D warnings
cargo test --locked
python3 -m unittest discover -s tools -p 'test_*.py' -v
python3 tools/build_android.py
python3 tools/build_accessibility_tests.py
adb -s emulator-5590 shell am start -W -a android.settings.SETTINGS
python3 tools/smoke_accessibility.py --serial emulator-5590 --output results/node-query/lifecycle
python3 tools/android_session.py --serial emulator-5590 --command node-query-probe --report results/node-query/query.json
```

The local AVD required sparse userdata creation to fit available tmpfs space; only
its private files were formatted. Other active emulators were preserved. The owned
AVD is stopped and removed after validation. No UI text or raw parcel is in evidence.
