# Extension UI Implementation Plan

> Execute in this session; user approved the design and explicitly requested implementation.

**Goal:** Ship v2 declarative extension UI with keyboard interaction and process callbacks.
**Architecture:** Versioned protocol types, scoped runtime delivery, independent session UI controller, Ratatui rendering.
**Tech Stack:** Rust, serde, Tokio, Ratatui; Python standard-library example.
**Spec:** ../specs/2026-09-20-extension-ui-design.md

## Global Constraints

Keep v1 wire behavior. No new runtime dependencies. UI never enters LLM context. Host owns source identity, revisions and lifecycle. Main TUI only; no child or /btw UI. Preserve permission priority and composer drafts.

## Review Focus

Stale callbacks after cancel/reset; malformed batches and forged events; UI while model owns Agent mutex; narrow terminals and keyboard focus; compatibility for v1 and no-UI callers.

## Tasks

- [x] Protocol/runtime: add serde tests that deserialize a v2 UI response and reject malformed/oversized components; run `cargo test -p koala-extension-api` RED then GREEN. Add ui.rs types/validation; add runtime scoped request context and hosted response validation; prove v1 and v2 process requests with `cargo test -p koala-extensions`.
- [x] Session controller: add tests that apply mount output, route select/input/confirm, reject stale/forged events, survive callback cancellation, and keep state isolated; implement extension_ui.rs and SessionCommand/UiEvent routing; run `cargo test --lib extension_ui`.
- [x] TUI: add TestBackend and real key event tests for widgets, dialogs, input/paste, permissions and narrow terminals. Implement extension_ui.rs, /extensions command and layout integration. Run `cargo test --lib tui`.
- [x] Example/docs and integration: add Python example plus end-to-end process tests; update README. Run `cargo fmt --all -- --check`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -D warnings`. Review complete diff and fix material findings before final report.

## Execution notes

Implementation authorized in the current workspace; no commits or worktree changes are needed to deliver the requested code. Tests will record actual failure and success evidence in the task conversation.

## Review and regression evidence

Independent review completed via requesting-code-review. Important findings were reproduced by failing tests before fixes:

- `invalid_tool_response_never_publishes_ui`: reject missing tool content before publishing UI.
- `callback_ack_preserves_unsent_input_and_focus`: separate interaction revision from component content revision.
- `cancel_reaches_extension_host_even_without_focused_ui_or_model_turn`: idle Ctrl+C reaches the UI host.
- `long_input_scrolling_keeps_caret_visible`: follow the actual caret row in narrow terminals (minor issue also fixed).
- Additional self-review: `old_session_action_cannot_target_new_session_with_same_component_ids` reproduced stale revision reuse; revisions now never repeat across sessions in the process.

All these regressions passed after their fixes. Real-process tests cover Python mount → select → input → confirmation/cancellation; session tests prove callbacks remain responsive while the Agent mutex is held. No review findings were deferred.

Concurrent unrelated work added image/clipboard and message queue support while implementation was underway. Those changes are preserved. Workspace-wide verification results must distinguish concurrent unfinished tests from this feature; no claim of all tests passing is made without a completed current run.

## Verification results

- `cargo test --workspace`: PASS on the final combined workspace run (`/tmp/koala-ui-latest.log`), including 227 main-library tests and every workspace integration/doc-test target.
- `cargo test -p koala-extension-api -p koala-extensions`: PASS, 10 tests (including existing runtime tests).
- `cargo clippy -p koala-extension-api -p koala-extensions --all-targets -- -D warnings`: PASS.
- `cargo fmt --all -- --check`: PASS after formatting the combined workspace.
- `git diff --check`: PASS.
- Workspace-wide strict Clippy was attempted but did not pass: pre-existing/concurrent diagnostics included derivable `PermissionsConfig::default`, a large `Panel` enum variant, and a nested journal-loading conditional. These are outside the extension feature; the protocol/runtime targeted check above is clean.

Earlier full-suite attempts were interrupted by sandbox restrictions on local test servers and by concurrent image/queue/status edits, including temporarily missing functions and new failing tests. Those compilation/test failures were resolved by the final full workspace run; they are not outstanding test failures.

No review findings were deferred. No user changes were reverted, and no commit, installation into the user's config, or external publication was performed.
