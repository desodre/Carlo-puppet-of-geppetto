# Minimum node decoder and CI acceptance — #23 / #78

Date: 2026-11-02. Branch `codex/feat/node-decoder-23`, based on the open PR #85
(which contains merged PR #86). This increment does not merge either branch.

## Result and scope

Carlo now decodes the supported minimum AccessibilityNodeInfo fields on the exact
API 34 x86_64 reference image: text, package/class, content description, resource
and unique IDs, source/window/parent IDs, children, three bounds and boolean
properties. A controlled app root changed text after a host ADB tap, and the next
query observed it. The agent runs as shell, without Java/JNI/DEX/APK bridge, root
or SELinux modifications. The fixture APK is solely the app under test.

`node-query-probe` now reports `fields_decoded=true` with metadata only.
`node-read-probe` explicitly emits decoded root data on stdout. Neither agent
stderr nor test evidence includes UI text. Node Debug output is redacted. The
fixture harness keeps decoded data in memory and records only comparisons and
artifact identities. Direct use of the deployment helper prints command data;
its optional `--report` saves that data only when requested.

The P1 reference-image proof now covers a real node and observable text change.
Full hierarchy/XML, general app compatibility and parent-stage integration remain
pending. `hierarchy_available=false` is intentional. The fixture's accessibility
delegate exposes deterministic root fields; it is not a broad UI-parity benchmark.
Before/after reads use separate sessions; same-session query/lifecycle cycles are
covered by the native harness, not a same-session text-change assertion.

## Decoder boundary

[Versioned contracts/source hashes](../compat/api34-accessibility.json) pin Android
14 r1 base revision `299fe6f5d6fc6f1af7c3411dcf4e5efdf7217368`, including
AccessibilityNodeInfo, TextUtils and BaseBundle. This remains a reference contract,
not exact source provenance for Google's runtime image.

- NDK `AParcel_marshal` copies argument bytes in memory only, after checking total
  parcel size (64 KiB). It refuses any Binder/FD object; no private C++ casts.
- Safe Rust parses a 45-field bitmap in fixed order. Full callback consumption and
  its trailing interaction ID are checked after decoding. Truncation cannot become
  a partial successful node.
- CharSequence uses String8 UTF-8; resource/unique IDs use String16 UTF-16. Validate
  lengths, terminators, encodings and four-byte alignment. Preserve valid Unicode.
- Limits: 8192 bytes/UTF-16 units per string, 1024 children, 128 actions/extra keys.
  Checked offsets and available child bytes precede allocations. Bounds are read
  as top/bottom/left/right and reject inverted geometry.
- Core boolean properties decode from the pinned bit masks. Password is preserved
  as a flag; explicit data output is not a privacy-filtered hierarchy API.
- Unknown field bits, nonempty text spans, TouchDelegateInfo, ExtraRenderingInfo,
  and leash/Binder/FD layouts return explicit `unsupported_node_layout` errors.
  Empty span lists and plain text are supported; styled text is not yet general.
- Range/collection layouts are consumed and validated but not exported as features.
  Bundle extras are checked for size, magic and boundaries, then omitted:
  `extras_omitted=true`. Their contents are not semantically validated, instantiated
  as Parcelables, or advertised as decoded fields.

## Executed evidence

Reference runtime: disposable `emulator-5600`, API 34 x86_64, UID 2000,
`u:r:shell:s0`, SELinux enforcing. Runtime fingerprint is unchanged from P1.5:
`google/sdk_gphone64_x86_64/emu64xa:14/UE1A.230829.050/12077443:userdebug/dev-keys`.

- fmt, host Clippy and Android Clippy for both ABIs passed.
- **31 host Rust tests**, **23 Python tests**, **27 native Android Rust tests** passed.
- Parser tests cover Unicode/null/defaults, IDs, actions, booleans, all byte-prefix
  truncations, deterministic mutations, invalid lengths/UTF/terminators/bounds,
  unsupported masks/spans/layouts and trailing data.
- NDK tests independently write a Unicode String16 through the platform API and
  verify the Rust decoder, and verify object-bearing parcels are rejected.
- Real root/null/rejected query, timeout/cancellation/recovery and closed-session
  checks passed. Twenty consecutive decoded queries remained at **9 FDs / 4 threads**;
  twenty separate session cycles had the same stable counts. These are short-run
  resource observations, not a long-duration leak/performance benchmark.
- Concurrent registration rejection, INT/TERM cleanup, forced-death recovery and
  host timeout cleanup passed. No ART mapping in Carlo; no automation left active.
- Controlled fixture verified text change, Unicode, IDs, bounds and booleans.
  It was installed only on the owned emulator and uninstalled in `finally`.
- Both release ABIs built with Rust 1.96.0 and NDK r28c 28.2.13676358.
  x86_64: 555984 bytes; arm64: 511584 bytes. **arm64 was not executed**.
- Fixture built using JDK 21, compile SDK 35, build-tools 36.0.0, min/target SDK 34.
  The initial build-tools 34/D8 attempt failed with the host default JDK; pin the
  documented fixture build environment. No Gradle/network build dependencies.

Artifacts: [fixture checks](p1-node-decoder/fixture.json),
[native tests](p1-node-decoder/native-tests.json),
[lifecycle summary](p1-node-decoder/summary.json),
[release build](p1-node-decoder/build-manifest.json),
[native test build](p1-node-decoder/test-build-manifest.json),
[fixture build](p1-node-decoder/fixture-build-manifest.json).
No raw runtime parcel or decoded UI content is persisted in these reports.

## #78 — CI acceptance

The existing `.github/workflows/ci.yml` already implements this issue's requested
checks. This increment supplies parser/test coverage and records acceptance against
its PR CI run; it does not duplicate that workflow.

| Job | Coverage |
|---|---|
| quality | Pinned Rust, fmt, host Clippy, Rust tests, Python tests/compileall |
| android | Pinned NDK, Clippy for x86_64/arm64, both release builds and ELF verification, reference test-harness build, uploaded `dist/` artifacts |

Green builds prove compilation and host tests. CI does **not** boot an emulator,
run native Binder tests or execute the fixture; those results above were obtained
separately on the reference emulator. Broader #75/#77 remain open.

## Reproduce

With the pinned Rust/NDK environment and an owned reference emulator:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --all-targets --target x86_64-linux-android -- -D warnings
cargo clippy --locked --all-targets --target aarch64-linux-android -- -D warnings
cargo test --locked
python3 -m unittest discover -s tools -p 'test_*.py' -v
python3 tools/build_android.py
python3 tools/build_accessibility_tests.py
# JAVA_HOME/PATH must select JDK 21; SDK needs platform 35 and build-tools 36.0.0.
python3 tools/build_node_fixture.py --sdk /path/to/android-sdk
python3 tools/smoke_node_decoder.py --serial emulator-5600
adb -s emulator-5600 shell am start -W -a android.settings.SETTINGS
python3 tools/smoke_accessibility.py --serial emulator-5600 --output results/node-decoder/lifecycle
```

The fixture script refuses physical devices, existing fixture packages and active
external automation. Setup may retry ambiguous query rejection within a bounded
15 s observation deadline; production retains the explicit rejection and 5 s
query deadline. A synchronous Binder hang still needs host process termination.
