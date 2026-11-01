# Working on Carlo

Read [CONTRIBUTING.md](CONTRIBUTING.md), [PLANO.md](PLANO.md) and the relevant issue.

## Scope and architecture

- Native Rust Android agent: hierarchy and video, launched as shell through ADB.
- No Java/JNI/DEX/APK fallback. Binder and capture access require versioned proofs.
- Host owns deployment, touch/text/system commands, decoding and test orchestration.
- Keep `main.rs` small, typed errors in the library and feature code independently testable.
- Isolate FFI with safe wrappers, ownership, documented thread rules and `SAFETY` comments.
- Bound queues, allocations and waits. Do not assume a synchronous Binder call is cancellable.
- `src/binder/native.rs` owns the API 34 probe C ABI. Resolve platform exports at runtime;
  add each new signature with pinned source and ownership evidence. Probe success only
  proves lookup/ping, not automation registration or hierarchy.
- Report unavailable features honestly; a successful doctor check is not Binder compatibility.
- Stdout is command data; errors/diagnostics go to stderr. Never log UI contents or secrets.

## Workflow

- Project 11 is the live board; roadmap parents #1–#11 own native subissues.
- Label type, area and priority; mark blocked only for a documented impediment.
- Work on a linked `codex/<type>/<slug>-<issue>` branch from `main`.
- Group subissues only for a cohesive deliverable and reference each implemented issue in the PR.
- Use `Closes #<child>` for accepted complete work; `Refs #<parent>` until full stage acceptance.
- A PR delivering an entire validated stage may close its parent and all children; merge is stage acceptance.
- Move implemented work to Em revisão while awaiting PR review/merge; do not claim it integrated.
- Conventional Commits in English; do not merge, tag or publish without maintainer request.
- Preserve user changes and update roadmap links/docs when architecture or workflow changes.

## Required validation

`cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`,
`cargo test --locked`, and `python3 -m unittest discover -s tools -p 'test_*.py' -v`.

For native changes run `python3 tools/build_android.py` with the pinned NDK and
record API/ABI actually executed. For Binder use the bounded `binder-probe` session
and `tools/binder_inventory.py`; never run platform calls on an unvalidated profile.
Use a disposable emulator by default; respect existing physical-device authorization and never infer permission for an unrelated device.
Record checks and evidence in the PR. Build artifacts belong in ignored `dist/`/`target/`.
