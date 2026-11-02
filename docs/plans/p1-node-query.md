# P1.5 — Single-node query and correlated callback (#22)

## Scope

Issue #22 implements the request/result transport. #23 owns decoding and validation
of AccessibilityNodeInfo fields. This increment reports only a bounded callback
envelope (presence, opaque payload byte count, interaction ID), never a usable node,
UI contents, XML, or hierarchy capability. A non-null envelope does not prove valid
node fields or changing text; those remain #23 and the P1 acceptance milestone.

## Dependency and branch

PR #85 remains open. Branch `codex/feat/node-query-22` was created from main and
carries its session commit as a prerequisite. Review this increment against #85's
branch; integrate #85 first. Do not merge either PR automatically.

## Implementation sequence

1. Pin query transaction 3 and single-result callback 1 to the existing Android
   14 r1 source revision. Record request order, root/window IDs, and callback origin.
2. Add one synchronous query at a time per session, monotonic interaction IDs,
   deadline starting before dispatch, a distinct Binder callback for each request,
   cancellation, null result and rejected-request outcomes.
3. Bound callback parcels to 64 KiB. Validate nullable presence and trailing ID
   from the pinned AIDL envelope without copying or interpreting the opaque node.
   Never reuse opaque bytes as a decoded node; #23 must replace this observation
   boundary with full field decoding. Reject malformed envelopes, stale IDs,
   duplicates, expired callbacks and callbacks after close.
4. Add `node-query-probe`: register, query active-window root, report metadata,
   unregister. Keep `hierarchy_available=false`. Preserve session-only probe.
5. Test pure correlation/timeouts and malformed envelopes; run native parcels,
   real root/null/rejection queries, repeated queries/resource samples and lifecycle
   regressions on the disposable API 34 x86_64 reference emulator. Cross-build both ABIs.
6. Record evidence, update docs/CHANGELOG and board; open a dependent PR for #22.

## Limits and ownership

Callback capability is passed only to the accessibility service; Android may forward
it to the target app (not exclusively UID 1000). Each callback owns isolated state,
so a late/malformed response cannot complete another request. No queues, UI payload
allocations, or detached workers. Synchronous Binder dispatch still needs the host
process deadline as fallback. Unsupported profiles fail before private transactions.
