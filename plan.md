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
  disk-space check before a `myTran_UploadFldr` (213), and no skip for
  an item whose path is too long for openline's fixed 1024-byte buffer.
  A folder download also skips a symlinked folder in `hls`; openline
  resolves aliases and skips only one that loops back to an ancestor.
- **Any received byte restarts a folder upload's idle timer.** openline's
  `ProcessFldrUploads` restarts it only while file data arrives, not
  while item headers do.
- **An `HTXF` connection must send its header within 60 seconds.**
  openline has no such timer. Without it, silent sockets on the transfer
  port, which the connection limits do not cover, stayed open forever.
- **Connection limits count from accept.** openline counts a
  connection against `nMaxConnectionsIP` only after its handshake, and
  it closes an over-limit connection after it accepts the handshake.
  This server counts every accepted socket, so a connection that never
  sends a handshake still uses a place. An over-limit socket closes at
  once, before the handshake. This server also has a limit on all
  connections together, `max_connections` (500 by default). openline
  has no such limit.
- **The login timeout is a fixed deadline from accept.** openline
  closes a client that has not logged in 60 seconds after its last
  transaction. This server closes it 60 seconds after accept, like
  mhxd's `login_timeout` (10 seconds there), and activity does not
  extend it. openline disconnects on any pre-login opcode except
  `myTran_Login` (107) and `myTran_Agreed` (121), but this server
  answers `myTran_KeepConnectionAlive` (500) before login, so a
  resetting timer would let keep-alives hold a connection forever.
- **Flood bans differ from openline in small ways.** A flood ban goes
  through `BanProvider`, so `hls` keeps it in `bans.json` across a
  restart; openline keeps at most 32 in memory. The repeated-text check
  hashes a classic news post's body; openline hashes the stored entry,
  whose header has the poster and the minute. `AccessExtra::CAN_SPAM`
  (mhxd's `can_spam`) exempts an account; openline has no exemption.
  The connection flood check counts every accepted socket. openline
  counts a connection only after its handshake.
- **`hls` account files keep passwords as plain text.** openline
  scrambles each password byte with bitwise NOT in its data file
  (`HotlineServTrans.cp`). This hides a password only from a casual
  reader, so `hls` does not scramble them.
- **A rename onto a taken login changes nothing.** openline's
  `ProcessTran_SetUserList` fails the folder rename, then still writes
  the record over the other account with `SetUser`. This server skips
  the record and leaves both accounts as they were. Both reply
  "Error saving list of accounts."
- **Activity logs use a different date and line format.** openline
  writes the Mac short date and `\r` line endings in the classic Mac
  text encoding. `hls` writes `YYYY-MM-DD HH:MM:SS` in local time with
  `\n` line endings in UTF-8. It also replaces a tab or control
  character in any column with a space, so a name or a file name cannot
  add a column or a line. openline writes each line before it replies;
  `hls` queues the line for a writer task, and writes the queue before
  it exits on Ctrl-C or SIGTERM.
- **Posted news article flavor default.** Openline stores an empty
  string when a post omits `myField_NewsArtDataFlav`; this server
  defaults to `"text/plain"`. A real client always sends this field, so
  this only affects a hand-crafted request; not changed.
- **Stored text is UTF-8.** openline stores text in the wire encoding.
  `hls` stores threaded news text bodies, like its other text, as
  UTF-8, and converts each one to the connection's encoding on the
  wire. A `myTran_GetMsgs` (101) reply that is too long is cut at a
  whole character; openline cuts at a byte.
- **`hls` replies to `myTran_InviteToChat` (113) and
  `myTran_SetChatSubject` (120).** openline's `ProcessTran_InviteToChat`
  and `ProcessTran_SetChatSubject` send no reply, and its client does
  not wait for one. `hls` replies, with an error when the request
  fails. `hlc` sends both without waiting and shows an error reply.

## Openline vs. mhxd divergences (resolved)

Where openline and mhxd disagree, this server matched mhxd in every case
found so far: chat room password/invite gating, chat subject checks, chat
invite reply behavior, dropbox path substring matching, login case
folding, agreement push branching, the `USER_GETLIST` access check,
`AccessExtra::CHAT_PRIVATE` gating chat-room membership opcodes, and
`AccessExtra::MSG_OVERRIDE` as an alternative to `Access::SEND_MESSAGE`.
One exception: `on_invite_new_chat`'s invite-target handling matched neither and
was fixed toward openline's shape (loop over every `Uid`, one shared room,
skip undeliverable targets), since that fits this server's existing
one-shared-room model.

## Backlog, in priority order

1. **Threaded news storage does not match openline's on-disk
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

2. **No HTTP tunnel listeners.** openline also listens on the base port
   plus 2 and plus 3 for HTTP-tunnelled clients (`HotlineServ.cp`).
   The tunnel is openline's `UHttpTransport.cp`, about 5,000 lines with
   `UHttpTransact.cp`. A client joins a `GET` and a `POST` connection by
   a GUID in the URL. Data then moves in coded `SData` blocks with
   padding. `HLProtocol.md` does not document it and mhxd does not
   implement it, so no client is at hand to test against.

## Resolved

1. `UserSelfinfo`'s `UserAccess` field now sends the session's real privileges
   (`self.access.bits()`), matching openline.
2. `on_get_file_info` intentionally omits `FileTypeString`/`FileCreatorString`/
   `FileCreateDate`/`FileComment` — no real HFS metadata to source them
   from, matching a widely used non-HFS server's own trimmed field set.
3. `on_set_file_info` no longer errors on an absent `FileNewName`; it just
   skips the rename and replies success.
4. `on_new_folder`/`on_get_file_info` support the `FileName`-absent
   fallback via `Session::file_name_and_dir_from_path_tail`.
5. Threaded news implemented in `crate::news` (bundles/categories/
   articles, all eight `News*` opcodes, `NEWS_*` privilege gating).
   `NewsArticle::data` stores a map from MIME type to body, so it can
   already hold more than one flavor. The wire protocol still posts and
   asks for one flavor at a time; revisit `on_post_news_art`/
   `on_get_news_art_data` and `NewsArticleSummary::encode`'s flavor count if
   multi-flavor posting is ever needed.
6. HTXF resume support: `myTran_DownloadFile` (202) honors
   `FileResumeData` on download; `opts == 1` on `myTran_UploadFile` (203)
   resumes an upload via a `.hpf` working-name
   convention (`FileProvider::finish_upload`/`partial_upload_name`),
   following mhxd's non-HFS codepath. Resource-fork (`"MACR"`)
   upload data needed no code change — already discarded by tag like
   mhxd's own data-fork-only storage.
7. Private-message/chat refusal and automatic response implemented:
   `on_set_client_user_info` persists refuse-message/refuse-chat/auto-response
   preferences; `Session::refusal_bounce` bounces or auto-replies on
   `on_send_instant_msg`/`on_invite_to_chat`.
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
    `on_invite_new_chat`'s invite-target handling fixed to match openline's
    one-shared-room, skip-undeliverable-targets shape (see "openline vs.
    mhxd" section above).
19. `on_set_chat_subject` kept matching mhxd (access check, membership
    requirement, room password).
20. `on_invite_to_chat` kept matching mhxd (always replies, requires prior
    membership, rejects a repeat invite).
21. `on_leave_chat` now sends no reply on success, matching openline's
    own never-reply behavior.
22. `on_reject_chat_invite` now sends no reply on success and broadcasts a
    decline notice to the room, matching openline.
23. `on_chat_send` now supports the emote/action style (`ChatOptions` == 1 renders
    as `"*** {name} ..."` instead of the normal prefix).
24. `on_user_broadcast` now excludes the sender from the fan-out
    (`state.announce` instead of `state.publish`).
25. `on_get_file_name_list` now gates on `AccessExtra::FILE_LIST` instead
    of the unrelated `Access::DOWNLOAD_FILE`.
26. `on_delete_file`/`_move`/`_symlink` no longer route through the
    drop-box visibility check; only their type-specific privilege remains
    (mkdir/rename are unaffected, still gated).
27. `on_upload_file`'s drop-box/upload-folder ancestor-substring match kept
    matching mhxd.
28. `on_download_file`'s reply now also sends raw `FileSize` alongside
    `RefNum`/`TransferSize`.
29. `on_old_post_news` now rejects posts over 8192 bytes with openline's
    own error text before appending.
30. Logins are now case-folded to lowercase consistently at every entry
    point (create/read/modify/delete/lookup), avoiding openline's own
    filesystem-case-insensitivity-dependent inconsistency.
31. `on_login`'s immediate-join gate now also requires a non-zero icon,
    matching openline's `psUserName[0] && nIconID` check.
32. `on_login`'s agreement-push logic kept its two-way branch, matching
    mhxd over openline's three-way branch.
33. `on_login` now sends `CommunityBannerID` (161) via the pre-existing
    `FieldId::Bannerid` variant.
34. `on_login` now always sends `ServerName`, empty when unconfigured.
35. `on_get_user_name_list`'s `AccessExtra::USER_GETLIST` gate kept as is.
36. `on_disconnect_user`'s plain kick now sends `RecipientMessage::Disconnect`
    instead of a `Politequit` push; a missing `Uid` defaults to 0 and
    still replies success; temp/perm ban options implemented (see
    resolved item "ban/mute list" below).
37. `on_get_client_info_text`'s reply reshaped to openline's own labels/section
    order and field set, plus an `Online` duration and `Icon`/`Version`
    lines not in either. File-transfer sections
    still always read `"None."` — needs the transfer-progress tracker
    from the open "HTXF transfer queue" backlog item.
38. Banner support implemented: `Opcode::BannerGet`/`Banner` added,
    login pushes `myTran_ServerBanner` (122) for `Vers` >= 151 clients,
    and `myTran_DownloadBanner` (212) streams the image over the HTXF
    connection unframed
    (`TransferKind::Banner`). `AccessExtra::BANNER_GET` is this server's
    own opt-in gate, on by default.
39. `RequireFlags::require` now renders each missing flag through a
    per-flag `actions()` denial-text table (`Access`/`AccessExtra`),
    matching openline's own fixed strings verbatim where one exists.
40. `on_download_file` now checks `Access::DOWNLOAD_FILE` (was
    `AccessExtra::FILE_GETINFO`, a copy/paste from the metadata-only
    handler). File/folder comments implemented end to end
    (`FileProvider::comment`/`set_comment`, `on_set_file_info`/
    `on_get_file_info`, both `InMemoryFileProvider` and
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
    `on_disconnect_user`'s ban options now actually ban (temp 30 min / perm).
    No admin-facing unban command yet — `unban` has no caller.
44. Pluggable flood/spam policy added (`FloodPolicy`,
    `InMemoryFloodPolicy`), checked once per inbound transaction in
    `Session::handle`; throttle drops the transaction with a denial `Msg`,
    kick disconnects via `RecipientMessage::Disconnect`.
45. Banner content now has a provider seam too (`BannerProvider`,
    `NoBanner`/`UrlBannerProvider`/`hotline-daemon`'s `FileBannerProvider`),
    resolved together with item 38 above.
46. `on_invite_to_chat`/`_join`/`_part`/`_decline`/`_subject` now all require
    `AccessExtra::CHAT_PRIVATE` up front, matching mhxd's dispatch-level
    gate.
47. `on_send_instant_msg` now allows `AccessExtra::MSG_OVERRIDE` as an alternative to
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

50. `on_chat_send` now checks `Access::SEND_CHAT` before testing for the
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

57. `on_get_msgs` clamps its reply to 65535 bytes, matching
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
    cap, `WaitingCount` (116) on `on_download_file`'s own reply when queued,
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

73. Re-ran the enum variant naming audit, a since-closed backlog item,
    over every `pub enum` added since the first pass: `RecipientMessage`/
    `FloodDecision`/`TransferDirection` (reshaped `TransferKind`)/
    `ServerError` (`hotline-server`), `ClientError` (`hotline-client`),
    `NewsCategoryListEntry` (`hotline-proto`), `NewsNode` (`hotline-server`),
    and the console-command/`hotline-cli` types (`Mode`/`EventOutcome`/
    `LineKind`). None correspond to an openline-named constant list:
    `NewsCategoryListEntry::Bundle`/`Category` already match
    `HLProtocol.md`'s own prose ("Bundle (2) or category (3)") for
    `myField_NewsCatListData15` (323), the only one of these with wire
    values behind it. Everything else is local bookkeeping with no
    openline name to match. No renames needed this pass.

74. `hls` no longer seeds `admin` with the password `admin`. When
    `hls` finds an empty `--accounts-dir`, it creates `admin` with 16
    random hex digits and `guest`, and prints the password once.
    `InMemoryAccountProvider` keeps its fixed seed for tests and embedders.

75. The accept loop now limits open connections, in total and for each
    address: `ServerConfig::max_connections` (500) and
    `max_connections_per_address` (5, openline's `nMaxConnectionsIP`),
    with `hls` flags for both. 0 means no limit, like openline's
    `!nMaxConnectionsIP ||` check. An over-limit connection closes with
    no message, like openline. This also bounds total reassembly memory,
    since each connection's `TransactionCodec` holds at most 64 MB.

76. A connection that has not logged in 60 seconds after accept now
    closes (`ServerConfig::login_timeout`). The deadline covers the
    handshake too, which had no timeout before.

77. Transfers no longer outlive their client. `PendingTransfer` records
    its session's `user_id`. A file or folder upload stops when that
    session ends, and an upload whose session has already ended is
    refused, like openline's `!mClientList.IsInList` check in
    `ProcessUploads`/`ProcessFldrUploads`. An upload also stops after
    120 seconds with no byte received, like openline. A queued download
    whose `HTXF` connection closes now leaves the queue at once, like
    `ProcessDownloadsWaiting`. A new `HTXF` connection must send its
    header within 60 seconds.

78. Flood control now follows openline's rules (`HotlineServ.cp`,
    `HotlineServTrans.cp`) in place of the old points policy and its
    throttle. Each rule disconnects the client and bans its address
    for 30 minutes (`kBanTimeLimitSecs`). Each count uses a fixed
    60-second window: more than 120 transactions, more than 20 private
    messages, broadcasts, and chat invites together, more than 10240
    bytes of relayed chat (each line counts 90 to 8192 bytes), and more
    than 25 name or icon changes. A 6th identical chat line, private
    message, broadcast, or news post in a row also bans, as does a 12th
    connection in a row from one address within 30 seconds of the first.
    Each check sits where openline makes it: before the access check for
    a private message or chat invite, and after it for a broadcast. A
    chat line tests the byte total before the access check and adds its
    own bytes after it. `FloodPolicy::check` now takes a `FloodEvent`, and
    `check_connection` covers the accept loop.

79. `/unban <address>` lifts an address ban, giving `BanProvider::unban`
    its first caller (item 43). It needs `Access::DISCON_USER`, the
    privilege that bans through `myTran_DisconnectUser` (110), so
    `ChatCommandActor` now carries `access` too. mhxd has no such
    command. Like `/access`, success replies nothing.

80. `hls --max-downloads` and `--max-downloads-per-client` set
    `ServerConfig`'s download limits, defaulting to openline's 100 and 10.

81. An `ActivityLog` provider receives openline's four activity logs:
    a completed login, a download, an upload, and an account change
    (create, delete, modify, or rename). Each entry is recorded where
    openline records it: after the change succeeds, not for an empty
    folder download, and for an empty folder upload. A rename through
    `myTran_SetUserList` (349) records "Rename" and then "Modify", like
    openline. `hls --download-log`, `--upload-log`, `--connect-log`,
    and `--account-log` each append openline's tab-separated columns to
    a file, with openline's header first. `hls` checks each path at
    startup.
    A connect is recorded at `myTran_Login` (107) only with a name and
    an icon, and always at `myTran_Agreed` (121).

82. A `myTran_SetUserList` (349) rename saves the new login before it
    deletes the old one. If the save fails, the account keeps its old
    login. Before, it deleted the old login first, so a failed save lost
    the account.

83. A struct field that holds a string built once and then only read or
    replaced is a `Box<str>`: accounts, user entries, sessions, chat
    rooms, news items, transfer targets, activity entries, the wire
    types, tracker entries, and `ServerConfig`. `String` stays for clap
    arguments, function parameters and return values, map keys,
    `HlPath`'s components, and text that is built up in place.

84. The tracker beacon's user count counts every connection that has
    finished the handshake, logged in or not, like openline's
    `mStats.connCount` (`HotlineServ.cp`). Before, it counted only
    logged-in users.

85. The file checks use `FileNameWithInfo::is_folder` instead of
    comparing `*b"fldr"`. `authorize_upload` and `start_folder_upload`
    share one upload destination check. A folder upload item whose
    folder cannot be made logs the error at `debug` and moves on, like
    openline's `ProcessFldrUploads`.

86. `Providers` implements `Default` with the in-memory backends that
    `SharedState::default` used. A test names only the providers it
    changes and takes the rest from `..Providers::default()`.

87. `myTran_GetClientInfoText` (303) lists each transfer in its section
    with openline's name, percent, and size columns. A running transfer
    reports the bytes its HTXF stream has moved. A queued download is
    listed under Waiting Downloads. A size under 100 bytes reads 0%,
    where openline divides by zero.

88. `hlc` sends every line on its input line as chat, a line that
    starts with `/` too, so a server's console commands reach it. Tab
    moves the focus to the user list. A click selects a user. A double
    click, or Enter on the selected user, opens a box that sends
    `myTran_SendInstantMsg` (108). A right click opens a user menu. A bar
    at the bottom lists the keys that work now. Shift+Enter starts a
    new line where the terminal reports it, through the kitty keyboard
    protocol or the Windows console. Ctrl+J works in every terminal,
    and Alt+Enter in most. A text box grows to five lines and then
    scrolls. `hlc` sends each line break as `\r`, and shows each `\r` in
    chat or a private message as its own line.
    `hotline-client/tests/server.rs` runs client requests against a
    real in-process server.

89. `hlc` shows a user's `myTran_GetClientInfoText` (303) text in a box,
    from the user menu or the `i` key, one line for each `\r`.

90. `hlc`'s News tab, opened with Ctrl-N or a click, reads classic news
    with `myTran_GetMsgs` (101) and posts with `myTran_OldPostNews`
    (103). A post may hold more than one line. Each `myTran_NewMsg`
    (102) push adds the post at the top.

91. `hls` stops on Ctrl-C or SIGTERM and first writes every queued
    activity log line.

92. `hlc`'s Files tab, opened with Ctrl-F or a click, lists a folder
    with `myTran_GetFileNameList` (200). Enter or a double click opens
    a folder, and Backspace goes up one folder. A folder shows a
    trailing `/`, and a file shows its size.

93. `hlc` downloads a file on Enter or a double click in the Files tab,
    with `myTran_DownloadFile` (202) and an `HTXF` connection on the
    next port. The file goes in `--download-dir`, the current folder by
    default, and never replaces a complete local file. Each path
    separator in the server's name becomes `_`. The transfer runs on its
    own task and writes a line in chat when it ends. A
    `myTran_DownloadInfo` (211) push shows a queued download's place in
    the queue.

94. `hlc` uploads a local file to the current folder: `u` in the Files
    tab asks for its path. `myTran_UploadFile` (203) sends the flat
    object's size in `myField_TransferSize` (108). The `HTXF` connection
    carries the file's `DATA` fork, and the upload ends when the server
    closes it.

95. `d` in `hlc`'s Files tab downloads the selected file, or a folder
    with everything inside it through `myTran_DownloadFldr` (210). The
    client sends Next File (3) for each item and Send File (1) for each
    file. The folder goes in `--download-dir` as a new folder. Item 101
    covers an existing local folder.

96. `hlc` uploads a local folder, with everything inside it, through
    `myTran_UploadFldr` (213) when the path after `u` names a folder.
    Each folder comes before the items inside it, and symbolic links
    are skipped. The client sends an item header after each Next File
    (3), and a file's data after Send File (1) or Resume File (2).

97. A partial upload lists under its own name, `name.hpf`. Delete,
    rename, and move resolve that exact name, so a client can act on the
    partial file and the complete file apart. openline lists a partial
    file under its disk name and resolves only the exact name too.
    openline's server uses `FindResumableFile` only to resume a folder
    upload.

98. An article summary in `myField_NewsArtListData` (321) lists every
    stored flavor with its MIME type and size. This matches openline's
    `CNZArticleList::AddArticleFlavor`. A post stores the one flavor that
    the client sends.

99. Text on disk is UTF-8, and text on the wire uses the connection's
    encoding, Mac OS Roman by default. A threaded news article body with a
    `text/` flavor is decoded on post and encoded again for
    `myTran_GetNewsArtData` (400). Its listed size is the size in that
    encoding. `myTran_GetMsgs` (101) cuts a long board at a whole character
    with `text_encoding::encode_truncated`. No transaction sets a
    connection's encoding yet, so every connection uses Mac OS Roman.

100. `hlc` shows the `myField_ServerBannerUrl` (153) of a
     `myTran_ServerBanner` (122) push as one line, for a link banner and
     for the link of an image banner. It does not show a banner image,
     because a terminal cannot show it.

101. `hlc` resumes a transfer only after the user agrees, like openline.
     A download goes into `name.hpf` and becomes `name` when it ends. A
     local `name.hpf` asks to resume, replace, or cancel; resume sends
     `myField_FileResumeData` (203). A complete local file asks for a
     new name. An upload whose `name.hpf` the server lists asks to
     resume or cancel; resume sends `myField_FileXferOptions` (204) = 1.
     A folder download into an existing folder asks to resume, replace,
     or cancel. Resume skips each complete file, continues each `.hpf`
     file with Resume File (2), and gets the rest with Send File (1). A
     folder upload onto a listed folder asks to resume or cancel.

102. `hlc` uses private chat rooms. "Private chat" in the user menu
     opens a room with `myTran_InviteNewChat` (112). An invitation,
     `myTran_InviteToChat` (113), opens a box: Enter joins with
     `myTran_JoinChat` (115), and Esc declines with
     `myTran_RejectChatInvite` (114). Each room has its own tab, named
     after its subject or `Chat <id>`, with its own lines, input, and
     member list. On a room tab, Ctrl-A invites a user, Ctrl-T sets the
     subject with `myTran_SetChatSubject` (120), Ctrl-W leaves with
     `myTran_LeaveChat` (116), and Ctrl-R moves to the next room. The
     pushes 117, 118, 119, and a `myTran_ChatMsg` (106) with a
     `myField_ChatID` (114) update the room.

103. `hlc` reads and writes threaded news. Ctrl-B opens the "Threaded
     News" tab. It lists bundles and categories with
     `myTran_GetNewsCatNameList` (370) and the articles of a category,
     indented by thread, with `myTran_GetNewsArtNameList` (371). Enter
     opens an item or reads an article with `myTran_GetNewsArtData`
     (400). `p` posts and `r` replies with `myTran_PostNewsArt` (410).
     `x` deletes an article and its replies with `myTran_DelNewsArt`
     (411), or a bundle or category with `myTran_DelNewsItem` (380).
     `b` and `c` create a bundle with `myTran_NewNewsFldr` (381) or a
     category with `myTran_NewNewsCat` (382). `hotline-client` has a
     method for each of these opcodes. The new `wire::SliceReader`
     cursor decodes the category and article listings.

104. `hlc` administers accounts. Ctrl-U opens the "Admin" tab, which
     shows only when the user has the open-user or create-user
     privilege, like openline's client. The admin opcodes changed over
     time. Old servers have only `myTran_NewUser` (350),
     `myTran_DeleteUser` (351), `myTran_GetUser` (352), and
     `myTran_SetUser` (353). Servers from 1.8.4 add `myTran_GetUserList`
     (348) and `myTran_SetUserList` (349), and mhxd does not have
     them. mhxd sends no reply to an opcode it does not know, so `hlc`
     does not probe. Like openline's client (`DoOpenUserList`), it uses
     348 and 349 when the login reply's `myField_Vers` (160) is 184 or
     more. An mhxd server set to report 184 or more gets no reply to
     348, and the Admin tab waits. Otherwise
     it uses 350 to 353: `o` opens an account by login, and an account
     cannot be renamed. The account form edits the name, login, and
     password, and Space turns each privilege on or off. A password
     that is not typed stays unchanged.

105. `hlc` sends and shows broadcasts. Ctrl-G opens a message to every
     user with `myTran_UserBroadcast` (355). The key works only when the
     user has the broadcast privilege. A broadcast push shows as an
     "Administrator Message" line, like openline's client
     (`ProcessTran_UserBroadcast`).

106. `hlc` changes the user's name, icon, and options after login with
     `myTran_SetClientUserInfo` (304). Ctrl-O opens the form. Like
     openline's client (`GetClientUserInfo`), each request sends the
     name, the icon, and every `myField_Options` (113) bit: refuse
     private messages, refuse private chat, and automatic response. It
     adds `myField_AutomaticResponse` (215) only when that bit is on.
     `UserChangeOptions` moved from the server to `hotline-proto`, so
     the server and `hotline-client` share it.

107. `hlc` disconnects users with `myTran_DisconnectUser` (110). The
     user menu shows "Disconnect", and `k` on the user list does the
     same, only when the user has the disconnect privilege, like
     openline's client (`DoDisconnectUser`). A modal then asks: `d`
     disconnects, `t` also bans for a while, and `p` bans for good.
     Like openline's client, `myField_Options` (113) is 1 or 2 only for
     a ban, and a permanent ban also sends the user's name in
     `myField_Data` (101). openline asks only before a ban. `hlc`
     always asks, because the modal is where the user picks the ban.

108. `hlc` runs the file operations `myTran_DeleteFile` (204) to
     `myTran_MakeFileAlias` (209) on the Files tab: `x` deletes after a
     `y` check, `n` creates a folder, `i` shows the info, `r` renames,
     `c` sets the comment, `m` moves, and `a` makes an alias. A move or
     an alias asks for a `/`-separated folder from the root. Like
     openline's client (`CMySetFileInfoTask`), `myTran_SetFileInfo`
     (207) leaves out an empty new name or comment, so `hlc` cannot
     clear a comment. openline sends `myField_FilePath` (202) and
     `myField_FileNewPath` (212) empty at the root, except in
     `myTran_SetFileInfo` (207). `hlc` leaves them out, as it already
     does for listings and transfers.

109. `hlc` cancels downloads. Ctrl-K lists the downloads that run, and
     Enter stops the chosen one. The `.hpf` file stays for a resume.
     Like openline's client (`~CMyDownloadTask`), `hlc` sends
     `myTran_KillDownload` (214) only for a download that still waits
     in the server's queue, and only when the server version is 182 or
     more. openline's client holds a download in that queue stage only
     when the server version is 185 or more. It opens the transfer
     connection after the wait. `hlc` opens it at once, so a download
     counts as queued until its first data arrives. A running download
     stops when its transfer connection closes. `hlc` does not send
     `myTran_DownloadBanner` (212). A terminal cannot show a banner
     image, so `hlc` shows only the banner's link from
     `myTran_ServerBanner` (122).

See AGENTS.md for durable working conventions (verification loop, commit
hygiene, rustdoc style, reference-naming rules); this file only tracks
current status and open work.
