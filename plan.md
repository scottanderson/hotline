# Near-term goals

Closing feature gaps versus openline, used for behavior parity. Backlog
below, in priority order.

## Known deliberate divergences from openline

Where openline sends no error and goes on, `ServerPolicy::error` can
send one. By default it sends none, as openline does. `hls` sends the
errors in `HlsPolicy`. A divergence below that names a `Silent` case
works this way.

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
- **Threaded news lives in one `news.json` file.** openline keeps each
  category in its own `HLNZ` database file, with a block allocator and
  an ID hash table (`HotlineServNewsDatabase.cp`). No client reads
  these files. `hls --import-news` reads them once into `news.json`.
- **Console commands differ from mhxd in small ways.** `/access <uid>`
  with no fields shows the target's privileges; mhxd has no such view.
  `/access` applies its fields all or nothing, but mhxd keeps the fields
  before an unknown key and notifies no one. `/0wn` ignores `color=`,
  `g0away=`, and `visible=`, since this server has no user color or
  invisibility. A denied command names the missing privilege through
  `RequireFlags::require`, not mhxd's fixed text.
- **A kicked client gets a drain window.** For a ban,
  `ProcessTran_DisconnectUser` puts `myTran_ServerMsg` (104) in the
  client's send queue, then calls `Disconnect()`, which drops the data
  in that queue. A plain kick sends no notice. In this server, a
  kicked session leaves the user list and stops its transfers at once.
  Then it writes the messages before the notice, and the notice, for
  at most `KICK_WRITE_TIMEOUT` (3 seconds). Then it closes its write
  side, and reads and discards for `KICK_DRAIN_TIMEOUT` (3 seconds).
  Without the drain, unread client data makes the close a reset, and
  the client can lose the notice. A full queue drops the notice. A
  disconnect with no notice drops the queue at once, as openline does.
- **A polite quit still accepts file transfers.** openline's polite
  quit refuses each new transfer connection while it waits for the
  users to leave. A polite quit of `hls` stops only the main listener.
- **Folder transfer drops one of openline's safeguards.** No skip for
  an item whose path is too long for openline's fixed 1024-byte buffer.
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
  extend it.
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
- **`hls` refuses a rename onto a taken login.** openline's
  `ProcessTran_SetUserList` fails the folder rename, then still writes
  the record over the other account with `SetUser`. The `ServerPolicy`
  default `TakenLogin::Overwrite` does the same. `HlsPolicy` picks
  `TakenLogin::Refuse`, which leaves both accounts as they were, so
  a typo in a login change cannot destroy another account. It also
  sends a `myTran_ServerMsg` (104) notice that names the taken login.
  Both reply "Error saving list of accounts."
- **Activity logs use a different date and line format.** openline
  writes the Mac short date and `\r` line endings in the classic Mac
  text encoding. `hls` writes `YYYY-MM-DD HH:MM:SS` in local time with
  `\n` line endings in UTF-8. It also replaces a tab or control
  character in any column with a space, so a name or a file name cannot
  add a column or a line. openline writes each line before it replies;
  `hls` queues the line for a writer task, and writes the queue before
  it exits on Ctrl-C or SIGTERM.
- **`hls` sends dates in UTC.** openline's
  `UDateTime::GetDateTimeStamp` sends the server's local time, and the
  `ServerPolicy` default `TimeZone::Local` does the same. `HlsPolicy`
  picks `TimeZone::Utc`, so a client sees the same times from every
  `hls`, in any zone.
- **Posted news article flavor default.** Openline stores an empty
  string when a post omits `myField_NewsArtDataFlav`; this server
  defaults to `"text/plain"`. A real client always sends this field, so
  this only affects a hand-crafted request; not changed.
- **Stored text is UTF-8.** openline stores text in the wire encoding.
  `hls` stores threaded news text bodies, like its other text, as
  UTF-8, and converts each one to the connection's encoding on the
  wire. A `myTran_GetMsgs` (101) reply that is too long is cut at a
  whole character; openline cuts at a byte.
- **`hls` replies with an error to a failed `myTran_InviteToChat` (113)
  or `myTran_SetChatSubject` (120).** openline's
  `ProcessTran_InviteToChat` and `ProcessTran_SetChatSubject` send no
  reply, and its client does not wait for one. `hls` sends no reply on
  success. On a failure, it sends the error (`Silent::ChatRequest`).
  `hlc` sends both without waiting and shows an error reply.
- **The file name rule maps more.** openline's
  `UFileSys::ValidateFileName` differs by platform, and so does
  `validate_file_name`. On Mac OS X, openline only cuts the length.
  Elsewhere than Windows, `hls` also maps `/` and `\` to `-`, and the
  names `.` and `..` to dashes, so a name cannot leave its folder. The
  AppleDouble fork store also maps the dot of a leading `._`, which
  starts a sidecar's name. On Windows, openline maps
  `\ / : * ? " < > |`, control bytes, the bytes 0x81, 0x8D to 0x90,
  0x9D, and 0x9E, a leading `.`, and each trailing `.` and space to
  `-`. `hls` maps the C1 control characters U+0080 to U+009F in place
  of those bytes, and gives a reserved device name such as `CON` a
  `-`. `hls` cuts a name at 255 UTF-8 bytes, at a character boundary.
  It also maps the name of a new alias, which openline's
  `ProcessTran_MakeFileAlias` does not.
- **Move and alias check drop boxes.** openline's `ProcessTran_MoveFile`
  and `ProcessTran_MakeFileAlias` check no drop box. A user who guessed
  a name could move a file out of a drop box, or alias it, and then
  download it. `hls` refuses both without `VIEW_DROP_BOXES` when the
  source is in a drop box (`Silent::DropBoxSource`).
  Delete matches openline: it shows nothing.
- **`myTran_GetClientInfoText` (303) shows more.** openline's
  `ProcessTran_GetClientInfoText` sends `Nickname:`, `Name:`,
  `Account:`, and `Address:` with the IP address only. `hls` also sends
  `Icon:`, `Version:` when the client sent `myField_Vers` (160), and
  `Online:` with the minutes and seconds since login. It adds the port
  to `Address:`. It shows an empty account name as `(none)` and an
  empty login as `(guest)`, where openline shows an empty value.
- **An empty news article adds nothing.** openline's
  `ProcessTran_PostNewsArt` adds the article before it checks the
  `myField_NewsArtData` (333) body, so it keeps an article with no
  text and then replies with an error. `hls` checks the body first and
  adds nothing.
- **Account fields have no length limit.** openline's
  `SMyUserDataFile` keeps 64 bytes of a name and 32 bytes of a login
  and a password, and `ProcessTran_NewUser`, `ProcessTran_SetUser`, and
  `ProcessTran_SetUserList` cut each field to fit.
  `ProcessTran_SetUserList` also keeps 31 bytes of a record's old
  login. `hls` keeps any length.
- **`myTran_SetUser` (353) keeps a field that the request leaves
  out.** openline's `ProcessTran_SetUser` clears the record, so a
  missing `myField_UserName` (102) or `myField_UserAccess` (110) gives
  an empty name or no access. `hls` keeps the old value. openline's
  client always sends both fields.
- **A failed comment write stops a rename.** openline's
  `ProcessTran_SetFileInfo` ignores a failed comment write and still
  renames. `hls` replies with the error and does not rename
  (`Silent::CommentWrite`).
- **A message with no `myField_Data` (101) is an error.** openline's
  `ProcessTran_SendInstantMsg` and `ProcessTran_UserBroadcast` handle
  it as an empty message and reply with success. `hls` replies with an
  error (`Silent::MessageWithoutData`).
- **A zero uploaded type reads as the type for the name.** openline's
  Mac `_FSSetFlatFileInfo` stores the `INFO` fork's type and creator as
  they are, also a zero type. `hls` stores them too, but a stored zero
  type reads back as the type and creator for the name's extension,
  which openline's Windows build gives every file. `hlc` uploads a zero
  type.
- **An uploaded comment stops at the end of the `INFO` fork.** openline's
  Mac `_FSSetFlatFileInfo` copies as many bytes as the comment size
  says, from a zeroed 592-byte buffer, so a short fork gives zero bytes
  or bytes past the buffer. `hls` copies only the bytes it received.
- **Finder info, comments, and resource forks are in AppleDouble
  files.** openline's Mac build keeps them in the HFS catalog, the
  Desktop database, and the resource fork, and its Windows build keeps
  none of them. `DiskFileProvider` keeps them in the sidecar `._name`
  next to each item, with the comment in the extended attribute
  `com.apple.metadata:kMDItemFinderComment`. Each read and change does
  what xnu's `bsd/vfs/vfs_xattr.c` does (xnu-11417.101.15). macOS 15
  and later use doubleagentd for these files, and its source is not
  public, so xnu is the reference. A fork write starts at the end of
  the fork, as openline's `ResumeUnflatten` does, and a write from 0
  replaces the fork, as xnu's `vnode_flushnamedstream` does. Finder can
  show a comment from `.DS_Store` that hls does not see. Two changes
  differ from xnu, which loses data in a sidecar in another RFC 1740
  order. hls refuses a fork write or an attribute growth that would
  write over another entry. hls writes a Finder info change past byte
  120, which xnu drops and reports as done.
- **Resume data without a `DATA` entry is an error.** openline's Mac
  `ResumeFlatten` resumes the data fork from 0 when the `RFLT` data has
  no `DATA` entry, and reads entries past the end of a short field.
  `hls` replies "Resume data is invalid." to both.
- **A repeated resume entry replaces the earlier one.** openline's Mac
  `ResumeFlatten` seeks a fork to its last `RFLT` entry, but reduces the
  fork's size by each entry. `hls` uses the last entry for both.
- **`hlc` grays out an action that needs a privilege.** openline's
  client hides the disconnect, broadcast, new user, and open user
  buttons without the privilege (`CMyUserListWin::SetAccess` and
  `CMyToolbar::SetAccess`), and disables the others. `hlc` shows each
  key, command, and menu item in gray, so the layout stays the same.
- **`hlc` offers an upload in every folder.** openline's client turns
  off its upload button at the root (`CMyFileWin::SetFileAccess` in
  `HotlineWindows.cp`). `hlc` turns on `u` with the upload privilege in
  every folder. The server decides, and `hlc` shows its error.
- **`hlc` reads news only with `myAcc_NewsReadArt`.** openline's
  client opens the message board and the news window with no check
  (`DoShowMsgBoardWin` and `DoShowNewsWin` in `Hotline.cp`). openline's
  server refuses `myTran_GetMsgs` (101) without the privilege, but
  answers `myTran_GetNewsCatNameList` (370) and
  `myTran_GetNewsArtNameList` (371) with no check. Its client checks
  the privilege only to read an article. `hlc` grays out the News and
  Threaded News tabs without the privilege, and sends none of these
  requests.
- **`hlc` rings for chat and news only when the user cannot see
  them.** openline's client plays its chat sound on each chat line,
  and its news sound on each news post (`ProcessTran_ChatMsg` and
  `ProcessTran_NewMsg` in `Hotline.cp`). `hlc` rings the terminal bell
  on each of them only while another tab shows, or while the terminal
  does not have the focus. A terminal that does not report focus
  counts as focused. `hlc` has no sound for a user who joins or
  leaves.

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

1. **No HTTP tunnel listeners.** openline also listens on the base
   port plus 2 and plus 3 for HTTP-tunnelled clients (`HotlineServ.cp`,
   `UHttpTransport.cp`). Only openline's own client uses the tunnel.
   `HLProtocol.md` does not document it, mhxd does not implement it,
   and no client is at hand to test against. Revisit this gap later.
2. **Chat rooms and online users have no trait.** `SharedState` keeps
   them in memory. Put them behind traits, like the storage providers.
3. **hlc has no task list.** Ctrl-K lists only the downloads that run,
   and Enter cancels one. hlc does not keep its uploads, and it shows
   no progress. It shows no request that waits for its reply.
   openline's client keeps each transfer as a task
   (`CMyDownloadTask`, `CMyUploadTask`). Decided:
   - A task is each TRTP request that hlc sends and that waits for its
     reply transaction, of every opcode, such as a file list, a news
     read, or a user info request. Each file or folder transfer is a
     task too.
   - A tasks box under the user list shows each task with its
     progress. The box shows only while a task is active, and it hides
     when none is.
   - Tab moves the focus into the box. The arrow keys select a task,
     and Enter cancels it.
   - Ctrl-T replaces Ctrl-K and focuses the box.
   - A cancelled download that waits in the queue sends
     `myTran_KillDownload` (214), as now. A cancelled upload closes its
     transfer connection and sends nothing, as openline's
     `~CMyUploadTask` does.
4. **`hlc` has no config file.** Keep the settings in
   `$XDG_CONFIG_HOME/hlc/config.toml`, or `~/.config/hlc/config.toml`
   when the variable is not set, on macOS too. The file holds the
   defaults of the command-line options: the name, the icon, the
   server, the options bits, and the bell. A flag overrides the file,
   and the file overrides the built-in default. `--bell` and
   `--no-bell` turn the bell on and off. The file chooses the events
   that ring it. On the first run with no name in the file and no
   `--name`, `hlc` asks for a name and saves it. A change in the
   options form (Alt-O) saves too. Derive `serde` on the settings
   type. The format is open: TOML with the `toml` crate, or JSON with
   the workspace's `serde_json`, which adds no new crate.

See AGENTS.md for durable working conventions (verification loop, commit
hygiene, rustdoc style, reference-naming rules); this file only tracks
current status and open work.
