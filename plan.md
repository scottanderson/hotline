# Near-term goals

Closing feature gaps versus the reference implementation used for behavior
parity (see AGENTS.md: never name it in code/commits/docs, only describe
*what*/*why*). Backlog below, in priority order.

## Known deliberate divergences from the reference server

- **`GETLIST` visibility timing.** The reference makes a raw TCP connection
  visible to `GETLIST` immediately at accept, before `Login` even arrives
  (blank name/icon until set). This server only becomes visible at `Login`
  time. Low priority.
- **`UserSelfinfo`'s access field.** The reference always sends a hardcoded
  "everything enabled" fake access bitmap in this push; enforcement is
  entirely server side. This server sends the real computed access instead,
  which real clients render wrong (see backlog item 1).
- **`reply_error()` text doesn't match the reference's error strings.** The
  reference uses short, sometimes cryptic strings (`"who?!?"`, `"Uh, no."`);
  this server uses friendlier text. Not changed yet — decide with the user
  whether exact-string parity matters for any known client first.

## Backlog, in priority order

1. **Fix `UserSelfinfo`'s access field to match the reference's hardcoded
   bitmap.** Confirmed real-client bug: sending the real computed
   `AccessPrivileges` (`self.access.bits()` in `Session::complete_join`)
   instead of the reference's hardcoded "everything enabled" value means
   real clients show wrong permissions for themselves. Enforcement is
   server side regardless of this field, so sending the hardcoded value
   should be safe.
2. `on_file_getinfo` sends `FileType`/`FileCreator`/`FileDateCreate`/
   `FileComment` as zeroed placeholders. With HFS disabled (this codebase's
   own "no real HFS metadata" scope) the reference sends only
   `FILE_SIZE`/`FILE_NAME`/`FILE_DATE_MODIFY`.
3. `on_file_setinfo` wrongly requires `FileNewName`. The reference only
   errors if *both* rename and comment are absent; since comment storage
   isn't implemented, the correct behavior is a no-op (empty reply) when
   `FileNewName` is absent.
4. `on_file_mkdir`/`on_file_getinfo` don't support the `FileName`-absent
   fallback: when only `FilePath` is given, the reference derives the
   target from `FilePath`'s last component. Lower priority (unusual client
   behavior).
5. **Threaded news.** Current news support is flat/classic only; also
   covers the unhandled `NewsListdir`/`NewsListcategory`/`NewsDelete`/
   `NewsMkdir`/`NewsMkcategory`/`NewsGetthread`/`NewsPostthread`/
   `NewsDeletethread` opcodes (370-411).
6. **HTXF follow ups, deferred out of the initial file-transfer work:**
   resume support (client-sent `RFLT` blob with `data_pos`/`rsrc_pos`),
   the transfer queue (`HTLS_DATA_QUEUE_POSITION`, `HTLS_HDR_QUEUE_UPDATE`),
   folder transfers (`HTLC_HDR_FILE_GETFOLDER`/`_PUTFOLDER`, a separate
   `FILE_NEXT`/`FILE_SEND`/`FILE_RESUME` loop on the transfer connection),
   and resource-fork (`"MACR"`) data.
7. **Private-message/chat refusal and automatic response are entirely
   unimplemented.** `UserFlags::REFUSES_PM`/`REFUSES_CHAT` are declared
   but nothing ever sets them: `on_user_change` (`UserChange`, 304) only
   reads `Name`/`Icon` from the request, dropping the client's `Options`
   field (113) entirely, so there's no `Session` state for these
   preferences (or an "automatic response" message) and no enforcement in
   `on_private_msg`/`on_chat_invite` — a user who's turned on "refuse
   private messages" still receives every one. Reference shape: on
   refusal, or when the target has an automatic response configured, the
   *sender* gets a server-generated bounce (a canned "so-and-so does not
   accept..." notice, or the target's custom auto-response text) instead
   of the message/invite reaching the target; delivery only happens when
   the target isn't refusing. Needs the `AutomaticResponse` (215) and
   `QuotingMsg` (214) fields, neither of which exist in `field_id.rs` yet
   either. Not started, no user-facing precedent confirmed yet for the
   bounce transaction's exact field shape.
8. **`ServerFieldId` has no `Options` (113) variant**, discovered while
   scoping item 7 above: `field_id.rs`'s own module docs cite `Options` as
   the canonical example of a one-directional, client-only field, but the
   push variant of the private-message transaction sends `Options` too
   (to tag a bounce as a refusal notice vs. an automatic response vs. a
   real message). This is a concrete counterexample to item 15's "no real
   collision" premise, found by tracing one specific transaction rather
   than the full field-by-field audit item 15 describes — worth folding
   into that audit rather than fixing in isolation.
9. ~~`USER_FLAGS` (112) may have two more meaningful bits...~~ **Resolved
   and implemented.** The reference server computes this field from a
   dedicated function, ORing in two bits beyond the four already
   implemented: one set when the connection's privileges are exactly the
   full set of currently-defined privileges (excluding the "no agreement
   needed" bit, which it deliberately ignores), one set when the
   connection has no privileges at all (all zero). Both are computed and
   sent by the server itself, not a client-only rendering convention, so
   they're in scope here. Added as `UserFlags::ALL_PRIVS`/`NO_PRIVS`
   (`crates/hotline-proto/src/types.rs`), computed in
   `Session::user_flags` and `UserEntry::flags`
   (`crates/hotline-server/src/session.rs`, `state.rs`) using the same
   `Access::all() - Access::NO_AGREEMENT`/`Access::is_empty()` comparisons
   already used elsewhere for the "everything enabled" hardcoded bitmap
   (backlog item 1) and account file `"ALL"` parsing.
10. Unbounded `FILE_PATH` component count: `HlPath::decode` caps nothing
    beyond the wire's own per-component 255-byte length, so one request can
    carry up to 65535 path levels. Pre-existing on `FILE_LIST`/`_GET`/
    `_PUT`; `FILE_MOVE`/`FILE_SYMLINK` each decode two such paths. Worth a
    depth/size limit eventually, not urgent.
11. `InMemoryFileProvider::delete`/`rename`/`r#move` auto-vivify missing
    intermediate directories as a side effect of a failed lookup (routes
    through `get_dir_mut`), leaving an empty directory tree behind instead
    of erroring "not found". `hls` always uses `DirectoryFileProvider`, so
    this doesn't affect production, but it's a test-only divergence.
12. ~~`HTLC_HDR_PING` (500) isn't in the original `HLProtocol.md` doc — a
    post-original addition, like `AccessPrivileges::VOICE_CHAT`/
    `READ_CHAT_HISTORY`. `opcode.rs` doesn't flag `Ping` the same way
    yet.~~ **Superseded by item 15.** The opcode is missing only from
    `HLProtocol.md`, not from either reference server, which both
    implement it under their own (different) names — see item 15's
    added note. `opcode.rs`'s doc comments just say it's undocumented
    in `HLProtocol.md`; the actual naming question belongs to item 15.
13. Revisit `const fn` lost from `ClientOpcode`/`ServerOpcode`'s `From`/
    `Into` (via `num_enum`, not const-stable), which cascaded to
    `TransactionType::from_client_wire`/`from_server_wire` and
    `TransactionHeader::decode_from_client`/`decode_from_server`. Nothing
    currently calls these from a `const`/`static` binding, so this costs
    nothing today, but it departs from the crate's otherwise consistent
    `const fn` style (`PacketReader`, `numbers::encode`/`decode`). Low
    priority.
14. Clean up `Session::handshake`'s exact match: a single `buf !=
    ClientHandshake::default().encode()` comparison, matching the
    reference's own `memcmp` (no per-field version reasoning, no reply on
    mismatch). Works and is reference-accurate, but gives no diagnostic on
    failure. Revisit without regressing the "no reply on mismatch"
    behavior.
15. Reconsider the `HTLC_HDR_*`/`HTLS_HDR_*`/`HTLC_DATA_*`/`HTLS_DATA_*`
    naming used throughout doc comments and `opcode_names!`/field-id
    tables. That convention comes from the reference's own C header, which
    has no stronger claim to being canonical than any other
    implementation's naming. `HLProtocol.md` (repo root) is the actual
    official source and uses its own names (`myTran_*`, `myField_*`,
    `myAcc_*`); doc comments and naming could cite those instead. Touches
    `opcode.rs`, `field_id.rs`, every "Original: `HTLC_HDR_X`" comment, and
    AGENTS.md's own style rule mandating `HTLC_HDR_*` (which would need to
    change too). Not started — wide, mechanical, non-trivial rename.
    Confirmed concretely while adding `HTLC_HDR_GET_USER_LIST`/
    `HTLC_HDR_SET_USER_LIST` (348/349): most existing 100-116 range field
    variants (`Login`, `Password`, `Access`, `Uid`, `Icon`, ...) are
    shortened Rust-ish names, not the reference's own tail (`UserLogin`,
    `UserPassword`, `UserAccess`, `UserID`, `UserIconID`, ...); field 109
    is `Style`/`HTLC_DATA_STYLE` here but the reference calls it
    `ChatOptions`. Newly added fields/opcodes since then use the
    reference's own tail (`UserAlias`, `GetUserList`, `SetUserList`);
    everything older still uses the shortened form until this item lands.
    Also applies to `Ping` (500): absent from `HLProtocol.md` entirely,
    so it has no official name to cite there either; the
    `HTLC_HDR_PING` name currently used for it traces to the secondary
    reference used for cross-checking (see AGENTS.md), while the
    primary reference's own header names the same opcode
    `myTran_KeepConnectionAlive`. Worth resolving which name this item
    settles on before renaming it.
16. Collapse `ClientFieldId`/`ServerFieldId` into one `FieldId`, and
    `ClientOpcode`/`ServerOpcode` into one `Opcode`. `HLProtocol.md` has no
    client/server split in its numbering — one flat `myField_*` list, one
    flat `myTran_*` list, with direction recorded separately per
    transaction. A full field-by-field, transaction-by-transaction audit
    against the doc found no case where the same numeric ID means
    something different depending on direction; every match was either
    identical both ways or simply unused in the other direction (per
    `field_id.rs`'s own module docs, e.g. `ErrorText` server-only,
    `Options` client-only) — **except `Options` itself, per item 8 above**,
    found after this item was originally written. Touches `field_id.rs`,
    `opcode.rs`, `transaction.rs`'s `Debug` impl, and every
    `ClientFieldId::X`/`ServerFieldId::X`/`ClientOpcode::X`/
    `ServerOpcode::X` reference across the workspace. Re-confirm the "no
    real collision" premise (now known to have at least one exception)
    before starting.
17. Consider `Cow<'_, str>` in place of `String` on the workspace's many
    string fields to cut needless clones, e.g. `UserEntry`/`Session`
    duplicating `name`/`login`/`automatic_response`, or the field decode
    paths that copy wire bytes into an owned `String` even when the value
    is only read once. Worth a small proof of concept on one struct's
    fields before deciding whether it's worth the lifetime plumbing
    workspace-wide. Not started.

## Needs manual verification against the real client

Unlike the `TCP_NODELAY`/push-`trans` fixes (explicitly verified against
the real Hotline 1.2.3 client), the accounts subsystem (named-account
login, account CRUD opcodes, the seeded `admin`/`admin` and passwordless
`guest` both providers start with), idle-to-away status broadcast, and
chat room invite/password gating have only been unit-tested plus one
manual `hlc` smoke test — not yet exercised against a real client. The
`UserSelfinfo` access-field bug (backlog item 1) *was* found by real-client
testing, so that pass is worth repeating as more surface lands.

See AGENTS.md for durable working conventions (verification loop, commit
hygiene, rustdoc style, reference-naming rules); this file only tracks
current status and open work.
