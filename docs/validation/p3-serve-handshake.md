# Control channel: socket, peer credentials and handshake — #31 / #32

Date: 2026-11-21. Branch `codex/feat/serve-handshake-31`. Contract:
`docs/protocol.md`.

## Result and scope

`carlo serve --session-dir <dir>` now runs a real control channel: one client,
peer credentials checked before any byte is read, a 128-bit session identifier
required in `hello`, and a handshake returning protocol version, binary
version, API/ABI and capabilities. `ping`, `capabilities` and `shutdown` work;
data commands answer `unavailable`. Capabilities come from the doctor source and
are all `false`, including `serve_protocol`. No second channel, hierarchy or
video command exists yet (#30, #35 and later stages).

## Transport finding (#31)

The preferred socket file cannot be used on the executed build. In a 0700
directory owned by `shell` (`u:object_r:shell_data_file:s0`), `bind` returned
`EACCES`. The cause is taken to be SELinux (the shell domain may not create a
socket file there); `shell` cannot read the audit log, so that is inferred from
the errno, not observed.

`serve` therefore keeps the socket file as first choice and falls back to an
abstract-namespace socket `carlo-<32 hex>` only when bind is denied. An abstract
socket has no file permissions, so access control there is the peer UID check,
the unguessable name, the single accepted client and the session identifier.

## Device run

One authorized physical device, API 36, arm64-v8a, UID shell, SELinux enforcing.

```sh
python3 tools/build_android.py
python3 tools/smoke_serve.py --serial <serial> --allow-physical \
  --manifest dist/manifest.json --output <dir>
```

Three consecutive runs passed. Each run uses three fresh sessions, exposed with
`adb forward tcp:0 localabstract:<name>`:

| Case | Result | Agent exit |
|---|---|---|
| complete | `hello` ok, 4 capabilities with 0 available, 5 reserved commands `unavailable`, unknown frame kind rejected and connection recovered, 12 requests, `shutdown` | 0 |
| wrong session | `invalid_session`, connection closed | 74 |
| bad magic | `bad_magic` rejection, connection closed | 74 |

In every case the listener was gone after the client was accepted, the port
forward was removed, no `control.sock` existed and the session directory was
removed. No forward, session directory or `carlo serve` process was left on the
device. The session identifier is not stored; only its length (32) is.

## Checks

`cargo fmt --all -- --check`; Clippy on host and both Android targets;
`cargo test --locked` (68 library, 10 CLI, 13 serve) in three consecutive runs;
54 Python tests; `build_android.py` for both ABIs. All passed.

This branch also restores `pub mod protocol;` in `src/lib.rs`. A merge on `main`
had dropped it, so the framing module of #33/#34 and its tests were not being
compiled.

## Limits

- The filesystem transport is exercised on the host only; `localfilesystem:`
  forwarding is unproven on any device.
- Not executed on the API 34 reference emulator or on a build where `adbd` runs
  as root (root is accepted as peer on Android only).
- A peer with a different UID was not produced; the rule is unit-tested as a
  pure function.
- The 30 s accept/handshake/idle and 10 s write deadlines were not waited for on
  the device; they are tested on the host with short durations.
- The session identifier is visible to whoever reads the agent's stdout.
