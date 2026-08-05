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
  filters *recipients* of join/part/change traffic by their own
  `access_extra.user_getlist`. `hotline-rs` now gates *issuing*
  `HTLC_HDR_USER_GETLIST` on `AccessExtra::USER_GETLIST`
  (`accounts.rs`/`session.rs`), but does not yet filter who *receives*
  `UserChange`/`UserPart` pushes — every connected session still gets
  them regardless of its own `access_extra`. Ties into backlog item 1
  below.

## Backlog, in priority order

1. **`access_extra`-equivalent modeling.** The reference gates some
   behaviors (global-subject-setting, `user_getlist` broadcast filtering,
   ...) on per-account config bits outside the wire `USER_ACCESS` bitmap.
   `AccessExtra` (`accounts.rs`) models this as its own bitflags type,
   extensible one bit at a time; every bit from the reference's
   `extra_access_bits` struct is named there, though only two have
   behavior wired up so far: `USER_GETLIST` (gates issuing
   `HTLC_HDR_USER_GETLIST`) and `SET_SUBJECT` (gates setting the global
   chat room's subject, `HTLC_HDR_CHAT_SUBJECT` with chat ID 0 —
   replaces the old `DISCONNECT_USERS` stand-in). Both are set at login
   from `Account::access_extra`. Still open: filtering *recipients* of
   join/part/change broadcasts by their own `USER_GETLIST` (see the
   divergence above) — this needs a recipient-side check in
   `Session::run`'s `select!` loop, which the user has pushed back on in
   the past for the issuance-only case, so confirm the approach before
   implementing.
2. **File transfer (`HTXF`).** Second-port transfer handshake, not
   started. Large standalone gap — no file upload/download exists at all,
   and `SharedState::root_files` is still a hardcoded placeholder listing,
   not real storage. Protocol researched; scoped first slice proposed to
   and not yet confirmed by the user:
   - **In scope**: a second `TcpListener` on `port + 1`; `HTLC_HDR_FILE_GET`/
     `_PUT` (202/203) on the main connection allocate an opaque `u32` ref
     and record a pending transfer (path, direction, size for PUT) keyed by
     that ref + the client's IP; the second-connection handshake is a fixed
     16-byte header (`"HTXF"` magic = `0x48545846`, `ref: u32`, `len: u32`,
     `type: u16`, 2 reserved bytes), matched against the pending entry; data
     is framed in the reference's "FILP" container — `"INFO"` fork (zeroed
     type/creator, no real HFS metadata) then `"DATA"` fork with raw bytes,
     skipping the resource fork entirely. Real file storage backing
     `SharedState::root_files` via a `--files-dir` flag, mirroring the
     `--accounts-dir` pattern `FileAccountProvider` already uses.
   - **Explicitly deferred** (own follow-up items, not silently dropped):
     resume support (client-sent `RFLT` blob with `data_pos`/`rsrc_pos`
     offsets), the transfer queue (`HTLS_DATA_QUEUE_POSITION`,
     `HTLS_HDR_QUEUE_UPDATE`), folder transfers (`HTLC_HDR_FILE_GETFOLDER`/
     `_PUTFOLDER`, a separate `FILE_NEXT`/`FILE_SEND`/`FILE_RESUME` command
     loop on the transfer connection), and resource-fork (`"MACR"`) data.
3. **Threaded news.** Current news support is flat/classic only.
4. **Chat room passwords.** Extend `ChatRoom` with an optional password;
   `HTLC_HDR_CHAT_CREATE`/`HTLC_HDR_CHAT_JOIN` should accept/check it,
   matching how the invite bit already bypasses the check. Verify against
   the reference's exact join/create field names and error text before
   implementing.
5. **Remaining unhandled opcodes.** All map to an item above rather than
   being new independent work: `FileGet`/`FilePut` (202/203, item 2);
   `FileDelete`/`FileMkdir`/`FileGetinfo`/`FileSetnfo`/`FileMove`/
   `FileSymlink` (204–209, blocked on real file storage, item 2);
   `NewsListdir`/`NewsListcategory`/`NewsDelete`/`NewsMkdir`/
   `NewsMkcategory`/`NewsGetthread`/`NewsPostthread`/`NewsDeletethread`
   (370–411, item 3).

## Needs manual verification against the real client

Unlike the `TCP_NODELAY`/push-`trans` fixes (explicitly verified against
the real Hotline 1.2.3 client), the accounts subsystem (named-account
login, account CRUD opcodes, the seeded `admin`/`admin` and passwordless
`guest` both providers start with) and the idle-to-away status broadcast
have only been unit-tested plus one manual `hlc` smoke test (login
accepted with the seeded admin account, rejected on wrong
password/unknown login) — not yet exercised against a real client.

See AGENTS.md for durable working conventions (verification loop, commit
hygiene, rustdoc style, reference-naming rules); this file only tracks
current status and open work.
