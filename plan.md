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
- **Posted news article title/poster/flavor length cap.** Openline caps
  these at a small fixed buffer and truncates silently past it; this
  server accepts up to the wire format's real 255-byte `pstring` limit
  and errors past that instead. More permissive by design; not changed.
- **Posted news article flavor default.** Openline stores an empty
  string when a post omits `myField_NewsArtDataFlav`; this server
  defaults to `"text/plain"`. A real client always sends this field, so
  this only affects a hand-crafted request; not changed.

## Decision point: primary vs. secondary reference for chat/login divergences (resolved)

Items 18, 19, 20, 27, 30, 32, 35, 47, and 48 below were all the same shape:
the two reference implementations disagree, and this server matched the
*secondary* reference in every one of these cases (chat room
password/invite gating, chat subject checks, chat invite reply behavior,
dropbox path substring matching, login case folding, agreement push
branching, the `USER_GETLIST` access check, `AccessExtra::CHAT_PRIVATE`
gating chat-room membership opcodes, and `AccessExtra::MSG` as an
alternative to `Access::SEND_MESSAGE`). **Decided: keep the
secondary-matching behavior for all of them**, each now marked resolved at
its own item below. The one exception found along the way was
`on_chat_create`'s invite-target handling (item 18's bundled sub-bug),
which matched neither reference and was fixed toward the *primary*
reference's shape instead, since it's the one that fits this server's
existing one-shared-room model. An entirely absent `Uid` still replies
"missing user id"; a present but undeliverable target is skipped with a
debug-logged reason rather than failing the request.

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
5. ~~**Threaded news.** Current news support is flat/classic only; also
   covers the unhandled `NewsListdir`/`NewsListcategory`/`NewsDelete`/
   `NewsMkdir`/`NewsMkcategory`/`NewsGetthread`/`NewsPostthread`/
   `NewsDeletethread` opcodes (370-411).~~ **Resolved and implemented.**
   `crate::news` holds an in-memory tree of bundles (folders) and
   categories, each category a thread of articles addressed by
   parent/reply linkage; sibling order (for `NEWS_PREVTHREADID`/
   `NEWS_NEXTTHREADID`) is derived from position in the parent's
   children list rather than stored as explicit links. All eight
   opcodes are wired up in `session.rs`, gated on the matching
   bundle/category/article `NEWS_*` privilege. A non-recursive
   `NewsDeletethread` promotes the deleted article's first reply into
   its place, taking on its remaining replies as its own, matching the
   reference's behavior for that case. Category add/delete serial
   numbers mirror a reference quirk: the add serial increments on every
   post, the delete serial never changes from its initial value.
   `NewsArticle` still stores a single MIME flavor and hardcodes the wire
   flavor count to 1; the reference supports up to 32 flavors per
   article. Fine as a starting simplification, revisit if multi-flavor
   posting is ever needed.
6. **HTXF follow ups, deferred out of the initial file-transfer work.**
   Four largely independent pieces:
   - ~~Resume support (client-sent `RFLT` blob).~~ **Resolved for
     downloads.** `FILE_GET` (202) now reads `myField_FileResumeData`
     (203) when present: `hotline_proto::htxf::decode_resume_data` finds
     the `DATA` fork entry's already-transferred byte count (any other
     fork entry, e.g. a resource-fork position, is ignored, matching the
     single-fork model below), the offset is clamped to the file's size
     rather than rejected (an offset past EOF just yields zero bytes left
     to send, matching the reference's own clamp), and `PendingTransfer`
     carries it through to the data connection: the flattened-object
     header is always resent in full, but its `DATA` fork now declares
     only the remaining size, and the reader skips the already-sent
     leading bytes before copying the rest. **Upload resume (`FILE_PUT`'s
     `opts == 1`, replying with a fresh `RFLT` via
     `myField_FileResumeData`) is not implemented and is a blocker, not
     just deferred work:** the
     reference only allows resuming into a file it recognizes as one of
     its own interrupted uploads, checked via a type/creator marker
     persisted on the partial file itself; this server's `FileProvider`
     trait has no equivalent (its own `info`/`size` calls have no way to
     distinguish "an interrupted upload this server started" from "an
     unrelated file that happens to have this name"), so implementing
     upload resume without that check would let a client "resume" into
     any existing file by name and append or overwrite its bytes. Closing
     the gap needs either a breaking addition to `FileProvider` (a
     marker-aware create/open call) or a server-side bookkeeping table
     that doesn't survive a restart, either of which is a real design
     decision, not a guess.
   - Transfer queue (`HTLS_DATA_QUEUE_POSITION`, `HTLS_HDR_QUEUE_UPDATE`
     equivalents (`WaitingCount` (116), the `DownloadInfo` push)): not
     implemented. Needs a concurrent-download cap in `ServerConfig` and a
     waiting list in `SharedState`; not started. A live per-transfer-
     progress tracker from this piece is also what item 37's
     `on_user_get_info` file-transfer sections are waiting on; until it
     exists, those five sections always read `"None."`, with no
     in-progress transfers ever reported.
   - Folder transfers (`HTLC_HDR_FILE_GETFOLDER`/`_PUTFOLDER`, a separate
     `FILE_NEXT`/`FILE_SEND`/`FILE_RESUME` loop on the transfer
     connection): not implemented, not started.
   - ~~Resource-fork (`"MACR"`) data.~~ **Resolved, no code change
     needed.** The upload receive path (`receive_file` in `server.rs`)
     already discards any fork it doesn't recognize by tag rather than
     assuming exactly `INFO`+`DATA`, so a client sending a third
     (resource-fork) fork on upload is already handled the same way the
     data-fork-only reference storage backend handles it: the bytes are
     read off the wire and dropped. Nothing in this server's `FileProvider`
     model has a place to keep resource-fork bytes, and the equivalent
     reference backend never emits one on download either, so there is no
     remaining gap to close here without a `FileProvider` extension no
     documented behavior actually calls for.
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
15. ~~Reconsider the `HTLC_HDR_*`/`HTLS_HDR_*`/`HTLC_DATA_*`/`HTLS_DATA_*`
    naming used throughout doc comments and `opcode_names!`/field-id
    tables. That convention comes from the reference's own C header, which
    has no stronger claim to being canonical than any other
    implementation's naming. `HLProtocol.md` (repo root) is the actual
    official source and uses its own names (`myTran_*`, `myField_*`,
    `myAcc_*`); doc comments and naming could cite those instead.~~
    **Resolved.** Every "Original: `HTLC_HDR_X`"/"Original: `HTLS_DATA_X`"
    style comment, the `opcode_names!`/`name()` tables in `opcode.rs`/
    `field_id.rs`, and AGENTS.md's own citation rule now cite
    `HLProtocol.md`'s `myTran_*`/`myField_*` constant instead, checked
    field by field and opcode by opcode against the doc's own tables.
    Two opcodes (`GetUserList`/`SetUserList`, 348/349) have no
    `HLProtocol.md` entry at all, so they keep citing the reference
    header's own name, and AGENTS.md's rule now says so explicitly. Same
    for `Ping` (500): still absent from `HLProtocol.md`, so it now cites
    the *primary* reference's own name for it, `myTran_KeepConnectionAlive`
    (the secondary reference's `HTLC_HDR_PING` traced to cross-checking
    only, per AGENTS.md the primary reference is the one with the
    stronger claim when a name has to be chosen without an official
    source). Rust variant identifiers (`Ping`, `Style`, `Uid`, ...) are
    unchanged; this item was about which name a doc comment cites, not
    about renaming the Rust-side symbols, so the pre-existing drift
    between variant names and their cited original noted below is still
    there where it was before.
16. ~~Collapse `ClientFieldId`/`ServerFieldId` into one `FieldId`, and
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
18. ~~Private chat rooms have no password/invitation gate in the primary
    reference at all, but do in the secondary one.~~ **Resolved: keep the
    secondary-matching gate.** `join_chat_room` (`state.rs`) keeps its
    per-room password plus invited-set check; the primary reference's
    "any client that learns a live chat ID can join" model was
    considered and rejected. Separately, `on_chat_create`'s invite
    behavior matched *neither* reference (it only read a single `Uid`
    and failed the whole creation if that target wasn't connected, where
    the primary reference loops over every `UserID` field, creates one
    shared room, and just skips undeliverable targets, and the secondary
    reference loops too but creates a separate room per target with a
    per-target error reply). **Resolved toward the primary reference's
    shape**, since it's the one that matches this server's existing
    one-shared-room-with-an-invite-set model: `on_chat_create` now reads
    every `Uid` field via `tx.fields(...)`, creates one room, and skips
    (rather than fails on) a target that isn't connected.
19. ~~Not a bug, a primary/secondary divergence like item 18.~~
    **Resolved: keep the secondary-matching behavior.**
    `on_chat_subject` keeps its `AccessExtra::SET_SUBJECT` check,
    membership requirement, and room-password mechanism; the primary
    reference's no-check, no-password behavior was considered and
    rejected. The always-empty-success-reply gap noted here (secondary
    sends no reply on success, only an error `Msg` push on failure) is
    left as is — harmless, no visible effect on a real client.
20. ~~Not a bug, a primary/secondary divergence like item 18.~~
    **Resolved: keep the secondary-matching behavior.** `on_chat_invite`
    keeps always sending a reply, requiring the inviter to already be a
    room member, and rejecting a repeat invite to an already-invited
    target; the primary reference's never-reply, looser-check behavior
    was considered and rejected.
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
    now reads `FieldId::Style` and formats the relayed message as
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
27. ~~Not a bug, a primary/secondary divergence like item 18.~~
    **Resolved: keep the secondary-matching behavior.**
    `on_file_put`'s drop-box/upload-folder match
    (`state.rs::is_dropbox_path`) keeps treating any ancestor path
    component containing "upload"/"drop box" as sufficient; the primary
    reference's immediate-destination-folder-only check
    (`GetPathTargetName`) was considered and rejected.
28. ~~`on_file_get`'s reply omits `FileSize` (207); the reference sends
    both `TransferSize` (the flattened-object envelope size) and
    `FileSize` (the raw file size) on this transaction.~~ **Resolved.**
    `on_file_get`'s reply now also sends `FileSize`, the raw file size,
    alongside `TransferId`/`TransferSize`. `FieldId::FileXferOptions`
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
32. ~~Not a bug, a primary/secondary divergence like item 18.~~
    **Resolved: keep the secondary-matching behavior.** `on_login`'s
    agreement-push logic stays a two-way branch (real agreement text
    sent from a file, or the hardcoded no-agreement flag for a modern
    client, skipping the push entirely for pre-150 clients in the
    no-text case); the primary reference's three-way branch (adding an
    empty `ShowAgreement` prompt case) was considered and rejected.
33. ~~`ServerFieldId` has no `CommunityBannerID` variant, so `on_login`'s
    reply can never include it; the reference always sends this field
    (typically zero) on login. Low priority, cosmetic given the reference
    default is usually unset.~~ **Resolved.** The variant already existed
    as `FieldId::Bannerid` (161); `on_login` now sends it as
    `Field::u16(FieldId::Bannerid, 0)`.
34. ~~`on_login` only sends `ServerName` when configured non-empty; the
    reference always sends the field (as an empty pstring when unset).~~
    **Resolved.** `on_login` now always pushes `FieldId::Servername`,
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
    target id 0 and still replies success. The temp/perm ban options are
    now implemented too, see item 43.
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
    an `Option<u16>` set from `myField_Vers` (160) at login),
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
39. ~~`on_chat`'s `SEND_CHAT` privilege check goes through the generic
    `Access::require`, which renders as "You are not allowed to do that
    (SEND_CHAT)."; the reference sends the fixed string "You are not
    allowed to participate in chat." for this specific denial.~~
    **Resolved.** `RequireFlags::require` now renders each missing flag
    through a per-flag `actions()` table (`Access`/`AccessExtra` each
    provide their own) instead of a raw flag-name dump, so `SEND_CHAT`'s
    denial reads "You are not allowed to participate in chat." with no
    dedicated code path needed at the `on_chat` call site. Every `Access`
    entry with a known reference denial string (all single-flag checks
    currently wired in `session.rs`/`state.rs`) now matches that string
    verbatim, plurals and all; `Access::SET_FILE_COMMENT`/
    `SET_FOLDER_COMMENT` are the one known exception, since the
    reference's own text for those two ("You are not allowed set comments
    for files.") drops "to", which reads as its own typo rather than
    something worth reproducing. `AccessExtra`'s table has no reference
    text to match against, since those flags have no wire representation.
40. **`Access::DOWNLOAD_FILE`/`DOWNLOAD_FOLDER`/`UPLOAD_FOLDER`/
    `SET_FILE_COMMENT`/`SET_FOLDER_COMMENT` have known reference denial
    strings in `Access::actions` (item 39) but no `require()` call site
    reaches any of them yet.** ~~`on_file_get` (202) checks
    `AccessExtra::FILE_GETINFO` instead of `Access::DOWNLOAD_FILE`, which
    means download is currently ungated by that privilege entirely, a
    real permission gap rather than just an unimplemented feature.~~
    **Resolved for `on_file_get`, and now also for file/folder comments.**
    `on_file_get` now checks `Access::DOWNLOAD_FILE` instead of
    `AccessExtra::FILE_GETINFO` (the latter was a copy/paste from the
    separate metadata-only `on_file_getinfo` handler, left untouched).
    `FileProvider` gained `comment`/`set_comment`, implemented by both
    `InMemoryFileProvider` (an in-memory map keyed by path) and
    `FileFileProvider` (a `.comments.json` sidecar file at the served
    root, since a comment isn't native filesystem metadata); `on_file_setinfo`
    now persists a `FILE_COMMENT` field through `SharedState::set_file_comment`,
    gated on `Access::SET_FOLDER_COMMENT` for a folder target and
    `Access::SET_FILE_COMMENT` for a file, and `on_file_getinfo` returns a
    stored comment when one exists. Still open: folder transfer
    (`UPLOAD_FOLDER`/`DOWNLOAD_FOLDER`) isn't implemented at all
    (`HtxfType::FileTransfer` is the only transfer type this server
    sends/accepts) — a larger feature this backlog doesn't otherwise track
    yet. `file_comment`/`set_file_comment`'s drop-box denial used to
    return a hardcoded `"You are not allowed to do that."` instead of
    going through `RequireFlags::require`; `VIEW_DROP_BOXES` now has an
    `actions()` entry ("view drop boxes") like every other flag, and a
    `dropbox_access` helper wraps the existing `dropbox_blocked` check
    with `access.require(Access::VIEW_DROP_BOXES)` so the denial reads
    "You are not allowed to view drop boxes." through the same mechanism
    item 39 wired up for every other single-flag denial. `file_size`/
    `file_info`/`mkdir`/`rename_file` had the same `dropbox_blocked`-plus-
    hardcoded-string pattern from before this item (not newly introduced
    by it); they now route through `dropbox_access` too, for the same
    wording (`list_files` excepted, which silently empties instead of
    erroring, so has nothing to word). Every one of these functions, plus
    `delete_file`/`move_file`/`symlink_file`/`news_delete` (which mix a
    static "not found." literal with an owned `io::Error`/tree-mutation
    message), now returns `Result<T, Cow<'static, str>>` instead of
    `Result<T, String>`, matching `authorize_upload`/
    `authorize_chat_subject_change`'s existing use of the same type, so
    the static branches no longer allocate a copy of a literal that never
    changes. Both comment stores key by path (`InMemoryFileProvider` by
    `(dir components, name)`, `FileFileProvider` by those same components
    joined into one string), matching how a real HFS-comment-store
    implementation of this same feature identifies an entry, rather than
    by an inode/file-identity number: portable across `tokio::fs`-backed
    and pure in-memory backends alike, at the cost of a comment needing to
    be explicitly carried forward on rename/move instead of surviving for
    free. `rename`/`r#move` on both providers now do exactly that:
    `FileProvider::rename`/`r#move` re-key the renamed/moved entry's own
    comment, plus (if it's a folder) every descendant comment whose key
    had the old path as a prefix, so a comment no longer goes silently
    orphaned under its old path. Deleting a file or folder still leaves
    any of its comments in the store rather than pruning them, a known
    asymmetry: harmless (an orphaned comment is just inert data) unless a
    new, unrelated file or folder is later created at that exact same
    path, which would then inherit it. Not fixed here; flagged for a
    follow-up.
41. ~~Neither classic nor threaded news is persisted anywhere.~~
    **Resolved.** Added `NewsProvider` (`crate::news`), following the same
    plug-in shape as `AccountProvider`/`FileProvider`: `hotline-server`
    only defines the trait and an `InMemoryNewsProvider` default, and the
    embedding application supplies the real backend to `Server::new`.
    Flat text and the threaded tree are two independent `load`/`save`
    pairs on the trait, not one combined snapshot, matching how they're
    unrelated data with no shared invariant, kept in two separate files on
    disk by real server implementations. `SharedState` keeps no news
    cache of its own: every read calls `load_flat`/`load_tree` and every
    mutation does a `load`-mutate-`save` round trip, guarded by a
    `SharedState`-side lock per data kind so two concurrent mutations of
    the same kind can't silently drop one of their updates (a provider's
    own internal lock, if it has one, only ever protects a single
    `load`/`save` call, not the pair). `load`/`save` move an `Arc` rather
    than an owned value or a reference: a read-only call only ever needs
    to borrow the result, so `load` returning `Arc<str>`/`Arc<NewsBundle>`
    makes that a refcount bump instead of a deep clone of the whole tree,
    and a provider caching its own copy on `save` just stores the `Arc`
    it's handed rather than cloning it again. A posted article's date is
    stored as an ISO 8601 string (`crate::news::iso_date`) rather than its
    raw wire fields; `hotline-proto` stays free of a `serde` dependency,
    so this conversion lives on the persistence side that actually needs
    it. `hotline-daemon` supplies `FileNewsProvider`: flat news persists
    as a plain text file (the same bytes a client reads, no framing) and
    the threaded tree persists separately as a JSON file, matching the
    real split between an unstructured flat-text file and a structured
    tree that a widely used non-primary server implementation uses for
    these two kinds of news. A posted article's body is also stored as
    plain text rather than a byte array when it reads as one (`flavor`
    starts with `"text/"`, or the bytes are plain ASCII regardless of
    flavor), so a hand-inspected news file reads like the post it holds
    instead of an opaque array of numbers. `NewsCategory::guid` is a real
    random UUIDv4 (the `uuid` crate) rather than a process-local
    incrementing counter reinterpreted as 16 bytes, stored as the
    canonical hyphenated string.
42. ~~**All wire text is decoded as lossy UTF-8, not the classic client's
    actual encoding.**~~ **Resolved.** Added `hotline_proto::text_encoding`
    (built on `encoding_rs`): `DEFAULT` is a real Mac OS Roman transcoding,
    and any other WHATWG label (`utf-8`, `shift-jis`, `latin1`, ...) works
    via `Encoding::for_label`. Every `Field`/`Transaction`/`HlPath`/
    `FileNameWithInfo`/`UserNameWithInfo`/`NewsCategoryListEntry`/
    `NewsArticleListData` method that turns wire bytes into text or back,
    and `PacketBuilder::pstring` (via a `with_encoding` typestate), now
    takes the encoding explicitly, so a call site that forgets it fails to
    compile instead of writing the wrong wire bytes. The encoding is
    per-connection and live, not a fixed default: `hotline_codec::TransactionCodec`
    holds an `EncodingHandle` (`Arc<ArcSwap<&'static Encoding>>`, same
    shape as item 49) that `Session`/`HotlineClient` seed from `DEFAULT`
    at connect and read at every text call site. Nothing yet changes the
    encoding after connect; the hook is in place for whatever adds that.
43. ~~**No pluggable ban/mute list.**~~ **Resolved.** Added a
    `BanProvider` trait (`is_banned`/`ban`/`unban`, mirroring
    `AccountProvider`'s plug-in shape) and its in-memory default,
    `InMemoryBanProvider`, self-expiring a temporary ban rather than
    needing a background sweep. `Server::new` takes a fourth
    `bans: Arc<dyn BanProvider>`; `hls` wires in a JSON-file-backed
    provider (`hotline-daemon`'s `FileBanProvider`), the same shape as
    its account/comment persistence. Bans are keyed by a single exact IP
    address, not a range and not login: the reference server records
    bans against the connecting address, keeping perm bans as address
    ranges with a capped, oldest-evicted list and an admin-supplied
    description; there's no admin ban-management transaction wired up
    here yet to ever populate a range or description, so this only
    stores the single address `on_user_kick` bans, uncapped. Revisit the
    range/cap/description shape if an admin ban-list command gets added.
    `Server::run`'s accept loop rejects a banned address immediately,
    before the TRTP handshake, matching the reference server's own
    accept-time check. There's deliberately no separate check in
    `on_login`: the reference doesn't check bans there either, only at
    accept and (for already-connected sessions) when an admin's ban-list
    edit is saved, which this server has no equivalent trigger for yet.
    `on_user_kick` reads its `Options` (113) kick-to-ban bit into a
    `KickBan` enum and, before disconnecting the target, bans their
    address (permanently for `Options == 2`, for a fixed
    `TEMP_BAN_DURATION` of 30 minutes for `Options == 1` — hardcoded,
    not yet a `ServerConfig` knob) and sends them a `Politequit` push
    with the reference's own ban notice text before the disconnect,
    matching the reference's send-message-then-disconnect order rather
    than disconnecting first and banning after (which would leave a
    window for the target to reconnect before the ban took effect).
    `BanProvider::ban`/`unban` return `Result<(), String>`, matching
    `AccountProvider::put`'s shape; `FileBanProvider` only commits a
    change to its in-memory cache after the backing file write succeeds,
    so a failed persist can't silently drift from what's on disk.
    `unban` has no caller yet: there's no admin-facing unban command,
    so once imposed a ban can only be lifted by hand-editing
    `bans.json`. Needs a `USER_UNBAN`-shaped transaction and handler
    before it's reachable.
44. ~~**No pluggable flood/spam policy.**~~ **Resolved.** Added a
    `FloodPolicy` trait (`check`/`reset`, mirroring `BanProvider`'s
    plug-in shape) and two implementations: `NoFloodPolicy` (always
    allows) and `InMemoryFloodPolicy`, which accumulates points per
    transaction, linearly decaying them over time, and returns
    `FloodDecision::Throttle`/`Kick` once accumulated points cross a
    configurable threshold (`FloodConfig`: points per transaction, a
    chat/message override, decay time, and the two thresholds).
    `Server::new` takes a fifth `flood: Arc<dyn FloodPolicy>`; `hls`
    wires in `InMemoryFloodPolicy::default()`, so a stock server enforces
    flood limits out of the box (no CLI flags to tune or disable it yet).
    `Session::handle` checks it once per inbound transaction; a session
    without `AccessExtra::CAN_SPAM` gets `Throttle`'s transaction dropped
    and replaced with a `Msg` push carrying the same
    `RequireFlags::require`-generated denial text ("You are not allowed
    to bypass flood limits.") as any other privilege check, and `Kick`
    disconnects the session the same way `on_user_kick`'s plain-kick path
    does, via `RecipientMessage::Disconnect`. Accumulated state for a user is
    forgotten on disconnect.
45. **No pluggable admin/console-command layer.** Several `AccessExtra`
    fields exist purely to gate the secondary reference's in-chat admin
    commands (`/access`, `/0wn`, `/color`, `/users`, `/mon`, a debug
    command) - `USER_COLOR`, `USER_ACCESS`, `USER_0WN`, `IS_0WN3D`,
    `MANAGE_USERS`, `DEBUG`, and `ACCESS_VOLATILE` (whether a command-
    driven access change persists to the account) - and none of them do
    anything today because `on_chat` has no command parser at all, just
    a plain broadcast. If this server ever adds one, the command set
    itself is a natural fit for a provider (a `ChatCommandProvider` or
    similar, mapping a leading-slash token to a handler), the same shape
    as `AccountProvider`/`FileProvider`: `hotline-server` would define
    the trait and dispatch, the embedding application would supply which
    commands actually exist.
46. **Banner content has no provider seam either**, once item 38's banner
    support is built. The secondary reference reads banner bytes/type/URL
    from static server config; a `BannerProvider` (parallel to
    `FileProvider`) would let the embedding application serve a banner
    generated per-login or per-request instead of one fixed image.
47. ~~`on_chat_invite`/`_join`/`_part`/`_decline`/`_subject` never checked
    `AccessExtra::CHAT_PRIVATE` at all, though the field and its account-
    default (on) already existed.~~ **Resolved.** Each of those five
    handlers now requires `AccessExtra::CHAT_PRIVATE` up front, matching
    the secondary reference's dispatcher, which gates entry to every one
    of those opcode handlers on this same bit before the handler itself
    ever runs — including `ChatSubject` for the global room, where the
    reference's dispatch-level check doesn't distinguish room 0 from a
    private one. Not a bug, a primary/secondary divergence like item 18:
    the primary reference has no equivalent bit or check at all for any
    of these five opcodes.
48. ~~`on_private_msg` only checked `Access::SEND_MESSAGE`, with no
    `AccessExtra::MSG` fallback.~~ **Resolved.** `on_private_msg` now
    allows `AccessExtra::MSG` as an alternative to `Access::SEND_MESSAGE`,
    matching the secondary reference's own compatibility allowance for
    client versions whose account-editor dialog can't set `send_msgs` at
    all — a client bug the reference works around from the server side,
    not a redundant permission. Not a bug, a primary/secondary divergence
    like item 18: the primary reference has no such fallback since it has
    no `AccessExtra` concept at all.
49. `NewsProvider`'s tree/flat caches (`RwLock<Arc<NewsBundle>>`/
    `RwLock<Arc<str>>`) switched to `arc_swap::ArcSwap`. `RwLock` implied
    concurrent readers, but the caches are read far more than written;
    `ArcSwap` gives lock-free reads that never block a writer and vice
    versa, which fits that shape better than `RwLock`/`Mutex`. Flat news
    moved to `Arc<Box<str>>` at the same time, since `arc_swap::ArcSwap<T>`
    needs `T: Sized`; `Box<str>` over `Arc<String>` since the cache is
    replaced wholesale, never mutated in place, so it doesn't need
    `String`'s spare capacity or growth methods.

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
