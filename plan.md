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
  sends the real computed access instead. Confirmed to matter: real clients
  don't display their own permissions correctly when sent the real bitmap
  instead of the reference's hardcoded one — see backlog item 4.
- **`reply_error()` text does not match the reference's error strings.**
  The reference uses short, sometimes cryptic strings (`"who?!?"`,
  `"Uh, no."`, ...); this server uses friendlier custom text. Not changed
  yet — decide with the user whether exact-string parity matters for any
  known client before touching this everywhere.

## Backlog, in priority order

1. ~~`access_extra`-equivalent modeling.~~ Done — see `AccessExtra` in
   `accounts.rs`.
2. ~~Unify per-recipient delivery into one ordered channel.~~ Done — see
   AGENTS.md's "Server session/state split".
3. ~~File transfer (`HTXF`), single-file slice.~~ Done — a second
   `TcpListener` on `port + 1` (`hotline_server::server::run_htxf_listener`);
   `HTLC_HDR_FILE_GET`/`_PUT` (202/203) allocate an opaque `u32` ref and
   record a pending transfer (login, dir, name, size), matched on the data
   connection by that ref plus the client's IP; the handshake is the
   16-byte `"HTXF"` header (`hotline_proto::htxf::HtxfHeader`); data is
   framed in the "FILP" container (`hotline_proto::htxf`) — an `INFO`
   fork (zeroed type/creator, no real HFS metadata) then a `DATA` fork
   with raw bytes, no resource fork; confirmed byte-for-byte against
   `HLProtocol.md`'s "Flattened File Object" section (see below).
   File storage goes through `hotline_server::FileProvider`
   (`list`/`size`/`exists`/`open_read`/`create_write`, each call carrying
   a `FileRequester { login }`), on the same plug-in shape as
   `AccountProvider`: `hotline-server` only ships `InMemoryFileProvider`
   (default/tests); `hls` plugs in a real `DirectoryFileProvider`
   (`--files-dir`, mirroring `--accounts-dir`, defined in
   `hotline-daemon`, not `hotline-server`). The interface supports
   per-login file visibility (an alternate daemon could show different
   logins different files), but `DirectoryFileProvider` itself doesn't
   use that yet — it serves the same directory to everyone. Verified
   with a manual round-trip (download + upload) against a running
   `hls`/`hotline-client`; not yet exercised against a real Hotline
   client.
   - **Explicitly deferred** (own follow-up items, not silently dropped):
     resume support (client-sent `RFLT` blob with `data_pos`/`rsrc_pos`
     offsets), the transfer queue (`HTLS_DATA_QUEUE_POSITION`,
     `HTLS_HDR_QUEUE_UPDATE`), folder transfers (`HTLC_HDR_FILE_GETFOLDER`/
     `_PUTFOLDER`, a separate `FILE_NEXT`/`FILE_SEND`/`FILE_RESUME` command
     loop on the transfer connection), and resource-fork (`"MACR"`) data.
4. **Fix `UserSelfinfo`'s access field to match the reference's hardcoded
   bitmap.** Confirmed real-client-visible bug: sending the real computed
   `AccessPrivileges` bitmap (`self.access.bits()` in
   `Session::complete_join`) instead of the reference's hardcoded
   "everything enabled" value means real Hotline clients don't display
   their own permissions correctly. Enforcement is entirely server-side
   regardless of what this field says, so sending the reference's
   hardcoded value should be safe. See the divergence note above.
5. **Threaded news.** Current news support is flat/classic only.
6. ~~Chat room passwords.~~ Done — `ChatRoom.password`, set via
   `HTLC_HDR_CHAT_SUBJECT` and checked on `HTLC_HDR_CHAT_JOIN`. A pending
   invite always admits regardless of password; without one, a room with
   no password can't be joined at all — a password is the only other way
   in, not a bypass for skipping invitation (`SharedState::join_chat_room`
   now returns `Result<(), &'static str>` folding that check into one
   atomic operation). Also closed a real gap where `on_chat_create` never
   recorded its own invitee via `state.invited()`, so a later password on
   that room would have locked out someone validly invited to it.
7. **Remaining unhandled opcodes.** Mostly map to an item above rather
   than being new independent work: `FileDelete`/`FileMkdir`/
   `FileGetinfo`/`FileSetinfo`/`FileMove`/`FileSymlink` (204–209, now
   unblocked by item 3's real file storage, but each still its own
   handler); `NewsListdir`/`NewsListcategory`/`NewsDelete`/`NewsMkdir`/
   `NewsMkcategory`/`NewsGetthread`/`NewsPostthread`/`NewsDeletethread`
   (370–411, item 5).
8. **Revisit `const fn` lost from `ClientOpcode`/`ServerOpcode`'s
   `From`/`Into` conversions.** Adopting `num_enum` (`FromPrimitive`/
   `IntoPrimitive`, `#[num_enum(catch_all)]` for `Unknown(u32)`) to
   generate these from real enum discriminants dropped their `const fn`
   — `From`/`Into` aren't const-stable, so this also cascaded to
   `TransactionType::from_client_wire`/`from_server_wire` and
   `TransactionHeader::decode_from_client`/`decode_from_server`. Nothing
   currently calls any of these from a real `const`/`static` binding, so
   it costs nothing today, but it's a real departure from this crate's
   otherwise consistently `const fn` style (`PacketReader`,
   `numbers::encode`/`decode`) worth deciding on deliberately rather than
   leaving as a side effect. Low priority, no known concrete need yet.
9. **Clean up `Session::handshake`'s exact-match approach some day.**
   Currently a single `buf != ClientHandshake::default().encode()` byte
   comparison, matching the reference server's own `memcmp` against one
   fixed 12-byte constant (no per-field sub-protocol/version reasoning,
   no rejection reply sent on mismatch - just a silent close). Works and
   is reference-accurate, but a raw byte-array comparison with no
   diagnostic on failure is a blunt instrument; worth revisiting once
   there's a reason to (e.g. wanting a logged reason for a rejected
   connection) without regressing the "no reply on mismatch" behavior.
10. **Reconsider the `HTLC_HDR_*`/`HTLS_HDR_*`/`HTLC_DATA_*`/`HTLS_DATA_*`
    naming convention used throughout doc comments and `opcode_names!`/
    field-id tables.** That convention comes from the reference
    implementation's own C header, which has no more claim to being
    "the" canonical name than any other implementation's internal
    naming — there's no real reason to preserve it here. `HLProtocol.md`
    (repo root) is the actual official source and uses its own constant
    names (`myTran_*` for transactions, `myField_*` for fields, `myAcc_*`
    for access privileges); those should be what this codebase's doc
    comments and naming cite instead, not the reference server's
    internal `HTLC_HDR_*` spelling. Not started: this touches
    `opcode.rs`'s `opcode_names!` invocations, `field_id.rs`'s tables,
    every "Original: `HTLC_HDR_X`"-style doc comment, and AGENTS.md's
    own style rule mandating the `HTLC_HDR_*` convention (which would
    need to change too if this is adopted) - a wide, mechanical, but
    non-trivial rename across the crate.
11. **Bring the other `session.rs` `on_*` handlers in line with the
    `if let Err(reason) = state.some_call(...).await { return
    tx.reply_error(reason); }` pattern** now documented in AGENTS.md and
    used by `on_chat_join`. Most handlers still do their own inline
    `Option`/bool checks against `SharedState` calls instead of getting
    a `Result<T, &'static str>` back directly. Not started.

## Needs manual verification against the real client

Unlike the `TCP_NODELAY`/push-`trans` fixes (explicitly verified against
the real Hotline 1.2.3 client), the accounts subsystem (named-account
login, account CRUD opcodes, the seeded `admin`/`admin` and passwordless
`guest` both providers start with) and the idle-to-away status broadcast
have only been unit-tested plus one manual `hlc` smoke test (login
accepted with the seeded admin account, rejected on wrong
password/unknown login) — not yet exercised against a real client.

Chat room invite/password gating (backlog item 6) is unit-tested only,
not yet exercised against a real client. The `UserSelfinfo` access-field
bug (backlog item 4) *was* found by real-client testing — see the
divergence note above — so real-client checks do surface real gaps here,
not just theoretical ones.

## `HLProtocol.md` review

`HLProtocol.md` (repo root) is the official protocol doc predating
community extensions — a stronger source of truth than the community
docs used for day-to-day cross-checking. Reviewed against current
behavior:

- Transaction opcode table (chat, file listing/transfer, users, news):
  matches this codebase's numbers and constant names exactly.
- The Flattened File Object framing: matches `hotline_proto::htxf`
  byte-for-byte (see item 3 above).
- `AccessPrivileges` bit layout: the doc lists 38 privileges (0–37) as a
  flat sequential list with no explicit bit numbers. Assuming privilege
  *N* → bit `63-N` throughout is **wrong** past privilege ~11 — the real
  wire layout has unnamed reserved bits partway through (confirmed
  against the reference's own `struct hl_access_bits`) that the doc's
  flat list doesn't show. `consts.rs`'s current bit positions were
  cross-checked against that struct and are correct; this was a
  false-alarm double check, not a bug.
- `HTLC_HDR_PING` (500) isn't in this original doc at all — a
  post-original addition, like `AccessPrivileges::VOICE_CHAT`/
  `READ_CHAT_HISTORY` (already flagged as community extensions in
  `consts.rs`'s module docs). `opcode.rs` doesn't currently flag `Ping`
  the same way. Cosmetic gap, offered to fix but not yet confirmed by
  the user.

See AGENTS.md for durable working conventions (verification loop, commit
hygiene, rustdoc style, reference-naming rules); this file only tracks
current status and open work.
