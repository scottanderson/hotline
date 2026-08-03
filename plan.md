# Near-term goals

Status snapshot before a context clear. The `SessionState`/login-phase
refactor described below is done and committed; a real desync bug against
the official Hotline 1.2.3 client is still open and is the active focus.

## Recently finished (this session)

- **`SessionState` machine.** `Session` now tracks `Agreement` vs `Joined`
  and gates opcodes on it (`handle_agreement`/`handle_joined` in
  `session.rs`): only `Login`/`Agreed`/`Ping` are accepted before login
  completes, everything else gets `"please finish logging in first"`.
- **Fixed the `got_name`/pre-150 immediate-join bug.** Verified against the
  reference `rcv_login`: a client that embeds its name in `HTLC_HDR_LOGIN`
  (107), or one reporting `HTLC_DATA_CLIENTVERSION` (160) below 150 (or
  omitting it entirely), completes the join *synchronously within
  `on_login`* — no `HTLC_HDR_AGREED` (121) is ever expected from it. This
  server previously always deferred to `Agreed` regardless, so the real
  Hotline 1.2.3 client (which sends its name in `Login`) got stuck: its own
  follow-up requests (e.g. `GETLIST`) were rejected once the `SessionState`
  gating landed, since the session was still stuck in the `Agreement`
  phase. Pre-150 clients also don't get the `Noagreement` marker push
  (only real agreement text, if configured) — matches the reference's
  version gating there too. `on_login`/`on_agreed` share a new
  `complete_join` helper. New `Transaction::client_version()` accessor.
- **`UserSelfinfo` carries the joiner's own `UserList` entry.** The join
  broadcast excludes the joiner itself, so without this a joiner never
  learned its own uid/icon/name after logging in.
- **`hltui` client is tolerant of any push in any `Mode`.** Only the
  *server* enforces session-phase opcode restrictions; the client always
  runs the same `apply_server_event` dispatch regardless of whether it's
  still showing the agreement screen, so a push that arrives early (e.g.
  another user's `UserChange`) isn't silently dropped.
- Added an AGENTS.md rule: don't touch/expand an existing comment as a side
  effect of an unrelated code change.

## Active bug: hl123 desync (not yet root-caused)

Reported against the real Hotline 1.2.3 client, reproducible:

1. Connect, send chat "a".
2. Post a news article — the news window shows the chat text instead of
   the real post.
3. Post a second article — the news window now shows the chat text
   *below* the real post.
4. Reconnect: the news is correct (just the two real posts). So the
   server's actual stored news state is fine; this is a **client-side
   reply/push misrouting** problem, not server-side data corruption.
5. Separately: log in, then immediately request `GetInfo` on yourself —
   that request hangs forever, but sending a *second* `GetInfo` completes
   both. Smells like a client-side reply-matching bug triggered by
   something we're sending it.

Leading hypothesis (unconfirmed): the client may be sensitive to
unsolicited pushes (chat broadcast, news-post broadcast, join
announcements, ...) arriving interleaved with the reply to its own
in-flight request, if it does simplistic ordering-based reply matching
rather than strict transaction-ID matching. `Session::run`'s
`tokio::select!` reads a client's own next request concurrently with
broadcast/targeted/announce channel messages, so a self-broadcast (e.g.
this session's own chat echo, or a news-post notification) could in
principle be written to the socket before or after a reply in a different
order than the reference server would produce, if fairness between
`tokio::select!` branches doesn't match the reference's fully-synchronous
per-transaction processing model.

Ruled out so far: the `TASK_REPLY` sentinel value (`0x0001_0000`) matches
the reference's `HTLS_HDR_TASK` exactly, so it's not a reply-vs-push
opcode encoding mismatch.

Next steps: trace exactly how the reference server orders a reply to the
sender against a broadcast triggered by that same request (does it always
flush the direct reply before touching any other socket, including its
own?), then check whether our `tokio::select!` loop can produce a
different order for this session's own socket specifically. Ask before
changing `Session::run`'s concurrency model — this is architecturally
sensitive.

## Known deliberate divergences from the reference server (flagged, not fixed)

- **`GETLIST` visibility timing.** The reference makes a raw TCP
  connection visible to `GETLIST` immediately at accept, before `Login`
  even arrives (blank name/icon until set). This server only becomes
  visible at `Login` time — an improvement over "only after `Agreed`" but
  not an exact match. Low priority.
- **`UserSelfinfo`'s access field.** The reference always sends a
  hardcoded "everything enabled" fake access bitmap in this push,
  regardless of real granted privileges (enforcement is entirely
  server-side). This server sends the real computed access. Left as-is;
  wasn't obviously worth replicating, but flagging in case it turns out to
  matter for client compatibility.
- **`reply_error()` text does not match the reference's error strings.**
  Confirmed by direct comparison: the reference uses short, sometimes
  cryptic strings (`"Transaction rejected. (Unknown or non-authorised)"`,
  `"who?!?"`, `"huh?!?"`, `"Uh, no."`, `"%s cannot be disconnected"`,
  `"Cannot kick the specified client because it does not exist."`, ...)
  where this server uses friendlier custom text (`"missing user id"`,
  `"no such chat room"`, `"that user can't be disconnected"`, ...). Not
  changed yet — decide with the user whether exact-string parity matters
  for any known client before touching this everywhere.
- **Join broadcast reaches every session unconditionally.** The reference
  filters recipients by `access_extra.user_getlist`, a per-account bit
  outside the wire `USER_ACCESS` bitmap that this server doesn't model at
  all (ties into the `access_extra`-equivalent item below).

## Backlog (older, not started, lower priority than the active bug)

1. **Chat room passwords.** Extend `ChatRoom` with an optional password;
   `HTLC_HDR_CHAT_CREATE`/`HTLC_HDR_CHAT_JOIN` should accept/check it,
   matching how the invite bit already bypasses the check. Verify against
   the reference implementation's exact join/create field names and error
   text before implementing.
2. **`access_extra`-equivalent modeling.** The reference gates some
   behaviors (global-subject-setting, `user_getlist` broadcast filtering,
   ...) on per-account config bits outside the wire `USER_ACCESS` bitmap.
   `hotline-rs` currently reuses `AccessPrivileges::DISCONNECT_USERS` as a
   stand-in for the subject case since there's no accounts subsystem yet.
3. **File transfer (`HTXF`).** Second-port transfer handshake, not started.
4. **Accounts subsystem.** Needed for real per-account privilege bits and
   for anything login-related beyond guest.
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
  variant name. Don't touch/expand an existing comment as a side effect of
  an unrelated change.
- When a change is large, split it into separate logical commits and
  confirm each one with the user before running `git commit` — don't batch
  everything into one commit and don't commit without explicit go-ahead.
- Only start the next roadmap item on explicit user direction — don't
  assume "continue" means the next item on this list without confirming.
