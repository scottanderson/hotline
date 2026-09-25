# Near-term goals

Closing feature gaps versus openline, used for behavior parity. Backlog
below, in priority order.

## Known deliberate divergences from openline

- **`GETLIST` visibility timing.** Openline makes a raw TCP connection
  visible to `GETLIST` immediately at accept, before `Login` even arrives
  (blank name/icon until set). This server only becomes visible at `Login`
  time. Low priority.
- **`reply_error()` text doesn't match openline's error strings.** Openline
  uses short, sometimes cryptic strings (`"who?!?"`, `"Uh, no."`);
  this server uses friendlier text. Not changed yet — decide with the user
  whether exact-string parity matters for any known client first.
- **Posted news article title/poster/flavor length cap.** Openline caps
  these at a small fixed buffer and truncates silently past it; this
  server accepts up to the wire format's real 255-byte `pstring` limit
  and errors past that instead. More permissive by design; not changed.
- **Console commands differ from mhxd in small ways.** `/access <uid>`
  with no fields shows the target's privileges; mhxd has no such view.
  `/access` applies its fields all or nothing, but mhxd keeps the fields
  before an unknown key and notifies no one. `/0wn` ignores `color=`,
  `g0away=`, and `visible=`, since this server has no user color or
  invisibility. A denied command names the missing privilege through
  `RequireFlags::require`, not mhxd's fixed text.
- **A kicked client gets a drain window.** `ProcessTran_DisconnectUser`
  calls `Disconnect()` outright, with its own comment saying it avoids
  `StartDisconnect()` "because we want to be sure that the Client gets
  disconnected". This server instead reads and discards for
  `KICK_DRAIN_TIMEOUT` (3 seconds) so the client can display the
  `myTran_DisconnectMsg` (111) notice, then closes regardless.
- **Folder transfer drops some of openline's safeguards.** No
  disk-space check before a `myTran_UploadFldr` (213), no 2-minute idle
  kill once a folder upload's connection is open, and no skip for an
  item whose path is too long for openline's fixed 1024-byte buffer.
  A folder download also skips a symlinked folder in `hls`; openline
  resolves aliases and skips only one that loops back to an ancestor.
- **A running upload outlives its client.** openline's `ProcessUploads`
  and `ProcessFldrUploads` abort an upload, file or folder, once its
  client disconnects. Here only downloads have a cancel token, so an
  open upload connection keeps writing until it closes.
- **Posted news article flavor default.** Openline stores an empty
  string when a post omits `myField_NewsArtDataFlav`; this server
  defaults to `"text/plain"`. A real client always sends this field, so
  this only affects a hand-crafted request; not changed.

## Openline vs. mhxd divergences (resolved)

Where openline and mhxd disagree, this server matched mhxd in every case
found so far: chat room password/invite gating, chat subject checks, chat
invite reply behavior, dropbox path substring matching, login case
folding, agreement push branching, the `USER_GETLIST` access check,
`AccessExtra::CHAT_PRIVATE` gating chat-room membership opcodes, and
`AccessExtra::MSG_OVERRIDE` as an alternative to `Access::SEND_MESSAGE`.
One exception: `on_chat_create`'s invite-target handling matched neither and
was fixed toward openline's shape (loop over every `Uid`, one shared room,
skip undeliverable targets), since that fits this server's existing
one-shared-room model.

## Backlog, in priority order

1. **No live per-transfer-progress tracker.** `on_user_get_info`'s
   file-transfer sections always read `"None."`, with no in-progress
   transfer ever reported. The concurrent-download queue itself
   (Resolved) tracks which references hold an active slot, but not
   their byte-level progress; this needs its own tracker fed from
   `server/htxf.rs`'s actual read/write loop.

2. **A still-uploading (`.hpf`) file can't be touched by its plain name
   until it finishes.** `delete`/`rename`/`r#move`/`symlink` all resolve
   only the exact name passed in, so a resumable in-progress upload is
   invisible to those operations by design, not by omission — flagged in
   case that turns out to matter for a real client.

3. **Enum variant naming isn't consistently the openline constant's tail
   yet.** `FieldId`/`Opcode` already follow this (see AGENTS.md's Naming
   section); other enums need the same audit. First pass (every `pub enum`
   in `hotline-proto`/`hotline-server`/`hotline-client`, plus session.rs's
   private `KickBan`) found one real match and fixed it: `HtxfType::FileTransfer`
   renamed to `HtxfType::File`, matching the secondary reference's own
   `HTXF_TYPE_FILE` (the primary reference has no named constant for this
   field at all; `HtxfType::Banner` already matched `HTXF_TYPE_BANNER`).
   Everything else checked this pass — `TransferKind`/`TransferDirection`
   (`state.rs`), `BannerKind` (`banner.rs`), `NewsItemKind`/`NewsListKind`
   (`news.rs`), `KickBan` (`session.rs`) — turned out to be locally
   invented bookkeeping with no openline-named constant list behind them
   (news bundle/category distinction reuses the reference's generic
   `fsItemType_Folder`/`fsItemType_File` filesystem enum, which would be
   actively misleading to copy here), so left as is; same reasoning kept
   each of them in its current crate rather than moving to `hotline-proto`.
   Left open rather than marked resolved: re-run this audit once the rest
   of the backlog lands, since later items are likely to add more enums
   this pass didn't have to consider.

4. **Threaded news storage does not match openline's on-disk
   binary database format.** Openline stores each news category
   in its own file. Each file starts with an `HLNZ` header (version 2,
   `_NZ_SDBHeader`). The file also has a free-block bitmap table and an
   ID-to-offset hash table (`_NZ_SHashInfo`/`_NZ_SIDOffset`). Each
   article is a chain of fixed-size blocks: one `_NZ_SNewsItemBlock`
   index block, plus one `_NZ_SGenericBlock` block for each stored MIME
   type. Files sit at `<bundle>/<category>` on disk. A real sample file
   confirms this: it starts with `HLNZ`, version 2, block size 256.
   This server stores the whole news tree in one `news.json` file
   instead (`hotline-daemon/src/news.rs`,
   `FileThreadedNewsProvider`/`ThreadedNewsProvider`). A matching format
   needs three new parts: a binary codec (in `hotline-proto`'s style,
   pure encode/decode, no I/O, testable without a filesystem), a block
   allocator that can split and merge blocks when an article grows past
   one block, and a new `ThreadedNewsProvider` backend. This work
   mirrors `HotlineServNewsDatabase.cp`. It is a large, standalone
   effort, not a quick change. No known client reads the file format
   directly; only the wire protocol matters for interop. This task
   stays low priority for now.

5. **Open design question from news article/category JSON storage.**
   `NewsArticle::primary_flavor` picks `self.data.iter().next()` on a
   `BTreeMap`, so once an article legitimately holds more than one
   flavor, "primary" means alphabetically-first MIME type, not
   first-posted or preferred — harmless today (always exactly one
   entry) but worth deciding before multi-flavor posting is wired up.

6. **`on_newsfile_get`'s clamp can split a multi-byte character.**
   The reply is truncated to 65535 encoded bytes. Under the Mac OS
   Roman default every character is one byte, so this is invisible;
   under `utf-8` or `shift-jis` the last character can arrive as a
   partial sequence. Openline truncates bytes too, so this matches
   openline. Worth deciding only if a multi-byte encoding is
   ever made selectable per connection.

7. **Reassembly's memory bound is per connection, not global.**
   `MAX_PENDING_BYTES` caps one `TransactionCodec` at 64 MB, and
   nothing caps how many connections a server accepts. Total
   reassembly memory is therefore unbounded. Needs a connection limit
   or a shared budget, plus a decision on which.

8. **The tracker beacon counts logged-in users.** openline sends
   `mStats.connCount`, which counts a connection when it is accepted,
   before login (`HotlineServ.cp`).

9. **The download queue does not watch the `HTXF` socket.** A queued
   download that closes its connection keeps its place until it is
   promoted. A disconnect frees an active slot, but the transfer keeps
   running, so the active count can pass `max_downloads` for a while.

10. **No per-address connection limit.** openline refuses a connection
    past `nMaxConnectionsIP` (default 5) from one address
    (`HotlineServ.cp`). This also bounds item 5.

11. **No login timeout.** openline disconnects a client that has not
    logged in after 60 seconds (`HotlineServ.cp`).

12. **Flood control does not ban.** openline adds a 30-minute temporary
    ban for each of these: more than 10 consecutive connections from one
    address within 30 seconds, more than 120 transactions in a minute,
    five identical private messages, more than 20 messages in a minute,
    more than 10 KB of chat in a minute, and more than 25 name changes
    in a minute (`HotlineServ.cp`, `HotlineServTrans.cp`). This server
    only disconnects, from one points policy with fixed thresholds
    (`FloodConfig::default()`).

13. **`hls` cannot set the download limits.** `ServerConfig` has
    `max_downloads`/`max_downloads_per_client`, but `hls` has no flags
    for them.

14. **No activity log.** openline logs downloads, uploads, connections,
    and account changes, each behind its own option (`myOpt_Log*`).

15. **No HTTP tunnel listeners.** openline also listens on the base port
    plus 2 and plus 3 for HTTP-tunnelled clients (`HotlineServ.cp`).
    The tunnel is openline's `UHttpTransport.cp`, about 5,000 lines with
    `UHttpTransact.cp`. A client joins a `GET` and a `POST` connection by
    a GUID in the URL. Data then moves in coded `SData` blocks with
    padding. `HLProtocol.md` does not document it and mhxd does not
    implement it, so no client is at hand to test against.

16. **Small cleanups found in review.** The `*b"fldr"` check repeats
    across `state/shared.rs` and `file/provider.rs`; an `is_folder()`
    helper would fit. `start_folder_upload` repeats
    `authorize_upload`'s destination rule. `prepare_folder_upload_item`
    ignores a `mkdir` error. `FileTransfer` and `FolderTransfer` share
    `login`, `dir`, and `name`.

17. **`hlc` covers only chat and lists.** It logs in, shows the
    agreement, chats in public, and lists users, files, and tracker
    servers. It cannot transfer files or folders, read or post classic or
    threaded news, send private messages, use private chat rooms, get
    user info, administer accounts, or show the banner. Each area can be
    its own task.

## Resolved

1. `UserSelfinfo`'s `Access` field now sends the session's real privileges
   (`self.access.bits()`), matching openline.
2. `on_file_getinfo` intentionally omits `FileType`/`FileCreator`/
   `FileDateCreate`/`FileComment` — no real HFS metadata to source them
   from, matching a widely used non-HFS server's own trimmed field set.
3. `on_file_setinfo` no longer errors on an absent `FileNewName`; it just
   skips the rename and replies success.
4. `on_file_mkdir`/`on_file_getinfo` support the `FileName`-absent
   fallback via `Session::file_name_and_dir_from_path_tail`.
5. Threaded news implemented in `crate::news` (bundles/categories/
   articles, all eight `News*` opcodes, `NEWS_*` privilege gating).
   `NewsArticle::data` stores a map from MIME type to body, so it can
   already hold more than one flavor. The wire protocol still posts and
   asks for one flavor at a time; revisit `on_news_postthread`/
   `on_news_getthread` and `NewsArticleSummary::encode`'s flavor count if
   multi-flavor posting is ever needed.
6. HTXF resume support: `FILE_GET` honors `FileResumeData` on download;
   `FILE_PUT`'s `opts == 1` resumes an upload via a `.hpf` working-name
   convention (`FileProvider::finish_upload`/`partial_upload_name`),
   following mhxd's non-HFS codepath. Resource-fork (`"MACR"`)
   upload data needed no code change — already discarded by tag like
   mhxd's own data-fork-only storage.
7. Private-message/chat refusal and automatic response implemented:
   `on_user_change` persists refuse-message/refuse-chat/auto-response
   preferences; `Session::refusal_bounce` bounces or auto-replies on
   `on_private_msg`/`on_chat_invite`.
8. `FieldId::Options` (113) added (needed by item 7's bounce push);
   module docs updated to note it now carries different meanings per
   direction.
9. `USER_FLAGS` (112) now also sets `ALL_PRIVS`/`NO_PRIVS`, matching
   openline's own computed bits.
10. `HlPath::decode` now rejects a declared component count over 256
    before allocating (decoder-side safety bound, no reference to match).
11. `InMemoryFileProvider::delete`/`rename`/`r#move` no longer
    auto-vivify missing intermediate directories (`Dir::get_dir_mut_existing`);
    they return `NotFound` like openline instead.
12. `Ping` (500)'s "undocumented in `HLProtocol.md`" status folded into
    item 15/naming's resolution below; no separate fix needed.
13. Investigated restoring `const fn` on `Opcode`'s `From`/`Into`: not
    possible without duplicating every discriminant a second time or an
    unsafe transmute (`num_enum`'s catch-all data-carrying variant blocks
    `as` casts, `E0605`). Left as is.
14. `Session::handshake`'s mismatch already produces a diagnostic, via
    `HandshakeRejected` propagating through `Session::run`'s `?` to
    `server.rs`'s existing `tracing::warn!` on session error. No change
    needed.
15. Doc comments and `opcode_names!`/field-id tables now cite
    `HLProtocol.md`'s own `myTran_*`/`myField_*` names instead of
    openline's header `HTLC_HDR_*`/`HTLS_DATA_*` names; the two opcodes
    absent from `HLProtocol.md` (`GetUserList`/`SetUserList`, `Ping`) keep
    citing an openline header name, now called out explicitly in
    AGENTS.md's naming rule.
16. `ClientFieldId`/`ServerFieldId` and `ClientOpcode`/`ServerOpcode`
    collapsed into single `FieldId`/`Opcode` types, matching
    `HLProtocol.md`'s flat numbering; five real client/server name
    collisions surfaced and were split into distinctly-named variants
    (`ChatSend`/`ChatMsg`, etc).
17. Investigated `Cow<'_, str>` on `UserEntry`'s string fields: no real
    call site avoided a clone (`Session` and `UserEntry` are independently
    owned with no common borrow scope), reverted.
18. Chat room password/invite gating kept matching mhxd;
    `on_chat_create`'s invite-target handling fixed to match openline's
    one-shared-room, skip-undeliverable-targets shape (see "openline vs.
    mhxd" section above).
19. `on_chat_subject` kept matching mhxd (access check, membership
    requirement, room password).
20. `on_chat_invite` kept matching mhxd (always replies, requires prior
    membership, rejects a repeat invite).
21. `on_chat_part` now sends no reply on success, matching openline's
    own never-reply behavior.
22. `on_chat_decline` now sends no reply on success and broadcasts a
    decline notice to the room, matching openline.
23. `on_chat` now supports the emote/action style (`Style` == 1 renders
    as `"*** {name} ..."` instead of the normal prefix).
24. `on_msg_broadcast` now excludes the sender from the fan-out
    (`state.announce` instead of `state.publish`).
25. `on_get_file_name_list` now gates on `AccessExtra::FILE_LIST` instead
    of the unrelated `Access::DOWNLOAD_FILE`.
26. `on_file_delete`/`_move`/`_symlink` no longer route through the
    drop-box visibility check; only their type-specific privilege remains
    (mkdir/rename are unaffected, still gated).
27. `on_file_put`'s drop-box/upload-folder ancestor-substring match kept
    matching mhxd.
28. `on_file_get`'s reply now also sends raw `FileSize` alongside
    `TransferId`/`TransferSize`.
29. `on_newsfile_post` now rejects posts over 8192 bytes with openline's
    own error text before appending.
30. Logins are now case-folded to lowercase consistently at every entry
    point (create/read/modify/delete/lookup), avoiding openline's own
    filesystem-case-insensitivity-dependent inconsistency.
31. `on_login`'s immediate-join gate now also requires a non-zero icon,
    matching openline's `psUserName[0] && nIconID` check.
32. `on_login`'s agreement-push logic kept its two-way branch, matching
    mhxd over openline's three-way branch.
33. `on_login` now sends `Bannerid` (161) via the pre-existing
    `FieldId::Bannerid` variant.
34. `on_login` now always sends `ServerName`, empty when unconfigured.
35. `on_user_get_list`'s `AccessExtra::USER_GETLIST` gate kept as is.
36. `on_user_kick`'s plain kick now sends `RecipientMessage::Disconnect`
    instead of a `Politequit` push; a missing `Uid` defaults to 0 and
    still replies success; temp/perm ban options implemented (see
    resolved item "ban/mute list" below).
37. `on_user_get_info`'s reply reshaped to openline's own labels/section
    order and field set, plus an `Online` duration and `Icon`/`Version`
    lines not in either. File-transfer sections
    still always read `"None."` — needs the transfer-progress tracker
    from the open "HTXF transfer queue" backlog item.
38. Banner support implemented: `Opcode::BannerGet`/`Banner` added,
    login pushes a `ServerBanner` for `Vers` >= 151 clients, `BANNER_GET`
    streams the image over the HTXF connection unframed
    (`TransferKind::Banner`). `AccessExtra::BANNER_GET` is this server's
    own opt-in gate, on by default.
39. `RequireFlags::require` now renders each missing flag through a
    per-flag `actions()` denial-text table (`Access`/`AccessExtra`),
    matching openline's own fixed strings verbatim where one exists.
40. `on_file_get` now checks `Access::DOWNLOAD_FILE` (was
    `AccessExtra::FILE_GETINFO`, a copy/paste from the metadata-only
    handler). File/folder comments implemented end to end
    (`FileProvider::comment`/`set_comment`, `on_file_setinfo`/
    `on_file_getinfo`, both `InMemoryFileProvider` and
    `FileFileProvider`), gated on `Access::SET_FILE_COMMENT`/
    `SET_FOLDER_COMMENT` and routed through `RequireFlags::require` via a
    `dropbox_access` helper. Fallible `state.rs` functions in this area
    now return `Result<T, Cow<'static, str>>` instead of `String`.
    Deleting a file/folder still leaves its comment orphaned in the store
    rather than pruning it — harmless unless something new is later
    created at the exact same path; flagged, not fixed.
41. Classic and threaded news are now persisted via a pluggable
    `NewsProvider` trait (`crate::news`), same plug-in shape as
    `AccountProvider`/`FileProvider`; `hotline-daemon` supplies
    `FileNewsProvider` (flat text file + JSON tree file).
42. Wire text now decodes/encodes through a real per-connection
    `Encoding` (`hotline_proto::text_encoding`, Mac OS Roman default via
    `encoding_rs`) instead of lossy UTF-8 everywhere.
43. Pluggable ban/mute list added (`BanProvider`, `InMemoryBanProvider`,
    `hotline-daemon`'s `FileBanProvider`), keyed by exact IP address;
    `on_user_kick`'s ban options now actually ban (temp 30 min / perm).
    No admin-facing unban command yet — `unban` has no caller.
44. Pluggable flood/spam policy added (`FloodPolicy`,
    `InMemoryFloodPolicy`), checked once per inbound transaction in
    `Session::handle`; throttle drops the transaction with a denial `Msg`,
    kick disconnects via `RecipientMessage::Disconnect`.
45. Banner content now has a provider seam too (`BannerProvider`,
    `NoBanner`/`UrlBannerProvider`/`hotline-daemon`'s `FileBannerProvider`),
    resolved together with item 38 above.
46. `on_chat_invite`/`_join`/`_part`/`_decline`/`_subject` now all require
    `AccessExtra::CHAT_PRIVATE` up front, matching mhxd's dispatch-level
    gate.
47. `on_private_msg` now allows `AccessExtra::MSG_OVERRIDE` as an alternative to
    `Access::SEND_MESSAGE`, matching mhxd's client compatibility
    allowance.
48. `NewsProvider`'s tree/flat caches switched from `RwLock` to
    `arc_swap::ArcSwap` (lock-free reads, read-heavy access pattern).

49. A pluggable console-command layer added (`ChatCommandDispatcher`,
    `console/`). The crate defines the trait and a `DefaultChatCommands`
    implementation, and the embedding application can supply its own.
    Console commands are opt-in: `ServerConfig::console_prefix`
    (`Option<char>`, `None` by default) plays the role of mhxd's
    `hxd_cfg.operation.commands` switch. Unset, every message relays as
    chat. `Some(c)` dispatches a message that starts with `c`, and `hls`
    exposes it as `--console-prefix`. Replies go to the sender only, like
    mhxd. An error is a `myTran_ChatMsg` (106) line `<command>: <text>`,
    like `cmd_err`, in the command's room if it has one. An unknown
    command replies `command not found`, like `cmd_notfound`. `/users`
    sends mhxd's table as a `myTran_ServerMsg` (104) from `hxd`, like
    `cmd_users`. A successful `/access` or `/0wn` sends nothing. Command
    names match case-insensitively, unlike mhxd's `strncmp`; this is
    deliberate.
    `/users`, `/access`, and `/0wn` are implemented. A target is a user ID
    or a nickname, found like mhxd's `nick_to_uid`. `/access` matches keys
    exactly and stops at a word with no `=`. `/0wn` matches keys by
    prefix, reads `icon=` like `strtoul`, and caps `name=` at 31 bytes.
    `/color` has no equivalent field to port, and `/mon` is an
    undocumented mhxd debug command. `/access` takes and shows mhxd's own
    console keys (`delete_files`, `cant_be_disconnected`) from
    `Access::CONSOLE_NAMES`. `Access::BROADCAST` has no key, since mhxd's
    `set_access_bit` has no case for it. mhxd has no `/access <uid>` view,
    so that display is our own. Field parsing and mutation live in
    `SharedState::apply_access_fields`/`apply_0wn_fields`, and each change
    logs one "console change" event, like mhxd.
    `access_extra.rs`'s `actions()` lists neither `ACCESS_VOLATILE` nor
    `IS_0WN3D`: both are target-eligibility checks that never reach
    `RequireFlags::require`. AGENTS.md records this exception.

50. `on_chat` now checks `Access::SEND_CHAT` before testing for the
    console prefix, matching mhxd's own dispatch-table gate (`send_chat`
    gates the whole chat opcode, including any
    console command inside it), so a chat-muted user can no longer
    reach a console command.

51. `hotline-daemon`'s provider `open()` calls now wrap each `io::Error`
    with `anyhow::Context::with_context`, naming both the flag and its
    value (`--accounts-dir`, `--files-dir`/`--comments-file`,
    `--news-file`/`--news-tree-file`, `--ban-file`, `--banner`), so a
    missing or unreadable file says which flag to fix instead of
    reporting a bare OS error.

52. A polite kick now removes the user from state at once, then gives the
    client a bounded (`KICK_DRAIN_TIMEOUT`, 3 seconds) chance to drain
    its `myTran_DisconnectMsg` (111) push before the connection closes.
    Before, the connection closed right after the send. openline closes
    at once, so the drain is a deliberate divergence. The silent-kick
    path (`RecipientMessage::Disconnect`) is unchanged.

53. `post_article` now rejects a reply chain longer than 32 articles,
    and `mkdir` rejects a bundle nested deeper than 16. The deepest
    legal tree stays within serde_json's nesting limit of 128, so
    `news.json` always loads back.

54. News category `flatten` now rejects a duplicate article id. A
    duplicate would overwrite an earlier article's slot, leaving
    `NewsCategory::depth_of` stuck in a `parent_id` cycle; it now fails
    `FileThreadedNewsProvider::open` outright instead. `serde_json`'s
    text parser caps nesting, so a hand-edited `news.json` nested too
    deep fails to parse cleanly instead of overflowing the stack. A test
    pins that down.

55. `news.txt` keeps the wire's bare `\r` separators verbatim, like
    openline's `MessageBoard.txt` and mhxd's `news.c`. A `\r` to `\n`
    translation for text editors was rejected: it is not injective, so a
    `\n` inside a post body would come back as a separator after a
    restart. A test pins the byte-for-byte round trip.

56. `post_news` now matches openline's `AddToMsgBoard` byte for byte: a
    header with poster and timestamp, then the body, then a footer
    divider. The `<` byte previously flagged in the backlog is a C
    string length-prefix artifact in openline's source; it is never
    written to the board.
    Wall-clock time comes from the new `chrono` dependency.

57. `on_newsfile_get` clamps its reply to 65535 bytes, matching
    openline's `if (s > max_Uint16) s = max_Uint16;`
    (`HotlineServTrans.cp`) and mhxd's `MAX_NEWS_SIZE`. A field's length
    is a `u16`, so a larger board previously failed to encode and the
    client got nothing. Posts are prepended, so the clamp keeps the
    newest. mhxd goes further and truncates the stored file on every
    post (`news_save_post` reads back at most `MAX_NEWS_SIZE`); this
    server follows openline and keeps the whole file on disk.

58. `UserNameWithInfo::flags` is now typed `UserFlags` instead of a raw
    `u16`; every call site uses `UserFlags`'s own methods, with
    `.bits()`/`from_bits_retain` kept only at the wire encode/decode
    boundary.

59. `PendingTransfer`'s file-specific fields (`login`, `dir`, `name`,
    `raw`) moved into a `FileTransfer` struct, held as
    `TransferKind::File(FileTransfer)`, and `TransferKind::Banner` is now
    bare. `server/htxf.rs` matches the kind once and passes the struct
    down, so `send_file`/`receive_file` need no `unreachable!` arm.

60. `hotline-cli`'s `App` split into `session::ClientSession` (session
    state) and `main.rs`'s loop-local `input`/`should_quit` (terminal
    state). `EventOutcome::Quit` lets a server `Politequit` push end the
    loop, so `ClientSession` owns no quit flag. `session/` holds one type
    per file.

61. `hotline_codec::TransactionCodec` now reassembles a split
    transaction (`len` != `len2`). The prefix guard ports all four terms
    of openline's own check (`UTransact.cp`), zero sizes included, and a
    whole transaction that reuses a pending `trans` appends to that
    partial rather than decoding on its own. Each part's total replaces
    the pending one, like openline's `UTransact.cp`, which sets
    `totalSize` from every part's header. The size cap is per direction:
    `SERVER_MAX_TRANSACTION_LEN` (512 KB, openline's server override) and
    `CLIENT_MAX_TRANSACTION_LEN` (2 MB, its transport default).
    Pending partials are bounded in bytes: `MAX_PENDING_BYTES` (64 MB)
    over the per-id ceiling, which is just under twice the size cap. That
    allows 64 ids on a server and 16 on a client. Both bounds are ours;
    openline caps neither. `transaction_codec/` holds one type per file.

62. Split `NewsProvider` into `FlatNewsProvider` and
    `ThreadedNewsProvider`. Each method is now one storage operation, not
    a whole-blob load/save pair. Each provider owns its own lock, so
    `SharedState`'s `news_flat_lock`/`news_tree_lock` are gone. A step
    toward the binary news format backlog item. `FlatNewsProvider::post`
    prepends, like openline's `UMemory::Insert(mMsgData, 0, ...)`.

63. `SharedState::new`/`Server::new` now take a single `Providers`
    struct instead of eight positional arguments, dropping
    `#[allow(clippy::too_many_arguments)]`. A new provider now touches
    one field, not every call site's argument list.

64. `AccessExtra::MSG` renamed to `MSG_OVERRIDE`. Its config key stays
    mhxd's own `msg`, because `AccessExtra` now carries a real
    `CONSOLE_NAMES` table like `Access` does, instead of deriving the key
    from the constant name. That drops the coupling that made this a
    migration question at all: the Rust name and the config key move
    independently now. The on-disk account JSON does use the new name
    (`bitflags_serde` writes Rust identifiers); an account file holding
    the old `"MSG"` fails to load with `unknown flag "MSG"` rather than
    silently dropping the bit. This code is unpublished, so no stored
    account anywhere holds the old name and no migration shim is needed.

65. `Session::run` now cleans up on every exit. Its select loop moved
    into `Session::dispatch`, returning a `SessionEnd`; `run` calls
    `leave_server` unconditionally after it, then drains only on
    `SessionEnd::Kicked`. Before, an early `?` (a malformed transaction
    through `frame.transpose()?`, or a failed `writer.send`) returned
    past the cleanup, leaving the `UserEntry`, the `recipients` sender,
    and the flood state behind and never broadcasting
    `NotifyDeleteUser`. Every other client then saw a user nobody could
    reach, and `/access` against that id reported success. A logged-in
    client reached this with one zero-length transaction.

66. `state/recipient.rs`'s `is_disconnect` is now a `const fn`, so
    `cargo clippy --workspace --all-targets` is warning-free again.

67. `AccountProvider::put`/`delete`, `BanProvider::ban`/`unban`, and
    `SharedState::put_account`/`delete_account`/`ban` now return
    `Cow<'static, str>` instead of `String`, so `delete_account`'s
    `"no such account"` literal no longer allocates.
    `Session::apply_user_list_record` follows.

68. `HTRK` tracker support is wired end to end. `hls --tracker
    [password@]host[:port]` beacons over UDP every 300 seconds (openline's
    `mTrackerTimer`), and `hlc --tracker host[:port]` lists a
    tracker's servers and exits. Two wire bugs came out of it:
    `ServerBeacon` encoded a `login` pstring that no tracker reads
    (openline's `ProcessRegister` and mhxd's `tracker_register_timer`
    both expect name, description, password only), and its fourth
    `u16` is openline's `flags`, not padding. openline's server always
    sends 0 there, and nothing in openline or mhxd reads it.
    `decode_server_list` is new: nothing could read past the 6-byte
    reply header before, and a reply is one or more chunks, so stopping
    at the first would drop servers. Each chunk is framed by its own
    `data_size`, since mhxd's split path can emit a chunk whose size
    and entry count disagree; a `data_size` under 4 is rejected,
    because `hlc --tracker` reads these bytes from an unauthenticated
    peer and a smaller value would make the entry slice run backwards.
    A tracker can also claim more servers than it sends, so `hlc`
    returns the ones that arrived rather than discarding them at the
    deadline. `hls` checks its beacon at startup: an empty `--name`,
    which openline's `RegisterServer` drops, and an over-long field
    both fail the process instead of registering nothing.
    `pass_id` keeps a server's listed name when its address changes.
    Like openline, `hls` saves a random nonzero ID on first run
    (`--tracker-id-file`), and `--tracker-id` overrides it.
    `--tracker` takes any number of trackers. openline registers with
    at most 5, the number of tracker fields in its settings window.

69. `session/connection.rs` split into per-area sibling files, each a
    separate `impl Session` block: `chat.rs`, `users.rs`, `files.rs`,
    `news.rs`, `accounts.rs`, plus `test_support.rs` for the shared
    test fixtures. `connection.rs` keeps the struct, `run`, the
    handshake, and the dispatch tables. No code changed; every
    function and test moved verbatim.

70. Concurrent-download queue implemented (`state/download_queue.rs`):
    `ServerConfig::max_downloads`/`max_downloads_per_client` (openline's
    own defaults, 100/10, `HotlineServ.cp`'s `mOptions.nMaxDownloads`/
    `nMaxDownloadsClient`), a waiting list in `SharedState` past either
    cap, `WaitingCount` (116) on `on_file_get`'s own reply when queued,
    and `myTran_DownloadInfo` (211) pushes as a queued download's
    position changes or it is promoted to an active slot. This follows
    openline's soft-queue shape (no hard reject). mhxd rejects uploads
    past a limit and openline never caps them; this server leaves uploads
    uncapped, like openline. Promotion is event-driven (on a slot's
    release or a user's disconnect), not polled on a timer like
    openline's `ProcessDownloadsWaiting`/mhxd's `queue_refresher`. An
    admitted download gets a 500-second grace period to open its `HTXF`
    connection, like openline's idle check and its `startSecs` reset at
    promotion. A download whose connection never arrives, arrives from
    the wrong address, or whose user disconnects leaves the queue,
    active or queued, and a queued waiter wakes and ends.

71. Folder transfer (`myTran_DownloadFldr`/`myTran_UploadFldr`, 210/213)
    implemented from openline alone, since mhxd never implemented it:
    `ProcessTran_DownloadFldr`/`ProcessTran_UploadFldr`
    (`HotlineServTrans.cp`), `ProcessFldrDownloads`/`ProcessFldrUploads`
    (`HotlineServ.cp`), and `CMyDLFldr` (`HotlineFolderDownload.cp`).
    `FileProvider::walk` lists a folder in pre-order; a download skips
    every drop box `VIEW_DROP_BOXES` doesn't cover at any depth, like
    `CMyDLFldr`'s constructor. 210 replies with the plain sum of file
    sizes and the item count, root not counted, and shares the
    concurrent-download queue (72). 213 creates the folder in the
    handler. On the `HTXF` connection, `server/htxf.rs` steps through
    items with `dlFldrAction_*`: a download sends each item's header on
    `NextFile` and a file's size and flattened object on
    `SendFile`/`ResumeFile`; an upload asks for each header with
    `NextFile`, resumes a file with a `.hpf` working copy, skips a
    finished one, and stops after the declared item count, sending no
    trailing `NextFile` only after a received last file. `Access::DOWNLOAD_FOLDER`/`UPLOAD_FOLDER` now
    have their first `require()` call sites.
    An upload reference, file or folder, expires after 500 s if its
    `HTXF` connection never arrives, like `ProcessUploads`/
    `ProcessFldrUploads`.

72. `myTran_KillDownload` (214) implemented, matching openline's
    `ProcessTran_KillDownload` (`HotlineServTrans.cp`) and
    `KillDownloadByRefNum` (`HotlineServ.cp`). It reads `myField_RefNum`
    (107) and always replies with no error. Only the owner's own download
    is dropped, queued or running. A running `HTXF` send stops, a pending
    reference refuses its later connection, and the queue promotes the
    next download.

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
