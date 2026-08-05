# Near-term goals

Working on this Rust reimplementation of the Hotline client/server
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
- ~~Join broadcast reaches every session unconditionally.~~ Fixed, and
  generalized: `Session::may_receive` is now a small opcode-to-required-
  privilege table checked in both the `broadcast_rx` and `announce_rx`
  arms of `Session::run`'s `select!` loop, covering `UserChange`/
  `UserPart` (`AccessExtra::USER_GETLIST`), public `Chat`
  (`AccessPrivileges::READ_CHAT`), and `NewsfilePost`
  (`AccessPrivileges::READ_NEWS` — `on_newsfile_post` now does a single
  `state.publish` instead of a manual per-user `send_to` loop). Room-
  scoped chat is untouched (delivered via `targeted_rx`/room membership,
  never `READ_CHAT`), matching the reference's own split between its
  public-chat and room-chat send paths.
- ~~Cross-channel delivery ordering isn't guaranteed for a single
  recipient.~~ Fixed: see backlog item 2.

## Backlog, in priority order

1. ~~`access_extra`-equivalent modeling.~~ Done. `AccessExtra`
   (`accounts.rs`) models the reference's `extra_access_bits` struct as
   its own bitflags type; `USER_GETLIST` gates both issuing
   `HTLC_HDR_USER_GETLIST` and, as of `Session::may_receive`, receiving
   `UserChange`/`UserPart` pushes, and `SET_SUBJECT` gates setting the
   global chat room's subject. Both are set at login from
   `Account::access_extra`.
2. ~~Unify per-recipient delivery into one ordered channel.~~ Done.
   `SharedState`'s four broadcast channels
   (`broadcast`/`targeted`/`announcements`/`access_updates`) are now one
   `HashMap<u16, mpsc::Sender<RecipientMessage>>`, `RecipientMessage`
   being `Push(Transaction) | AccessUpdate(AccessUpdate)`; `publish`/
   `send_to`/`announce`/`push_access_update` all fan out through it, so
   `Session::run` collapses to a single inbound `select!` arm per
   connection — every recipient now sees everything in true send order.
   Privilege/login filtering deliberately stayed on the session side
   (`Session::may_receive`, the login check in `apply_access_update`)
   rather than moving into `SharedState`: an earlier version centralized
   it there (`SharedState::recipient_may_receive`, looking up each
   recipient's privileges from the shared `users` table at fan-out time),
   but that traded an O(1) lock-free broadcast for an O(N) sequential
   locked lookup per recipient, borne synchronously by whichever session
   was sending — a real latency/contention regression under load, for no
   benefit the channel unification actually required. Reverted in favor
   of keeping filtering free (a plain bitflag check against fields the
   receiving session already holds in memory). Deliberately out of scope:
   ordering between two genuinely independent concurrent clients' actions
   arriving at a third client — only per-recipient ordering of a single
   server-side send sequence is guaranteed, matching what the reference
   actually relies on.
3. **File transfer (`HTXF`).** Second-port transfer handshake, not
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
