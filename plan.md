# Near-term goals

Closing feature gaps versus the reference implementation used for behavior
parity (see AGENTS.md: never name it in code/commits/docs, only describe
*what*/*why*). Backlog below, in priority order.

## Known deliberate divergences from the reference server

- **`GETLIST` visibility timing.** The reference makes a raw TCP connection
  visible to `GETLIST` immediately at accept, before `Login` even arrives
  (blank name/icon until set). This server only becomes visible at `Login`
  time. Low priority.
- **`reply_error()` text doesn't match the reference's error strings.** The
  reference uses short, sometimes cryptic strings (`"who?!?"`, `"Uh, no."`);
  this server uses friendlier text. Not changed yet — decide with the user
  whether exact-string parity matters for any known client first.

## Backlog, in priority order

1. **`UserSelfinfo`'s access field sends the session's real privileges.**
   `Session::complete_join`'s push sends `self.access.bits()`, the same
   bitmap set at login/agreement time; the reference does the same
   (`inClient->access`, assigned from the account's real stored
   privileges with no hardcoding, is what's pushed on this opcode both at
   login and at agreement). **Resolved**, with a regression test covering
   this field.
2. `on_file_getinfo` sends only `FILE_NAME`/`FILE_SIZE`/`FILE_DATE_MODIFY`,
   omitting `FileType`/`FileCreator`/`FileDateCreate`/`FileComment`
   entirely. This matches how a server without real HFS metadata (the
   scope this codebase is in) behaves in practice: those four fields
   depend on genuine Mac-filesystem type/creator/comment data the
   reference only has when actually running on HFS, and a widely used
   non-HFS server implementation (relied on for years by real Windows and
   Mac clients) sends this exact trimmed set when that data isn't
   available. **Resolved**, confirmed against non-HFS server behavior
   rather than the primary reference's HFS-only code path.
3. ~~`on_file_setinfo` wrongly requires `FileNewName`. The reference never
   errors on an absent rename or comment; a missing `FileNewName` field
   reads back as an empty string, which just skips the rename step
   entirely and falls through to a success reply.~~ **Resolved.** An
   absent `FileNewName` now replies with an empty success transaction
   instead of an error.
4. ~~`on_file_mkdir`/`on_file_getinfo` don't support the `FileName`-absent
   fallback: when only `FilePath` is given, the reference derives the
   target from `FilePath`'s last component. Lower priority (unusual client
   behavior).~~ **Resolved.** Added `Session::file_name_and_dir_from_path_tail`,
   used by `on_file_mkdir`/`on_file_getinfo`: when `FileName` is absent, the
   target name comes from `FilePath`'s last component and the containing
   directory from the rest.
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
7. ~~**Private-message/chat refusal and automatic response are entirely
   unimplemented.**~~ **Resolved and implemented.** `on_user_change`
   (`UserChange`, 304) now reads `Options` (113) as a persisted-preference
   bitmap (bit 0 refuse messages, bit 1 refuse chat, bit 2 automatic
   response) plus `AutomaticResponse` (215) for the custom reply text
   (defaulting to a canned "not able to respond" string when the bit is
   set but the field is empty), storing all three on `Session` and
   mirroring them onto `UserEntry` so other sessions can look them up.
   `on_private_msg`/`on_chat_invite` share a `Session::refusal_bounce`
   helper: when the target refuses (and the sender doesn't refuse
   messages themselves, matching the reference's reuse of that one flag
   for both message and chat-invite bounces) or has an automatic response
   configured, the sender gets a `Msg` (104) push back with `Options`
   tagging it a refusal notice or an automatic response, and delivery to
   the target is skipped only when the target is actually refusing (an
   automatic response still lets the real message through). The
   PM-bounce gate is also conditioned on the client's own `Options` value
   in the `Msg` (108) request equaling "user message", matching the
   reference literally — a request that omits `Options` doesn't bounce.
   `QuotingMsg` (214) round-trips from request to the delivered push when
   present. Not replicated: an obscure 1.5-era client quirk where a
   `Msg` (108) request's `Options` can itself carry `myOpt_RefuseMessage`/
   `myOpt_RefuseChat` to have the server substitute canned text into an
   otherwise-normal delivery; narrow enough, and unconfirmed against any
   real client, that it's left as a possible future gap rather than
   guessed at.
8. ~~**`ServerFieldId` has no `Options` (113) variant**~~ **Resolved.**
   Added as part of item 7 above, since the bounce push needs it. The
   module doc's one-directional-field example was also stale (`Options`
   is no longer client-only) and has been updated to cite `FileNewName`
   instead, plus a note that `Options` itself now carries different
   meanings per direction. Still worth folding into item 15's full
   field-by-field audit rather than treating as fully settled.
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
   already used elsewhere for account file `"ALL"` parsing.
10. ~~Unbounded `FILE_PATH` component count: `HlPath::decode` caps nothing
    beyond the wire's own per-component 255-byte length, so one request can
    carry up to 65535 path levels. Pre-existing on `FILE_LIST`/`_GET`/
    `_PUT`; `FILE_MOVE`/`FILE_SYMLINK` each decode two such paths. Worth a
    depth/size limit eventually, not urgent.~~ **Resolved.** `HlPath::decode`
    now rejects a declared component count over 256 before allocating,
    returning `ProtoError::Malformed`. 256 isn't a reference-matched value
    (no reference behavior to match here), just a decoder-side safety bound
    picked to be generously beyond any real client's directory depth.
11. ~~`InMemoryFileProvider::delete`/`rename`/`r#move` auto-vivify missing
    intermediate directories as a side effect of a failed lookup (routes
    through `get_dir_mut`), leaving an empty directory tree behind instead
    of erroring "not found". `hls` always uses `DirectoryFileProvider`, so
    this doesn't affect production, but it's a test-only divergence.~~
    **Resolved.** Added a non-vivifying `Dir::get_dir_mut_existing`
    alongside the existing vivifying `get_dir_mut` (kept as-is for
    `mkdir`/`seed`/`create_write`, which should still create missing
    directories); `delete`/`rename`/`r#move` now use it and return
    `io::ErrorKind::NotFound` instead of creating the missing directory.
    `r#move`'s destination directory is checked the same way, matching the
    reference server's own "destination folder could not be found" error
    rather than creating it. Also fixed
    `session::tests::file_move_relocates_a_file`, which had been passing
    only because of the auto-vivify bug (it moved a file into a folder it
    never created).
12. ~~`HTLC_HDR_PING` (500) isn't in the original `HLProtocol.md` doc — a
    post-original addition, like `AccessPrivileges::VOICE_CHAT`/
    `READ_CHAT_HISTORY`. `opcode.rs` doesn't flag `Ping` the same way
    yet.~~ **Superseded by item 15.** The opcode is missing only from
    `HLProtocol.md`, not from either reference server, which both
    implement it under their own (different) names — see item 15's
    added note. `opcode.rs`'s doc comments just say it's undocumented
    in `HLProtocol.md`; the actual naming question belongs to item 15.
13. ~~Revisit `const fn` lost from `ClientOpcode`/`ServerOpcode`'s `From`/
    `Into` (via `num_enum`, not const-stable), which cascaded to
    `TransactionType::from_client_wire`/`from_server_wire` and
    `TransactionHeader::decode_from_client`/`decode_from_server`. Nothing
    currently calls these from a `const`/`static` binding, so this costs
    nothing today, but it departs from the crate's otherwise consistent
    `const fn` style (`PacketReader`, `numbers::encode`/`decode`). Low
    priority.~~ **Investigated, not worth doing.** The one route that
    would avoid duplicating each variant's numeric value a second time
    (a macro-generated `const fn from_u32` matching on `Self::Variant as
    u32` patterns, reusing `opcode_names!`'s existing variant list)
    doesn't compile: rustc rejects `as` casts to an integer on any enum
    with a data-carrying variant (`E0605`), and both enums have one
    (`Unknown(u32)`, the catch-all). The only remaining ways to hand
    write the conversion are a second, by-hand copy of every numeric
    literal next to the enum's own discriminants (exactly the
    drift-prone duplication `num_enum` was adopted to avoid) or an
    unsafe transmute (same reason, worse). Neither is the small,
    low-risk change this item was looking for, so left as is.
14. ~~Clean up `Session::handshake`'s exact match: a single `buf !=
    ClientHandshake::default().encode()` comparison, matching the
    reference's own `memcmp` (no per-field version reasoning, no reply on
    mismatch). Works and is reference-accurate, but gives no diagnostic on
    failure. Revisit without regressing the "no reply on mismatch"
    behavior.~~ **Already resolved.** `handshake`'s `HandshakeRejected`
    error propagates through `Session::run`'s `?` to `server.rs`'s
    `tracing::warn!(%peer, error = %e, "session ended with error")`,
    which already logs the peer and the specific rejection reason on any
    session error, including this one. No wire-visible behavior changes:
    the connection still closes without a reply on mismatch.
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
18. **Private chat rooms have no password/invitation gate in the primary
    reference at all, but do in the secondary one.** `SMyChat` (the
    primary reference's chat-room struct) has no password field and no
    invited-set; `ProcessTran_JoinChat` performs no check beyond "does
    this chat ID still exist" before adding the joiner — the invite push
    is purely a client-side UX nicety there, and any client that learns a
    live chat ID can join directly. The secondary reference does
    implement both: a per-room password plus an invited-file-descriptor
    set that a join either matches against or bypasses, which is what
    this server's `join_chat_room` (`state.rs`) actually ports (AGENTS.md's
    Status section description of chat being "gated on invitation like
    the reference" was written against that implementation, before the
    primary reference's source was available to check against). Also
    still true regardless of which reference this settles on:
    `on_chat_create` only invites a single `Uid` where the primary
    reference loops over every `UserID` field in the request, and fails
    the whole room creation if that one target isn't connected, where
    the primary reference just skips undeliverable invites and creates
    the room regardless. Needs a decision on which reference's chat-room
    semantics this server should track going forward, now that both are
    known to disagree here.
19. `on_chat_subject` adds an `AccessExtra::SET_SUBJECT` check, a
    membership requirement, a room-password mechanism, and always sends a
    reply; the reference's `ProcessTran_SetChatSubject` has none of
    these — no access check, no reply ever sent, and no password field
    exists on this transaction at all.
20. `on_chat_invite` always sends a reply (empty or error);
    `ProcessTran_InviteToChat` never replies at all. Separately,
    `invite_to_chat_room` (`state.rs`) requires the inviter to already be
    a room member and rejects a repeat invite to an already-invited
    target — both checks the reference doesn't perform (it only skips a
    target already in the room's client list).
21. `on_chat_part` always sends an empty reply; `ProcessTran_LeaveChat`
    never replies (`inTsn` is unused in the reference). Member
    notification and delete-when-empty already match.
22. `on_chat_decline` never broadcasts the decline to the room; the
    reference sends a `"<<< "Name" declined invitation to chat >>>"` chat
    message to the room's remaining members. `on_chat_decline` also
    always sends an empty reply where the reference sends none.
23. `on_chat` doesn't support the emote/action message style. The
    reference reads `Style` (109, `myField_ChatOptions`) and formats the
    message as `"*** {name} ..."` instead of the normal
    `"{name}: ..."` prefix when it's set to 1; this server always uses
    the normal prefix regardless of `Style`.
24. `on_msg_broadcast` sends the broadcast to every connected user
    including the sender (`state.publish`); the reference explicitly
    excludes the sender (`client->userID != inClient->userID`) from the
    fan-out. Should use the sender-excluding `announce` path instead.
25. `on_get_file_name_list` requires `Access::DOWNLOAD_FILE`;
    `ProcessTran_GetFileNameList` performs no access check at all to list
    a folder — it only checks `ViewDropBoxes` to decide whether to filter
    the listing, never to refuse it outright. Any logged-in client can
    list files in the reference regardless of download privilege.
26. `on_file_delete`/`on_file_move`/`on_file_symlink` all route through
    `dropbox_blocked`, requiring `VIEW_DROP_BOXES` to touch anything under
    a folder named "drop box". The reference's `ProcessTran_DeleteFile`/
    `ProcessTran_MoveFile`/`ProcessTran_MakeFileAlias` only check the
    type-specific privilege (delete/move/alias); drop-box visibility is
    only ever checked by `GetFileNameList` (listing) and `DownloadFile`
    (download), not by these three.
27. `on_file_put`'s drop-box/upload-folder match
    (`state.rs::is_dropbox_path`) treats any ancestor path component
    containing "upload"/"drop box" as sufficient. The reference only
    inspects the immediate destination folder's own name
    (`GetPathTargetName`), so `Uploads/Subfolder/` would be rejected
    there but is accepted here.
28. `on_file_get`'s reply omits `FileSize` (207); the reference sends
    both `TransferSize` (the flattened-object envelope size) and
    `FileSize` (the raw file size) on this transaction.
    `ClientFieldId::FileXferOptions` also isn't read at all — the
    reference's `opts == 2` ("data fork only") request shape has no
    equivalent here, every `FILE_GET` is treated as a full flattened
    transfer. `FILE_PUT`'s resume option (`opts == 1`) and
    `FILE_GET`/`FILE_PUT`'s `FileResumeData` field are already covered by
    item 6's HTXF follow-ups.
29. `on_newsfile_post` has no size cap; the reference rejects a post over
    8192 bytes with `"Your news post was not accepted because it was too
    big."` before appending it to the board.
30. Login names aren't case-folded anywhere in `accounts.rs`/`session.rs`.
    The reference lowercases the login on every account create/delete/
    modify and on login lookup itself, so two accounts differing only by
    case can't coexist there and a login always matches regardless of
    case. This server currently allows both.
31. `on_login`'s immediate-join gate (skipping the `Agreed` step for
    old clients) checks only that a name was sent; the reference also
    requires a non-zero icon (`psUserName[0] && nIconID`) before doing
    so.
32. `on_login`'s agreement-push logic collapses two distinct reference
    cases into one: when `NO_AGREEMENT` isn't set and no agreement text
    is configured, the reference still sends an empty `ShowAgreement`
    push (three-way branch: hardcoded "no agreement" flag, real text, or
    an empty prompt); this server sends the hardcoded "no agreement" flag
    in both the `NO_AGREEMENT`-set and no-text-configured cases, and skips
    the push entirely for pre-150 clients in the latter.
33. `ServerFieldId` has no `CommunityBannerID` variant, so `on_login`'s
    reply can never include it; the reference always sends this field
    (typically zero) on login. Low priority, cosmetic given the reference
    default is usually unset.
34. `on_login` only sends `ServerName` when configured non-empty; the
    reference always sends the field (as an empty pstring when unset).
35. `on_user_get_list` requires `AccessExtra::USER_GETLIST`;
    `ProcessTran_GetUserNameList` performs no access check at all.
    Invisible for default accounts (the bit is on by default) but denies
    an account explicitly stripped of it where the reference would still
    serve the list.
36. `on_user_kick`'s plain kick (no ban option) always pushes a
    `Politequit` notice to the target before disconnecting; the
    reference's plain kick sends nothing, it just tears down the
    transport. The temp/perm ban options (`Options` 1/2) are already
    flagged as unimplemented in `on_user_kick`'s own doc comment. A
    missing `Uid` field is also handled differently: the reference
    defaults to 0, finds no such client, and still replies success; this
    server replies with an explicit "missing user id" error.
37. `on_user_get_info`'s reply is a different report entirely, not just a
    different format: it sends connected duration and a privilege
    summary, and an extra `UserId` field the reference never includes.
    The reference's `ProcessTran_GetClientInfoText` instead reports
    active file-transfer state (upload/download file and folder lists,
    waiting downloads) plus nickname/account/address, with no
    duration/privilege summary at all, and only two fields (`Data`,
    `UserName`).

## Needs manual verification against the real client

Unlike the `TCP_NODELAY`/push-`trans` fixes (explicitly verified against
the real Hotline 1.2.3 client), the accounts subsystem (named-account
login, account CRUD opcodes, the seeded `admin`/`admin` and passwordless
`guest` both providers start with), idle-to-away status broadcast, and
chat room invite/password gating have only been unit-tested plus one
manual `hlc` smoke test — not yet exercised against a real client.

See AGENTS.md for durable working conventions (verification loop, commit
hygiene, rustdoc style, reference-naming rules); this file only tracks
current status and open work.
