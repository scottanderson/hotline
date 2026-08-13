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

## Decision point: primary vs. secondary reference for chat/login divergences

Items 18, 19, 20, 27, 30, 32, and 35 below are all the same shape: the two
reference implementations disagree, and this server currently matches the
*secondary* reference in every one of these cases (chat room password/
invite gating, chat subject checks, chat invite reply behavior, dropbox
path substring matching, login case folding, agreement push branching,
and the `USER_GETLIST` access check). None of them has been switched to
the primary reference's behavior. This note just records the current
state as a single decision point to revisit later; it isn't a
recommendation either way.

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
   and resource-fork (`"MACR"`) data. A live per-transfer-progress tracker
   from this item is also what item 37's `on_user_get_info` file-transfer
   sections are waiting on; until it exists, those five sections always
   read `"None."`, with no in-progress transfers ever reported.
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
17. ~~Consider `Cow<'_, str>` in place of `String` on the workspace's many
    string fields to cut needless clones, e.g. `UserEntry`/`Session`
    duplicating `name`/`login`/`automatic_response`, or the field decode
    paths that copy wire bytes into an owned `String` even when the value
    is only read once. Worth a small proof of concept on one struct's
    fields before deciding whether it's worth the lifetime plumbing
    workspace-wide.~~ **Investigated, not worth doing.** Tried it on
    `UserEntry` (`login`/`account_name`/`name` to `Cow<'a, str>`, with `'a`
    forced to `'static` since `UserEntry` lives in `SharedState.users:
    Arc<RwLock<HashMap<u16, UserEntry>>>`, cloned into `tokio::spawn`'d
    sessions). Every construction site
    (`Session::announce_user_change`'s `UserEntry { login:
    self.login.clone(), .. }`, and the rest of `insert_user`'s call
    sites) still had to call `.clone()` on `Session`'s own `String`
    fields before wrapping the result in `Cow::Owned` via `.into()`,
    identical clone cost to today plus the wrapping noise: `Session` and
    `UserEntry` are separately owned, independently lived structs (one
    survives the whole connection task, the other sits in a shared map
    consumed by unrelated tasks), so there's no common ancestor scope a
    `Cow::Borrowed` could actually borrow from. `SharedState::users()`'s
    existing `self.users.read().await.values().cloned().collect()` has
    the same shape: it must clone every entry out from behind the
    `RwLockReadGuard` before returning owned data past the guard's
    lifetime, which `Cow` can't avoid either. No real call site was
    found where `Cow::Borrowed` would have avoided a clone that happens
    today; reverted.
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
19. **Not a bug, a primary/secondary divergence like item 18.**
    `on_chat_subject` adds an `AccessExtra::SET_SUBJECT` check, a
    membership requirement, and a room-password mechanism; the primary
    reference's `ProcessTran_SetChatSubject` has none of these — no
    access check, no password field on this transaction at all. The
    secondary reference's `rcv_chat_subject` has the identical shape to
    this server's: the same `set_subject`-for-global/membership-for-room
    check, and a real per-room password field. One gap either way: this
    server always sends an empty reply on success where the secondary
    reference sends none there either (it only replies, with an error
    `Msg` push, on failure) — harmless either way, since a stray reply to
    this transaction has no visible effect on a real client, but worth
    matching if this item is revisited.
20. **Not a bug, a primary/secondary divergence like item 18.**
    `on_chat_invite` always sends a reply (empty or error), and
    `invite_to_chat_room` (`state.rs`) requires the inviter to already be
    a room member and rejects a repeat invite to an already-invited
    target. The primary reference's `ProcessTran_InviteToChat` never
    replies and only skips a target already in the room's client list;
    the secondary reference's `rcv_chat_invite` matches this server
    instead — it requires the inviter to already be in the room, rejects
    a target already there, and always sends a `TASK` reply (success or
    error).
21. ~~`on_chat_part` always sends an empty reply; `ProcessTran_LeaveChat`
    never replies (`inTsn` is unused in the reference). Member
    notification and delete-when-empty already match.~~
    **Resolved.** `on_chat_part` now returns `Vec<Transaction>` and sends
    no reply on the success path, matching the dispatch table's
    established "empty vec means no reply" convention. Member
    notification and delete-when-empty are unchanged.
22. ~~`on_chat_decline` never broadcast the decline to the room, and
    always sent an empty reply where neither reference does.~~
    **Resolved as a superset of both references.** `on_chat_decline` now
    sends no reply on the success path (matching the dispatch table's
    established "empty vec means no reply" convention; a stray reply had
    no visible effect on a real client either way, since it
    fire-and-forgets the decline and never waits on one) and broadcasts a
    `"<<< "Name" declined invitation to chat >>>"` `Chat` push to the
    room's remaining members, matching the primary reference's
    `ProcessTran_RejectChatInvite`. The secondary reference does neither
    the broadcast nor the reply; adding the broadcast doesn't conflict
    with that; it's silent on it rather than doing something different.
23. ~~`on_chat` doesn't support the emote/action message style. The
    reference reads `Style` (109, `myField_ChatOptions`) and formats the
    message as `"*** {name} ..."` instead of the normal
    `"{name}: ..."` prefix when it's set to 1; this server always uses
    the normal prefix regardless of `Style`.~~ **Resolved.** `on_chat`
    now reads `ClientFieldId::Style` and formats the relayed message as
    `"\r*** {name} {text}"` when it's 1, keeping the normal
    `"\r{name:>13}:  {text}"` prefix otherwise.
24. ~~`on_msg_broadcast` sends the broadcast to every connected user
    including the sender (`state.publish`); the reference explicitly
    excludes the sender (`client->userID != inClient->userID`) from the
    fan-out. Should use the sender-excluding `announce` path instead.~~
    **Resolved.** `on_msg_broadcast` now calls `state.announce(self.user_id, ...)`
    instead of `state.publish(...)`, so the sender no longer receives
    their own broadcast.
25. ~~`on_get_file_name_list` requires `Access::DOWNLOAD_FILE`, a bit
    unrelated to listing in either reference.~~ **Resolved.** The
    secondary reference gates this same opcode
    (`HTLC_HDR_FILE_LIST`) on a dedicated `access_extra.file_list` bit;
    this server already defined `AccessExtra::FILE_LIST` for exactly this
    purpose (sibling of `FILE_GETINFO`/`FILE_HASH`) but never referenced
    it anywhere. `on_get_file_name_list` now checks `AccessExtra::FILE_LIST`
    instead, matching the existing `FILE_GETINFO`/`USER_GETLIST` pattern.
26. ~~`on_file_delete`/`on_file_move`/`on_file_symlink` all route through
    `dropbox_blocked`, requiring `VIEW_DROP_BOXES` to touch anything under
    a folder named "drop box". The reference's `ProcessTran_DeleteFile`/
    `ProcessTran_MoveFile`/`ProcessTran_MakeFileAlias` only check the
    type-specific privilege (delete/move/alias); drop-box visibility is
    only ever checked by `GetFileNameList` (listing) and `DownloadFile`
    (download), not by these three.~~ **Resolved.** `delete_file`/
    `move_file`/`symlink_file` no longer call `dropbox_blocked`; only the
    type-specific privilege check remains. `mkdir`/`rename_file` still
    gate on drop-box visibility, unaffected by this change.
27. **Not a bug, a primary/secondary divergence like item 18.**
    `on_file_put`'s drop-box/upload-folder match
    (`state.rs::is_dropbox_path`) treats any ancestor path component
    containing "upload"/"drop box" as sufficient. The primary reference
    only inspects the immediate destination folder's own name
    (`GetPathTargetName`), so `Uploads/Subfolder/` would be rejected
    there but accepted here. The secondary reference matches this server
    instead: its upload gate runs `strcasestr()` over the whole resolved
    destination path, the same substring-anywhere behavior.
28. ~~`on_file_get`'s reply omits `FileSize` (207); the reference sends
    both `TransferSize` (the flattened-object envelope size) and
    `FileSize` (the raw file size) on this transaction.~~ **Resolved.**
    `on_file_get`'s reply now also sends `FileSize`, the raw file size,
    alongside `TransferId`/`TransferSize`. `ClientFieldId::FileXferOptions`
    still isn't read at all — the reference's `opts == 2` ("data fork
    only") request shape has no equivalent here, every `FILE_GET` is
    treated as a full flattened transfer. `FILE_PUT`'s resume option
    (`opts == 1`) and
    `FILE_GET`/`FILE_PUT`'s `FileResumeData` field are already covered by
    item 6's HTXF follow-ups.
29. ~~`on_newsfile_post` has no size cap; the reference rejects a post over
    8192 bytes with `"Your news post was not accepted because it was too
    big."` before appending it to the board.~~ **Resolved.**
    `on_newsfile_post` now rejects text over 8192 bytes with that exact
    error before calling `state.post_news`, so an oversized post is never
    appended or published.
30. ~~Not a bug, a primary/secondary divergence like item 18.~~
    **Resolved: case-fold logins to lowercase.** One reference lowercases
    the login on delete, modify, and login lookup, but not on create or
    read; that inconsistency only ever worked because its accounts are
    folders on a filesystem that's case-insensitive by default, so a
    mixed-case login created there still resolves to the same folder
    later regardless of case. Ported literally onto this server's
    case-sensitive `HashMap`/JSON-file backends, that same inconsistency
    would let an account get created with a mixed-case login and then
    become permanently unreachable by delete, modify, or login. Chose to
    fold consistently on every login-touching operation instead (the
    other reference's own accounts, in contrast, are case-sensitive
    everywhere with no folding at all). A submitted login is lowercased
    once, at every point one enters the system (`on_login`,
    `on_account_delete`, `on_account_read`, `on_account_create_or_modify`,
    `apply_user_list_record`), so account storage keys and
    `SharedState`'s live per-session `login` are always
    the same canonical form.
31. ~~`on_login`'s immediate-join gate (skipping the `Agreed` step for
    old clients) checks only that a name was sent; the reference also
    requires a non-zero icon (`psUserName[0] && nIconID`) before doing
    so.~~ **Resolved.** `on_login` now joins immediately on
    `(tx.name().is_some() && icon != 0) || !modern`, so a modern client
    with a name but a zero or absent icon waits for `Agreed`; the
    pre-150 fallback is unaffected.
32. **Not a bug, a primary/secondary divergence like item 18.**
    `on_login`'s agreement-push logic collapses two distinct primary
    reference cases into one: when `NO_AGREEMENT` isn't set and no
    agreement text is configured, the primary reference still sends an
    empty `ShowAgreement` push (three-way branch: hardcoded "no
    agreement" flag, real text, or an empty prompt); this server sends
    the hardcoded "no agreement" flag in both the `NO_AGREEMENT`-set and
    no-text-configured cases, and skips the push entirely for pre-150
    clients in the latter. The secondary reference matches this server
    instead: it's a two-way branch too (real agreement text sent from a
    file, or the hardcoded no-agreement flag for a modern client), with
    no empty-prompt case.
33. ~~`ServerFieldId` has no `CommunityBannerID` variant, so `on_login`'s
    reply can never include it; the reference always sends this field
    (typically zero) on login. Low priority, cosmetic given the reference
    default is usually unset.~~ **Resolved.** The variant already existed
    as `ServerFieldId::Bannerid` (161); `on_login` now sends it as
    `Field::u16(ServerFieldId::Bannerid, 0)`.
34. ~~`on_login` only sends `ServerName` when configured non-empty; the
    reference always sends the field (as an empty pstring when unset).~~
    **Resolved.** `on_login` now always pushes `ServerFieldId::Servername`,
    empty when unconfigured.
35. ~~Not a bug, a primary/secondary divergence like item 18.~~
    **Resolved: kept the existing gated behavior.** `on_user_get_list`
    requires `AccessExtra::USER_GETLIST`; one reference performs no
    access check here at all, the other gates the opcode on this exact
    bit. Decided to keep the gate: it's consistent with every other
    opt-in behavior already modeled as an `AccessExtra` bit in this
    server, and it's invisible for default accounts since the bit is on
    by default. No code change.
36. ~~`on_user_kick`'s plain kick (no ban option) always pushes a
    `Politequit` notice to the target before disconnecting; the
    reference's plain kick sends nothing, it just tears down the
    transport. The temp/perm ban options (`Options` 1/2) are already
    flagged as unimplemented in `on_user_kick`'s own doc comment. A
    missing `Uid` field is also handled differently: the reference
    defaults to 0, finds no such client, and still replies success; this
    server replies with an explicit "missing user id" error.~~
    **Resolved.** Plain kick now sends `RecipientMessage::Disconnect`
    (a new silent-close signal, handled in `Session::run`'s select loop)
    instead of pushing `Politequit`. A missing `Uid` now defaults to
    target id 0 and still replies success. The temp/perm ban options
    remain unimplemented.
37. ~~`on_user_get_info`'s reply was a different report entirely, not just
    a different format: it sent connected duration and a privilege
    summary, and an extra `UserId` field the reference never includes.~~
    **Resolved for the parts this server can report.** `on_user_get_info`
    now follows the primary reference's own labels and section order
    (`Nickname`/`Name`/`Account`/`Address`, then File Downloads/Folder
    Downloads/File Uploads/Folder Uploads/Waiting Downloads, each
    `"None."` when empty) and sends only the two fields the reference
    does (`Data`/`Text`, `UserName`/`Name`), dropping the extra `UserId`
    field and the privilege summary neither reference includes. An
    `Online` duration line is kept alongside `Address` — not in the
    primary reference, but the secondary reference reports the same
    fact (`online`/`idle` duration) and it costs nothing extra since
    `connected_at` is already tracked. `Icon` and `Version` lines were
    added the same way: neither reference reports icon/client-version in
    this exact shape, but both are locally useful and now tracked
    end to end (`UserEntry::client_version`/`Session::client_version`,
    an `Option<u16>` set from `HTLC_DATA_CLIENTVERSION` (160) at login),
    so they're included alongside `Online` rather than left out. The
    `Version` line itself is omitted entirely rather than printed as
    empty/zero when a client never sent one. Still not carried over
    from the secondary reference: per-connection color/cipher/
    compression reporting, since none of those are tracked on
    `UserEntry`/`Session` today and adding just for this report would
    mean threading more new fields through all ~20 existing `UserEntry`
    construction sites for a cosmetic addition. **Not done:** the five
    file-transfer sections always read `"None."` — populating them with
    real upload/download progress needs a live per-transfer-progress
    tracker this server doesn't have yet (item 6's territory, not
    something to invent standalone here).
38. **Banner support is incomplete.** `ServerFieldId` has the wire fields
    (`Banner` (151), `BannerType` (152), `BannerUrl` (153), `Bannerid`
    (161)) and `AccessExtra::BANNER_GET` exists, but `on_login` only ever
    sends `Bannerid` hardcoded to 0, and nothing serves actual banner
    bytes/type/URL or handles a banner-fetch request. Needs the
    fetch opcode identified and a real banner source (configured image or
    URL) before this is more than a stub.
39. `on_chat`'s `SEND_CHAT` privilege check goes through the generic
    `Access::require`, which renders as "You are not allowed to do that
    (SEND_CHAT)."; the reference sends the fixed string "You are not
    allowed to participate in chat." for this specific denial. Needs a
    dedicated error path for this one check rather than the generic
    multi-flag renderer, matching the reference's exact text.

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
