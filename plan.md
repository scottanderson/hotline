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

## Backlog, in priority order

1. ~~`access_extra`-equivalent modeling.~~ Done — see `AccessExtra` in
   `accounts.rs`.
2. ~~Unify per-recipient delivery into one ordered channel.~~ Done — see
   AGENTS.md's "Server session/state split".
3. ~~File transfer (`HTXF`), single-file slice.~~ Done — a second
   `TcpListener` on `port + 1` (`hotline_server::server::run_htxf_listener`);
   `HTLC_HDR_FILE_GET`/`_PUT` (202/203) allocate an opaque `u32` ref and
   record a pending transfer (path, direction, size), matched on the data
   connection by that ref plus the client's IP; the handshake is the
   16-byte `"HTXF"` header (`hotline_proto::htxf::HtxfHeader`); data is
   framed in the "FILP" container (`hotline_proto::htxf`) — an `INFO`
   fork (zeroed type/creator, no real HFS metadata) then a `DATA` fork
   with raw bytes, no resource fork. File storage goes through
   `FileProvider` (a directory-backed default, `--files-dir`, mirroring
   `--accounts-dir`), not a hardcoded placeholder. Verified with a manual
   round-trip (download + upload) against a running `hls`/`hotline-client`;
   not yet exercised against a real Hotline client.
   - **Explicitly deferred** (own follow-up items, not silently dropped):
     resume support (client-sent `RFLT` blob with `data_pos`/`rsrc_pos`
     offsets), the transfer queue (`HTLS_DATA_QUEUE_POSITION`,
     `HTLS_HDR_QUEUE_UPDATE`), folder transfers (`HTLC_HDR_FILE_GETFOLDER`/
     `_PUTFOLDER`, a separate `FILE_NEXT`/`FILE_SEND`/`FILE_RESUME` command
     loop on the transfer connection), and resource-fork (`"MACR"`) data.
4. **Threaded news.** Current news support is flat/classic only.
5. ~~Chat room passwords.~~ Done — `ChatRoom.password`, set via
   `HTLC_HDR_CHAT_SUBJECT` and checked on `HTLC_HDR_CHAT_JOIN`; also
   closed missing membership/validation checks found on
   `HTLC_HDR_CHAT_SUBJECT`/`HTLC_HDR_CHAT_INVITE` along the way.
6. **Remaining unhandled opcodes.** Mostly map to an item above rather
   than being new independent work: `FileDelete`/`FileMkdir`/
   `FileGetinfo`/`FileSetinfo`/`FileMove`/`FileSymlink` (204–209, now
   unblocked by item 3's real file storage, but each still its own
   handler); `NewsListdir`/`NewsListcategory`/`NewsDelete`/`NewsMkdir`/
   `NewsMkcategory`/`NewsGetthread`/`NewsPostthread`/`NewsDeletethread`
   (370–411, item 4).
7. **Revisit `const fn` lost from `ClientOpcode`/`ServerOpcode`'s
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
