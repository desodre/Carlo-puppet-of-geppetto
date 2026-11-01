# Changelog

## Unreleased

- Add the reference API 34 x86_64 accessibility session proof: versioned contracts,
  bounded Binder callbacks/pool, minimal ServiceInfo registration and connection init.
- Add idempotent unregister, safe conflict refusal, SIGINT/SIGTERM cleanup and
  forced-death recovery validation. Keep hierarchy explicitly unavailable.
- Add Android-native malformed-parcel/resource-cycle tests and reproducible
  lifecycle smoke tooling; build the reference test harness in CI.

- Add API 34 shell `binder-probe`: dynamically resolve four C Binder exports,
  lookup/ping accessibility, test a null control lookup and release strong refs
  before unloading the library. Report unsupported targets and failures as JSON.
- Add reproducible system/public-NDK symbol inventory and P1.1 source/decision
  evidence; keep hierarchy unavailable until registration and parcel proofs pass.

- Add issue lifecycle policy, labels, native parent/subissue tracking, linked-branch
  workflow, issue/PR templates, CI and parent acceptance automation.
- Add the initial `carlo` CLI: help, version and native environment diagnostics in
  text/JSON, typed errors and documented exit codes.
- Add pinned Rust 1.96.0/NDK r28c builds for x86_64 and arm64, ELF alignment checks,
  checksums and artifact manifests.
- Add bounded deployment into private Android session directories with checksum/ABI
  checks, physical-device opt-in and cleanup on success, error and timeout.
- Keep Binder hierarchy, screen capture, streaming and serve protocol explicitly
  unavailable pending their respective roadmap stages.
