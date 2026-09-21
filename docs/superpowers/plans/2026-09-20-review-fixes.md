# Code review fixes

**Goal:** Fix the five reported runtime defects, remove two redundant implementations, and pass strict Clippy.

**Architecture:** Keep the existing session/extension boundaries. Reuse composer cancellation state, distinguish failed confirmation callbacks from successful completion, and bound shell capture while continuing to drain both pipes.

**Constraints:** Preserve existing uncommitted work. Make changes in the reviewed workspace; no commit or publication is required. Keep foreground/background default timeouts at 30/3600 seconds. Retain at most 1 MiB per output stream and mark truncation explicitly.

## Steps

- [x] Add regression tests for extension Ctrl+C racing with Done; use the existing queue test channel and verify no new Submit before or after Cancelled.
- [x] Add a read regression with an oversized skipped line, then use bounded buffer skipping before collecting requested lines; preserve EOF and requested-line limits.
- [x] Add background timeout and simultaneous large stdout/stderr regressions; honor explicit timeout and drain pipes concurrently into bounded captures.
- [x] Add a failing-confirmation integration test through the real extension host; failed confirmation keeps its surface enabled with a fresh revision, successful retry closes it.
- [x] Remove duplicate hook response validation and unused compaction functions/tests; derive PermissionsConfig default, collapse the nested condition, and box the graph panel.
- [x] Update shell output documentation, run formatting, workspace tests, strict Clippy, and review the final changes.

## Review focus

- Done arriving before Cancelled must not resume queued input.
- Oversized skipped lines and EOF without newline must preserve line numbering.
- Full stdout and stderr must be drained concurrently even after capture fills.
- Cancellation and timeout must still terminate process descendants.
- Failed confirmation must allow explicit retry without accepting an old event.

## Execution record

Prior review reproduced cancellation, offset reading, and ignored timeout in temporary probes. Existing workspace tests: 283 passed. Clippy: three findings. Implementation proceeds directly under the user's instruction to fix all findings.

Focused verification: all five new regression tests failed against the previous behavior and passed after fixes. Existing shell cancellation/timeout descendant tests and extension integration tests also passed. Formatting and strict workspace Clippy passed. Full workspace regression: 286 passed, zero failures. Independent read-only review found no high-confidence regressions or missing fixes. `cargo fmt --all --check`, `git diff --check`, and `cargo clippy --workspace --all-targets -- -D warnings` all passed.
