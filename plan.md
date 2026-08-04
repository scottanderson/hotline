# Near-term goals

Working on `hotline-rs`, a Rust reimplementation of the Hotline client/server
wire protocol, cross-checked against a reference implementation for behavior
parity (see AGENTS.md: never name it in code/commits/docs, only describe
*what*/*why*). Current focus: closing feature gaps versus the reference, in
priority order (backlog below).

## Known deliberate divergences from the reference server (flagged, not fixed)

- **`GETLIST` visibility timing.** The reference makes a raw TCP connection
  visible to `GETLIST` immediately at accept, before `Login` even arrives
  (blank name/icon until set). This server only becomes visible at `Login`
  time. Low priority.
- **`UserSelfinfo`'s access field.** The reference always sends a hardcoded
  "everything enabled" fake access bitmap in this push, regardless of real
  granted privileges (enforcement is entirely server-side). This server
  sends the real computed access. Left as-is; flagging in case it turns out
  to matter for client compatibility.
- **`reply_error()` text does not match the reference's error strings.**
  The reference uses short, sometimes cryptic strings (`"who?!?"`,
  `"Uh, no."`, ...); this server uses friendlier custom text. Not changed
  yet — decide with the user whether exact-string parity matters for any
  known client before touching this everywhere.
- **Join broadcast reaches every session unconditionally.** The reference
  filters recipients by `access_extra.user_getlist`, a per-account bit
  outside the wire `USER_ACCESS` bitmap. Ties into backlog item 2 below.

## Backlog, in priority order

1. **Live account changes don't propagate to connected sessions.** The
   reference's account-modify handler immediately walks every connected
   session and overwrites the in-memory privilege bitmap (and name/color,
   broadcasting `UserChange`) for any live session logged in under the
   modified account — no reconnect needed. `on_account_modify` here only
   writes to the `AccountProvider`; a currently-connected session keeps
   its privileges from login time until it reconnects, since `Session`
   owns a private copy (per-task, unreachable from outside its own
   `Session::run`). Fixing this needs: (a) tracking which live session is
   logged in under which login (`SharedState` currently only indexes by
   `user_id`), and (b) a new control channel into a running session's
   `select!` loop to push a live privilege update — the existing
   `targeted`/`announce` channels only carry outbound `Transaction`s, not
   session-state mutations. Related: the reference also has a
   `kick_transients` config flag (default off) that force-disconnects
   live sessions when their account is deleted; we don't have an
   equivalent, but this already matches the reference's *default*
   (flag off) behavior, so it's lower priority than the modify case.
2. **`access_extra`-equivalent modeling.** The reference gates some
   behaviors (global-subject-setting, `user_getlist` broadcast filtering,
   ...) on per-account config bits outside the wire `USER_ACCESS` bitmap.
   `hotline-rs` currently reuses `AccessPrivileges::DISCONNECT_USERS` as a
   stand-in for the subject case. `user_getlist` (gates both issuing
   `HTLC_HDR_USER_GETLIST` and receiving other users' join/change
   broadcasts) is the next concrete, testable piece worth porting.
3. **File transfer (`HTXF`).** Second-port transfer handshake, not
   started. Large standalone gap — no file upload/download exists at all,
   and `SharedState::root_files` is still a hardcoded placeholder listing,
   not real storage.
4. **Threaded news.** Current news support is flat/classic only.
5. **Chat room passwords.** Extend `ChatRoom` with an optional password;
   `HTLC_HDR_CHAT_CREATE`/`HTLC_HDR_CHAT_JOIN` should accept/check it,
   matching how the invite bit already bypasses the check. Verify against
   the reference's exact join/create field names and error text before
   implementing.
6. **Remaining unhandled opcodes.** All map to an item above rather than
   being new independent work: `FileGet`/`FilePut` (202/203, item 3);
   `FileDelete`/`FileMkdir`/`FileGetinfo`/`FileSetnfo`/`FileMove`/
   `FileSymlink` (204–209, blocked on real file storage, item 3);
   `NewsListdir`/`NewsListcategory`/`NewsDelete`/`NewsMkdir`/
   `NewsMkcategory`/`NewsGetthread`/`NewsPostthread`/`NewsDeletethread`
   (370–411, item 4).

## Architecture notes worth keeping in mind

- **Accounts are pluggable, not owned by `hotline-server`.**
  `crates/hotline-server/src/accounts.rs` defines `Account` and an
  `AccountProvider` trait (`async-trait`); the embedding application
  supplies the backend to `Server::new(config, accounts)`. `hls` currently
  passes `InMemoryAccountProvider` (also `hotline-server`'s default for
  `SharedState::default()`/tests), with a `TODO` marking where a
  persistent implementation would plug in. No file I/O or storage-format
  decisions belong in `hotline-server` itself.

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
  variant name. Don't touch/expand an existing comment as a side effect of
  an unrelated change.
- When a change is large, split it into separate logical commits and
  confirm each one with the user before running `git commit` — don't batch
  everything into one commit and don't commit without explicit go-ahead.
- Only start the next roadmap item on explicit user direction — don't
  assume "continue" means the next item on this list without confirming.
