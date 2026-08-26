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

## Primary vs. secondary reference divergences (resolved)

Where the two reference implementations disagree, this server matched the
*secondary* reference in every case found so far: chat room password/invite
gating, chat subject checks, chat invite reply behavior, dropbox path
substring matching, login case folding, agreement push branching, the
`USER_GETLIST` access check, `AccessExtra::CHAT_PRIVATE` gating chat-room
membership opcodes, and `AccessExtra::MSG` as an alternative to
`Access::SEND_MESSAGE`. One exception: `on_chat_create`'s invite-target
handling matched neither reference and was fixed toward the *primary*
reference's shape (loop over every `Uid`, one shared room, skip
undeliverable targets), since that fits this server's existing
one-shared-room model.

## Backlog, in priority order

1. **No pluggable admin/console-command layer.** Several `AccessExtra`
   fields exist purely to gate the secondary reference's in-chat admin
   commands (`/access`, `/0wn`, `/color`, `/users`, `/mon`, a debug
   command) — `USER_COLOR`, `USER_ACCESS`, `USER_0WN`, `IS_0WN3D`,
   `MANAGE_USERS`, `DEBUG`, and `ACCESS_VOLATILE` (whether a command-
   driven access change persists to the account) — and none of them do
   anything today because `on_chat` has no command parser at all, just
   a plain broadcast. If this server ever adds one, the command set
   itself is a natural fit for a provider (a `ChatCommandProvider` or
   similar, mapping a leading-slash token to a handler), the same shape
   as `AccountProvider`/`FileProvider`: `hotline-server` would define
   the trait and dispatch, the embedding application would supply which
   commands actually exist.

2. **Folder transfer (`myTran_DownloadFldr`/`UploadFldr`) isn't
   implemented at all.** Needs new opcodes, a `TransferKind::Folder` (or
   equivalent), and the `dlFldrAction_SendFile`/`_ResumeFile`/`_NextFile`
   (1/2/3, `HotlineClientServerCommon.h`) action field the client sends on
   the HTXF connection to step through a folder's files one at a time.
   Independently visible feature, large enough for its own commit(s) once
   picked up, not a small addition to fold into other work. Also covers
   `Access::DOWNLOAD_FOLDER`/`UPLOAD_FOLDER`, which have known reference
   denial strings in `Access::actions` but no `require()` call site yet.

3. **HTXF transfer queue not implemented**
   (`HTLS_DATA_QUEUE_POSITION`/`HTLS_HDR_QUEUE_UPDATE` equivalents:
   `WaitingCount` (116), the `DownloadInfo` push). Needs a concurrent-
   download cap in `ServerConfig` and a waiting list in `SharedState`. A
   live per-transfer-progress tracker from this piece is also what
   `on_user_get_info`'s file-transfer sections are waiting on; until it
   exists those five sections always read `"None."`, with no in-progress
   transfers ever reported.

4. **A still-uploading (`.hpf`) file can't be touched by its plain name
   until it finishes.** `delete`/`rename`/`r#move`/`symlink` all resolve
   only the exact name passed in, so a resumable in-progress upload is
   invisible to those operations by design, not by omission — flagged in
   case that turns out to matter for a real client.

5. **A polite kick (`Politequit` push, `on_user_kick`'s ban path) closes
   the connection immediately after sending the notice, with no chance
   for the client to drain it first.** `Session::run`'s select loop
   (`session.rs`) does `writer.send(tx).await?` then `break`s straight
   into dropping the socket — the same shape that caused the `FILE_GET`/
   `BANNER_GET` "connection closed before transfer complete" bug (fixed by
   waiting for the client's own close instead of closing first). Unlike
   that fix, an unconditional wait for the peer to close isn't safe to
   reuse verbatim here: a kicked client may be exactly the unresponsive
   case an admin is disconnecting, so waiting forever would leak a hung
   connection per stuck kick. Needs a bounded wait (drain with a short
   timeout, then force-close regardless). The silent-kick path
   (`RecipientMessage::Disconnect`) is unaffected: it sends nothing before
   closing, matching the reference's own immediate `Disconnect()`.

6. **Startup file errors don't say which file.** `hotline-daemon`'s
   provider `open()` calls (`FileAccountProvider`, `FileFileProvider`,
   `FileNewsProvider`, `FileBanProvider`, `FileBannerProvider`) all
   propagate a raw `io::Error` straight through `main`'s `?` to `anyhow`,
   so a missing/unreadable `--banner`, `--accounts-dir`, `--files-dir`,
   etc. prints just `Error: No such file or directory (os error 2)` with
   no path and no hint which flag caused it. Each `open()` should wrap its
   `io::Error` with the path it was trying (`anyhow::Context::with_context`,
   or a matching `io::Error::new` with the path folded into the message).

7. **Enum variant naming isn't consistently the openline constant's tail
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

7. **`HTRK` tracker support stops at wire-format types.**
   `hotline-tracker` has `encode_client_request_v1`/`decode_reply_header`/
   `TrackedServer::decode`/`ServerBeacon::encode`, but nothing in the
   workspace calls any of them: `hls` never sends a `ServerBeacon` to
   register with a tracker, and `hlc` has no tracker-query mode to list
   servers before connecting. Needs a periodic beacon task wired into
   `hotline-daemon` (with a `--tracker` config flag) and a query command in
   `hotline-cli` that opens the tracker's TCP port, sends the client
   request, and decodes the `TrackedServer` list back.

8. **Split transactions (`len` != `len2`, `HLProtocol.md`'s "Total size"
   vs. "Data size") aren't reassembled.** `transaction.rs` already
   documents this as an assumption (`len == len2` on the wire).
   Confirmed against the reference: its transport (`UTransact.cp`)
   really does reassemble multi-part transactions on receive, keyed by
   `(isReply, id)`, appending each part's raw bytes until `totalSize` is
   reached before parsing `count`/fields from the assembled buffer. But
   no real call site (client or server) ever calls
   `SetSessionSendSize`/sends more than one part, so `len == len2` on
   every transaction the reference actually emits. Low priority: this
   would only harden decoding against a hypothetical non-reference
   sender. If picked up, needs: a reassembly map in
   `hotline_codec::TransactionCodec`'s decoder keyed by `(is_reply, id)`
   accumulating raw bytes per id until `len` bytes are in hand, only
   then running today's `count`/field decode against the assembled
   buffer; a cap on claimed `len` and on the number of concurrently
   pending partial ids, since an unbounded version lets a peer hold
   memory open indefinitely by trickling one byte per frame across many
   ids. No corresponding encode-side change needed, since nothing
   expects to receive a deliberately split transaction from us.

9. **`UserNameWithInfo::flags` is a raw `u16` even though `UserFlags`
   (`types/user_flags.rs`) exists for exactly this field** — its own doc
   comment says "Bit meanings for `UserNameWithInfo::flags`", but every
   caller does `.bits()`/raw `u16` math instead of using the typed flags.
   Changing the field to `UserFlags` restores type safety but touches
   ~23 call sites across `hotline-server`/`hotline-client`; worth a
   standalone commit, not a quick edit. Found during the naming/structure
   review after the one-file-per-struct reorg (commit 3c7df8b).

10. **`session/connection.rs` (~6800 lines) holds one `Session` struct
    with a single giant `impl` spanning login, chat, private messages,
    files, news, and account-admin handlers.** AGENTS.md documents that
    `session.rs`'s handlers belong together, but a type's `impl` blocks
    can span multiple files in the same module: splitting into
    `session/chat.rs`, `session/files.rs`, `session/news.rs`,
    `session/accounts.rs`, `session/admin.rs` (each a separate
    `impl Session { ... }`) would leave `connection.rs` with just the
    struct definition, `run`, handshake, and the two dispatch tables.
    Large, mechanical-but-risky diff; own reviewed PR.

11. **`PendingTransfer` (`state/transfer.rs`) mixes two payload
    shapes**: its own doc comment admits `dir`/`name`/`login`/`raw` are
    "unused for `TransferKind::Banner`". Cleaner shape: fold the
    file-specific fields into `TransferKind::File { dir, name, raw }`,
    leaving `TransferKind::Banner` bare, with `direction`/`size`/
    `resume_offset`/`client_ip`/`login` shared at the top level. Touches
    `state/transfer.rs`, `session/connection.rs`'s transfer-registration
    call sites, and `server/htxf.rs`'s dispatch — a real data-shape
    change, not a rename.

12. **`ChatCommandProvider` is misnamed relative to its siblings.**
    Unlike `AccountProvider`/`FileProvider`/`NewsProvider`/`BanProvider`/
    `BannerProvider` (real storage backends), it dispatches `/`-prefixed
    chat commands and provides no persistence — the same reasoning that
    already pulled `FloodPolicy` out of the `*Provider` family (commit
    48ae74d). Candidate rename: `ChatCommandDispatcher`/
    `ConsoleCommandHandler`, likely paired with renaming the `commands/`
    module itself (ambiguous against "commands" meaning wire
    transactions) to something like `console/`. Touches ~8 files; a
    naming/design call, not mechanical.

13. **`AccessExtra::MSG` is a cryptic bitflag name** among otherwise
    self-explanatory siblings (`CHAT_PRIVATE`, `USER_GETLIST`, ...). A
    clearer name (`MSG_OVERRIDE`?) is tempting, but `bitflags_serde`
    serializes flag names by their Rust identifier directly into
    on-disk account JSON (`"ALL"` or `["MSG", ...]`), so renaming the
    constant breaks any already-persisted account store. Needs a
    deliberate migration decision (e.g. a `#[serde(alias = "MSG")]`-style
    read-side compat shim), not a code-only rename.

15. **Classic news posts (`myTran_OldPostNews`, `SharedState::post_news`,
    `state/shared.rs`) are missing the timestamp the reference includes.**
    The reference's `AddToMsgBoard` (`HotlineServ.cp`) header format is
    `"From %#s (%s%2.2hu %2.2hu:%2.2hu):\r\r"` — poster name followed by
    `(Mon day hour:minute):`, e.g. `From Alice (Jan 5 14:32):`. Our
    `post_news` writes `From {poster}\r\r` with no date/time at all.
    Separately, the reference's divider is a *footer* appended after each
    message (`"\x3c\r______...\r"`, prefixed by a `<` byte), not a header
    prepended before the next one like our `DIVIDER` constant. Needs a
    fix that reproduces the reference's exact header/footer shape
    (including its `<` marker byte, whose purpose isn't yet understood —
    ask before guessing) rather than a quick timestamp bolt-on.

16. **`news.txt` (`hotline-daemon`'s `FileNewsProvider::flat_path`)
    stores classic news entries joined with bare `\r` (Mac Classic line
    endings), inherited from the wire's own `\r`-separated text.** This
    matches what the client displays, but on disk it reads as one giant
    line in most modern editors/tools that only recognize `\n`/`\r\n` —
    a `.txt` file people may reasonably expect to be openable normally.
    Worth deciding deliberately (translate to native line endings on
    disk and back on load, or at least document it prominently) rather
    than leaving it as an accidental Mac-only artifact; the reference has
    no equivalent question since its `MessageBoard.txt` only ever ran on
    classic Mac OS itself.

17. **`hotline-cli`'s `App` mixes UI-agnostic session state with raw
    terminal-input state in one struct** (`app.rs`): `nick`/`icon`/
    `mode`/`users`/`lines`/`access`/`encoding` are session state, while
    `input`/`should_quit` are terminal-interaction state, despite
    `App`'s own doc comment describing it as pure update logic over
    data. Splitting into a `Session` (domain) + input-handling pair (or
    moving `input`/`should_quit` into `main.rs`'s own loop state) would
    make that true. Touches `main.rs`'s event loop wiring; own reviewed
    change.

18. **Threaded news storage does not match the reference's on-disk
    binary database format.** The reference stores each news category
    in its own file. Each file starts with an `HLNZ` header (version 2,
    `_NZ_SDBHeader`). The file also has a free-block bitmap table and an
    ID-to-offset hash table (`_NZ_SHashInfo`/`_NZ_SIDOffset`). Each
    article is a chain of fixed-size blocks: one `_NZ_SNewsItemBlock`
    index block, plus one `_NZ_SGenericBlock` block for each stored MIME
    type. Files sit at `<bundle>/<category>` on disk. A real sample file
    confirms this: it starts with `HLNZ`, version 2, block size 256.
    This server stores the whole news tree in one `news.json` file
    instead (`hotline-daemon/src/news.rs`,
    `FileNewsProvider`/`NewsProvider`). A matching format needs three
    new parts: a binary codec (in `hotline-proto`'s style, pure
    encode/decode, no I/O, testable without a filesystem), a block
    allocator that can split and merge blocks when an article grows past
    one block, and a new `NewsProvider` backend. This work mirrors
    `HotlineServNewsDatabase.cp`. It is a large, standalone effort, not a
    quick change. No known client reads the file format directly; only
    the wire protocol matters for interop. This task stays low priority
    for now.

19. **Two open design questions from news article/category JSON
    storage.** `NewsArticle::primary_flavor` picks
    `self.data.iter().next()` on a `BTreeMap`, so once an article
    legitimately holds more than one flavor, "primary" means
    alphabetically-first MIME type, not first-posted or preferred —
    harmless today (always exactly one entry) but worth deciding before
    multi-flavor posting is wired up. Separately, nesting a category's
    articles under their JSON parent made both `ArticleNodeRef::serialize`
    and `category.rs`'s `flatten` recurse per thread-depth, where the old
    flat-map-plus-id-links shape needed no recursion to save/load a
    category; a pathologically deep, self-nested reply chain could now
    stack-overflow the server on save or load. Worth a depth cap or an
    iterative rewrite if deep threads ever become realistic.

## Resolved

1. `UserSelfinfo`'s `Access` field now sends the session's real privileges
   (`self.access.bits()`), matching the reference.
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
   following the non-HFS reference codepath. Resource-fork (`"MACR"`)
   upload data needed no code change — already discarded by tag like the
   reference's own data-fork-only storage.
7. Private-message/chat refusal and automatic response implemented:
   `on_user_change` persists refuse-message/refuse-chat/auto-response
   preferences; `Session::refusal_bounce` bounces or auto-replies on
   `on_private_msg`/`on_chat_invite`.
8. `FieldId::Options` (113) added (needed by item 7's bounce push);
   module docs updated to note it now carries different meanings per
   direction.
9. `USER_FLAGS` (112) now also sets `ALL_PRIVS`/`NO_PRIVS`, matching the
   reference's own computed bits.
10. `HlPath::decode` now rejects a declared component count over 256
    before allocating (decoder-side safety bound, no reference to match).
11. `InMemoryFileProvider::delete`/`rename`/`r#move` no longer
    auto-vivify missing intermediate directories (`Dir::get_dir_mut_existing`);
    they return `NotFound` like the reference instead.
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
    `HLProtocol.md`'s own `myTran_*`/`myField_*` names instead of the
    reference header's `HTLC_HDR_*`/`HTLS_DATA_*` names; the two opcodes
    absent from `HLProtocol.md` (`GetUserList`/`SetUserList`, `Ping`) keep
    citing a reference header name, now called out explicitly in
    AGENTS.md's naming rule.
16. `ClientFieldId`/`ServerFieldId` and `ClientOpcode`/`ServerOpcode`
    collapsed into single `FieldId`/`Opcode` types, matching
    `HLProtocol.md`'s flat numbering; five real client/server name
    collisions surfaced and were split into distinctly-named variants
    (`ChatSend`/`ChatMsg`, etc).
17. Investigated `Cow<'_, str>` on `UserEntry`'s string fields: no real
    call site avoided a clone (`Session` and `UserEntry` are independently
    owned with no common borrow scope), reverted.
18. Chat room password/invite gating kept matching the secondary
    reference; `on_chat_create`'s invite-target handling fixed to match
    the primary reference's one-shared-room, skip-undeliverable-targets
    shape (see "Primary vs. secondary" section above).
19. `on_chat_subject` kept matching the secondary reference (access
    check, membership requirement, room password).
20. `on_chat_invite` kept matching the secondary reference (always
    replies, requires prior membership, rejects a repeat invite).
21. `on_chat_part` now sends no reply on success, matching the
    reference's own never-reply behavior.
22. `on_chat_decline` now sends no reply on success and broadcasts a
    decline notice to the room, matching the primary reference.
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
    matching the secondary reference.
28. `on_file_get`'s reply now also sends raw `FileSize` alongside
    `TransferId`/`TransferSize`.
29. `on_newsfile_post` now rejects posts over 8192 bytes with the
    reference's own error text before appending.
30. Logins are now case-folded to lowercase consistently at every entry
    point (create/read/modify/delete/lookup), avoiding the reference's own
    filesystem-case-insensitivity-dependent inconsistency.
31. `on_login`'s immediate-join gate now also requires a non-zero icon,
    matching the reference's `psUserName[0] && nIconID` check.
32. `on_login`'s agreement-push logic kept its two-way branch, matching
    the secondary reference over the primary's three-way branch.
33. `on_login` now sends `Bannerid` (161) via the pre-existing
    `FieldId::Bannerid` variant.
34. `on_login` now always sends `ServerName`, empty when unconfigured.
35. `on_user_get_list`'s `AccessExtra::USER_GETLIST` gate kept as is.
36. `on_user_kick`'s plain kick now sends `RecipientMessage::Disconnect`
    instead of a `Politequit` push; a missing `Uid` defaults to 0 and
    still replies success; temp/perm ban options implemented (see
    resolved item "ban/mute list" below).
37. `on_user_get_info`'s reply reshaped to the primary reference's own
    labels/section order and field set, plus an `Online` duration and
    `Icon`/`Version` lines not in either reference. File-transfer sections
    still always read `"None."` — needs the transfer-progress tracker
    from the open "HTXF transfer queue" backlog item.
38. Banner support implemented: `Opcode::BannerGet`/`Banner` added,
    login pushes a `ServerBanner` for `Vers` >= 151 clients, `BANNER_GET`
    streams the image over the HTXF connection unframed
    (`TransferKind::Banner`). `AccessExtra::BANNER_GET` is this server's
    own opt-in gate, on by default.
39. `RequireFlags::require` now renders each missing flag through a
    per-flag `actions()` denial-text table (`Access`/`AccessExtra`),
    matching the reference's own fixed strings verbatim where one exists.
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
    `AccessExtra::CHAT_PRIVATE` up front, matching the secondary
    reference's dispatch-level gate.
47. `on_private_msg` now allows `AccessExtra::MSG` as an alternative to
    `Access::SEND_MESSAGE`, matching the secondary reference's client
    compatibility allowance.
48. `NewsProvider`'s tree/flat caches switched from `RwLock` to
    `arc_swap::ArcSwap` (lock-free reads, read-heavy access pattern).

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
