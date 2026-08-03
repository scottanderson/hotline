# Near-term goals

Status snapshot before a context clear. Chat room 0 (global room) and
chat-invite tracking, plus the agreement/`Agreed` login flow (see below),
are done and committed. Room passwords were explicitly deferred.

## Recently finished

- **Agreement / `HTLC_HDR_AGREED` login flow.** `ServerConfig::agreement`
  (default `"hotline-rs"`) drives an `HTLS_HDR_AGREEMENT` (109) push sent
  right after the `Login` reply: agreement text if set, a `Noagreement`
  marker if empty. The join broadcast and `UserSelfinfo` push now happen
  in a new `Session::on_agreed`, gated on the client sending
  `HTLC_HDR_AGREED` (121), not in `on_login` itself. `HotlineClient::login`
  no longer sends name/icon or joins automatically; callers call the new
  `HotlineClient::agree(nick, icon)` after handling the Agreement push.
  `hlc` prompts on stdin only when text is present; `hltui` gained an
  `App::Mode::Agreement` screen with the same auto-skip behavior.

## Next up (in rough priority order, none started)

1. **Chat room passwords.** Extend `ChatRoom` with an optional password;
   `HTLC_HDR_CHAT_CREATE`/`HTLC_HDR_CHAT_JOIN` should accept/check it,
   matching how the invite bit already bypasses the check. Verify against
   the reference implementation's exact join/create field names and error
   text before implementing (see AGENTS.md: ask rather than guess when a
   behavior isn't obvious from existing code).
2. **`access_extra`-equivalent modeling.** The reference implementation
   gates some behaviors (e.g. would-be global-subject-setting) on a
   per-account config bit outside the wire `USER_ACCESS` bitmap. Right now
   `hotline-rs` reuses `AccessPrivileges::DISCONNECT_USERS` as a stand-in
   since there's no accounts subsystem yet. Once accounts exist, revisit
   whether this stand-in should become a real separate bit.
3. **File transfer (`HTXF`).** Second-port transfer handshake, not started.
4. **Accounts subsystem.** Needed for real per-account privilege bits
   (see #2) and for anything login-related beyond guest.
5. **Threaded news.** Current news support is flat/classic only.
6. **Remaining small independent opcodes.** Not yet enumerated in detail;
   worth a fresh pass over `ClientOpcode`/`ServerOpcode` to find what's
   still unhandled in `session.rs`'s dispatch.

## Working conventions to remember

- Never mention the reference implementation by name anywhere in code,
  comments, commit messages, or PR text — describe *what* the behavior is
  and *why*.
- Ask rather than guess when a behavior isn't obvious from existing code.
- Verification loop before calling anything done: `cargo build --workspace`,
  `cargo test --workspace`, `cargo clippy --workspace`, `cargo fmt --all`.
- Rustdoc comments: terse, a line or less, no rationale chains or trailing
  justification clauses. Cite wire constants as `HTLC_HDR_*`/`HTLS_HDR_*`/
  `HTLC_DATA_*`/`HTLS_DATA_*` with their decimal value, not the Rust enum
  variant name.
- Only start the next roadmap item on explicit user direction — don't
  assume "continue" means the next item on this list without confirming.
