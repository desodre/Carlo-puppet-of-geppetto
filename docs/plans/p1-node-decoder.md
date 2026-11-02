# Decoder increment — issues #23 and #78

## Selection and dependency

Implement #23 (bounded AccessibilityNodeInfo decoding), and accept #78 against
actual CI coverage/results for this native increment. #75/#77 remain broader
ongoing quality work; no claim to implement their whole scope. Branch
`codex/feat/node-decoder-23` was created from main and fast-forwarded to PR #85's
base (which contains #86). Review against that branch until #85 enters main.

## Steps and acceptance

1. Pin AccessibilityNodeInfo, TextUtils and BaseBundle contracts to Android 14 r1.
   Record field mask/order, UTF-8 CharSequence vs UTF-16 Strings, nulls and bounds.
2. Copy at most 64 KiB of a callback's argument data through public NDK
   AParcel_marshal; reject Binder/FD-bearing parcels. Decode in safe Rust with
   checked offsets, bounded strings/collections and explicit unsupported layouts.
   Never persist raw runtime parcels or log UI text. Unsupported fields/spans must
   fail explicitly rather than silently misalign subsequent fields.
3. Integrate complete callback consumption and ID correlation. Keep query metadata
   output; expose decoded root only via an explicit data-output probe. Hierarchy/XML
   capability remains unavailable until P4.
4. Add synthetic parser coverage (every truncation, Unicode/null, unknown masks,
   invalid lengths/bounds, unsupported spans/optional layouts), native parcel
   interop and a controlled test APK whose root text changes after an ADB tap.
   The APK is solely the app under test; Carlo remains Rust with no Java bridge.
5. Execute native session/query lifecycle regression, fixture before/after text,
   cleanup and both ABI cross-builds. Preserve redacted evidence and limitations.
6. Verify #78's existing CI checks and artifacts on the PR, document commands and
   coverage; move completed issues to review, never merge automatically.

## Initial supported subset

Core fields, IDs, children, bounds, actions and plain CharSequence are the minimum.
Optional complex layouts will be handled only with verified boundaries or rejected
with typed errors. Complete general-purpose node compatibility is not assumed.
