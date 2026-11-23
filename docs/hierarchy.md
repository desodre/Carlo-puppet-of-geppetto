# Hierarchy snapshot and XML contract

Status: implemented for one pinned build and executed on it (evidence in
`docs/validation/p4-hierarchy-core.md`). Host tests cover the decoders and the
traversal against synthetic parcels and a scripted transport. `serve` does not
offer the hierarchy yet.

Two facts were learned on the device and are part of the contract: a non-empty
`Bundle` is followed by one extra boolean on Android 16 (`mHasIntent`), and the
window list can be empty for a moment after registration, so an empty list is
polled for up to 2 s before it is reported as an empty screen.

Commands:

- `hierarchy-probe` prints one JSON report: profile, session, counters, limits
  and the typed error. It contains no text, description, resource id, package,
  class, window title or bounds.
- `hierarchy-dump` prints the XML document below and nothing else. The document
  **is** UI content. A snapshot that did not fully succeed prints nothing and
  exits 69.

## Profiles

A profile is selected by exact `ro.build.fingerprint`, API level, architecture
and shell UID 2000. Nothing is inferred from `SDK_INT`.

| Profile | Build | Hierarchy |
|---|---|---|
| `reference_api34` | `google/sdk_gphone64_x86_64/emu64xa:14/UE1A.230829.050/12077443:userdebug/dev-keys`, API 34, x86_64 | `unsupported_profile`: its window-enumeration contract is not pinned |
| `samsung_a16x_api36` | `samsung/a16xub/a16x:16/BP4A.251205.006/A166MUBSADZJA:user/release-keys`, API 36, aarch64 | yes |

Any other device gets `unsupported_profile` and no Binder call. The pinned
transactions and layouts are in
[compat/samsung-a16x-api36-accessibility.json](compat/samsung-a16x-api36-accessibility.json).

## Acquisition

One UI automation session per command: register (flags exactly `1`,
`FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES`, so the user's accessibility
services stay bound), wait for `init`, `getWindows`, walk, unregister. The
unregister runs on every exit path, including errors and signals; a second
session in the same process is refused; a session somebody else holds is
reported as `conflict` and never unregistered.

1. `getWindows` lists the windows of every display.
2. Each window's root is requested with `(window id, ROOT_NODE_ID)` and prefetch
   flags `DESCENDANTS_DEPTH_FIRST | UNINTERRUPTIBLE`, so one answer carries the
   requested node and up to 50 of its descendants.
3. The walk is depth first. A child that an earlier answer of the same attempt
   already delivered is taken from it; any other child is requested, again with
   prefetch. An answer only prefetches descendants of the node it was asked
   for: children cut off by the 50-node bound cost one request each.
4. Nodes are identified by `(window id, node id)`. A node reached a second time
   is emitted once, where it was first reached, and counted (`duplicates`).

Nothing is cached between snapshots. Every attempt starts from the window
list; every request has a fresh interaction id and its own callback object, so
an old answer cannot satisfy a new request. `onInterrupt` and
`clearAccessibilityCache` callbacks received during the snapshot are counted
(`invalidations`).

### Consistency

A snapshot is **not atomic**: the screen may change while it is read. The
attempt is inconsistent when

- a child listed by its parent comes back null (`child_missing`),
- a window disappears: the app answers the invalid-window sentinel or the
  system rejects the request (`window_gone`),
- a request gets no answer within its deadline, or the answer arrives after it
  (`request_timeout`).

The whole snapshot is then retried, at most 2 more times, 100 ms apart, inside
the total deadline. If the last attempt is still inconsistent the result is
`inconsistent_snapshot`. A partial tree is never returned as success.

### Limits

| Limit | Value | When reached |
|---|---|---|
| Total deadline | 10 s, all attempts | `truncated` (`deadline`) |
| One node request | 2 s | attempt inconsistent |
| Attempts | 3 | `inconsistent_snapshot` |
| Nodes, all windows | 10 000 | `truncated` (`nodes`) |
| Depth | 128 | `truncated` (`depth`) |
| XML document | 4 MiB | `truncated` (`xml_bytes`) |
| Displays / windows per display | 16 / 64 | `invalid_window_list` |
| One result callback | 512 KiB | `invalid_callback` |
| `getWindows` reply | 64 KiB | `runtime` (`invalid_reply_size`) |

The callback bound is the largest oneway transaction the kernel can deliver
(half of the ~1 MiB Binder buffer). `init` has its own 5 s wait before the
snapshot starts. Synchronous Binder calls cannot be cancelled: `getWindows` may
be held by the system for up to 5 s and the deadline is checked when it
returns, so a host timeout of at least 25 s is advisable.

### Errors

Typed, in the `error` field of the probe report (`kind`, optional `detail`):

| `kind` | Meaning |
|---|---|
| `unsupported_profile` | not the pinned hierarchy build; nothing was sent |
| `conflict` | another automation session is registered |
| `windows_unavailable` | `getWindows` answered null |
| `invalid_window_list` | the window list did not match the pinned layout; `field` and `reason` say where |
| `inconsistent_snapshot` | every attempt saw the tree change; `reason`, `attempts` |
| `truncated` | a limit was reached; `limit` names it |
| `unsupported_node` | `count` nodes could not be decoded; `class` is the first reason and `snapshot.node_failures` lists every `(kind, field, reason, count)` |
| `invalid_node_field` | a root answer named another window (`field: window_id`) |
| `invalid_callback` | a callback or reply exceeded its byte bound or was malformed outside any node |
| `invalid_tree` | a node is its own ancestor (`cycle`) |
| `cancelled`, `initialization_timeout`, `binder`, `remote_exception`, `runtime` | session failures, as in the session probes |

Empty results are not errors: zero windows gives `windows="0"`, and a window
whose root is null is counted in `empty-windows` (probe: `empty_windows`, per
display too) and emits no element.

A node whose text is a spanned `CharSequence` with spans, or that carries
extra rendering info or leash Binders, cannot be decoded (those layouts are not
pinned). A touch delegate (API 36 bit 42, pinned from the device's
framework.jar) is consumed with bounded counts and never emitted. Such a node is never emitted partially and never
dropped silently: the walk continues to count the failures and the snapshot
fails with `unsupported_node`. When an answer fails to decode, the node is
requested again alone so the failure is attributed to the right node.

## Document

```
<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy ...><node ...>...</node>...</hierarchy>
```

UTF-8, no whitespace between elements, no trailing newline. One `<node>` tree
per window that has a root, as direct children of `<hierarchy>`.

**Window order (document order):** displays by ascending display id; inside a
display, windows by descending layer (topmost first), ties by ascending window
id. Children are in the order the parent lists them.

### `<hierarchy>` attributes, in this order

| Attribute | Value |
|---|---|
| `rotation` | always `unknown`: not available without another pinned contract, never a fake `0` |
| `snapshot-id` | counter per process, starting at 1; failed snapshots consume an id |
| `start-ms`, `end-ms` | monotonic clock (`CLOCK_MONOTONIC` through `std::time::Instant`), milliseconds since this process first used that clock for a snapshot; `end-ms` is the end of acquisition, before serialization |
| `generation` | geometry generation: starts at 1 and increments when the derived display sizes differ from the previous successful snapshot of the process. Always 1 for a one-shot command |
| `windows` | window trees in the document (direct children of `<hierarchy>`) |
| `empty-windows` | enumerated windows whose root was null; they have no element. The probe's `windows` counter is the sum of both |
| `nodes` | `<node>` elements in the document |
| `displays` | space-separated `id:WIDTHxHEIGHT`, or `id:unknown` when no window of that display has a region. **Derived** from the union of the display's window regions (extent of the bounding box); not read from DisplayManager |

### `<node>` attributes, in this order

| # | Attribute | Value |
|---|---|---|
| 1 | `index` | position in the parent's child list, `0` for a root. Positions of children emitted elsewhere (duplicates) are skipped, not renumbered |
| 2 | `text` | text, `""` when absent |
| 3 | `resource-id` | view id resource name, `""` when absent |
| 4 | `class` | class name, `""` when absent |
| 5 | `package` | package name, `""` when absent |
| 6 | `content-desc` | content description, `""` when absent |
| 7 | `checkable` | `true`/`false` |
| 8 | `checked` | `true` when the boolean property is set or the tri-state value is "true"; "partial" is `false` |
| 9 | `clickable` | |
| 10 | `enabled` | |
| 11 | `focusable` | |
| 12 | `focused` | |
| 13 | `scrollable` | |
| 14 | `long-clickable` | |
| 15 | `password` | |
| 16 | `selected` | |
| 17 | `bounds` | `[left,top][right,bottom]`, bounds in screen, as the app reported them |

Window root nodes carry four more, after `bounds`: `window-id`, `display-id`,
`window-type` (the numeric `AccessibilityWindowInfo.TYPE_*` value: -1 for an
unmapped WindowManager type, or 1..7) and `window-layer`. The -1 comes from the
default branch of `AccessibilityWindowManager.updateWindowsLocked` in the device's
`services.jar`, which keeps the raw WindowManager type only in Samsung's `mRawType`.

Structural nodes without text are kept. Unlike `uiautomator dump`, nodes that
are not visible to the user are **not** filtered out.

### Escaping

| Input | Output |
|---|---|
| `&` `<` `>` `"` `'` | `&amp;` `&lt;` `&gt;` `&quot;` `&apos;` |
| TAB, LF, CR | `&#9;` `&#10;` `&#13;`, so attribute-value normalisation keeps them |
| U+0000–U+0008, U+000B, U+000C, U+000E–U+001F, U+FFFE, U+FFFF | U+FFFD, counted in `sanitized_characters` |
| everything else, emoji and astral characters included | unchanged UTF-8 |

Unpaired surrogates cannot occur: UTF-16 that contains one is rejected when the
node is decoded.

## Not provided

- **Rotation.** `rotation="unknown"`.
- **Atomicity.** Consistency is checked per attempt, not guaranteed.
- **A cache.** Every snapshot reads everything again.
- **Persistent node ids.** Node and window ids are valid inside one snapshot
  only and are not emitted as selectors.
- **Real display sizes.** `displays` is derived from window regions.
- **Invisible-node filtering**, WebView/Compose/Canvas content the app does not
  expose, and nodes behind leashed (embedded) hierarchies.
- **The hierarchy over `serve`**, and the 51-target parity run against the Java
  reference: both are later work.

## Known uncertainties

Read from dex, never executed. The probe names the failing field for each:

| Part | How a mismatch shows |
|---|---|
| Native `Region` layout (inferred from AOSP) | `invalid_window_list`, `field: region` |
| Locales encoding (inferred from AOSP) | `invalid_window_list`, `field: locales` |
| Samsung trailing `mRawType` | `invalid_window_list`, `field: raw_type`, `list_display_id` or `class_name` |
| Node field order and the three new trailing fields | `unsupported_node` with `node_failures` naming field and reason on (nearly) every node |
| Root request answered with the root first in the list, node `windowId` equal to the window list id | `invalid_node_field`, `field: window_id`, or `inconsistent_snapshot` (`child_missing`) |
| Callback envelope (list count, presence flags, interaction id) | `unsupported_node` with `node_failures` naming `list_count`, `item_presence`, `interaction_id` or `callback` |
| `getWindows` needing more than capability 1 and flag 64 | `windows_unavailable` |
| How common spanned text is on real screens | `unsupported_node`, `class: char_sequence_span`, with the count |
