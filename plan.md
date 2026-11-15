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
- **Activity logs use a different date and line format.** openline
  writes the Mac short date and `\r` line endings in the classic Mac
  text encoding. `hls` writes `YYYY-MM-DD HH:MM:SS` in local time with
  `\n` line endings in UTF-8. It also replaces a tab or control
  character in any column with a space, so a name or a file name cannot
  add a column or a line. openline writes each line before it replies;
  `hls` queues the line for a writer task, and writes the queue before
  it exits on Ctrl-C or SIGTERM.
- **Stored text is UTF-8.** openline stores text in the wire encoding.
  `hls` stores threaded news text bodies, like its other text, as
  UTF-8, and converts each one to the connection's encoding on the
  wire. A `myTran_GetMsgs` (101) reply that is too long is cut at a
  whole character; openline cuts at a byte.
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
- **The client info text lists each transfer.** openline's
  `ProcessTran_GetClientInfoText` stops a section of the transfer list
  when less than 40 of its 512 bytes are free, and keeps at most 1024
  bytes of the whole text. `TransferReport::text`, which the default
  `ServerPolicy::client_info_text` and `HlsPolicy` use, lists each
  transfer with no limit.
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
- **A post with no article ID left is an error.** openline gives out
  article ID 4294967295 and stores 0 as the next ID. Its next
  `ProcessTran_PostNewsArt` throws in `_NZ_AddIDToHash`, so the client
  gets no reply and the server shows an alert. When the ID hash bucket
  of 0 is empty, openline adds an article with ID 0 instead, and
  replies with an error. `hls` adds nothing and replies with an error
  (`Silent::NoArticleId`).
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
- **`hlc` asks only for a name on the first run.** openline's client
  shows a wizard on the first launch that asks for a name and an icon
  (`CMyApplication::DoWizard` in `Hotline.cp`). A blank name gives
  `unnamed` in both. `hlc` asks for the name on standard input, and
  takes the icon from the settings file or `--icon`. Its default is
  414, as in openline.
- **`hlc` connects to another server without a check.** openline's
  `CMyApplication::DoConnect` (`Hotline.cp`) asks to confirm when
  transfers run (`ConfirmDisconnect`), then disconnects. `hlc` ends the
  connection and its transfers when the user accepts the Connect to
  form.
- **`hlc` keeps its bookmarks in the settings file.** The Save button
  of openline's login window (`CMyApplication::DoShowConnect` in
  `Hotline.cp`) asks for a file name, with the address as the default,
  and saves the bookmark as a file in the `Bookmarks` folder
  (`SaveServerFile`). A blank address gives the name `Untitled`. `hlc`
  keeps each bookmark in the settings file, names it after its
  address, and saves no bookmark for an address that it cannot use.
  Both keep the password as the user typed it. On Unix, `hlc` writes
  the settings file with mode 0600, so that only the owner can read
  the passwords.
- **`hlc` connects to a bookmark at the start without a check.**
  openline's client connects at the start only in a custom
  distribution (`bAutoconnectEveryLaunch` in `Hotline.cp`), and asks
  first (message 1121). On the first launch, it connects to
  `hlserver.com` without such a distribution. `hlc` connects to the
  bookmark that `auto_connect` names without a question, and with no
  such bookmark it starts with no connection.
- **`hlc` lists the bookmarks without the trackers.** openline's client
  shows the bookmarks and the servers of each tracker in one servers
  window (`CMyTracker` in `HotlineTracker.cp`). `hlc` shows only the
  bookmarks, in a box.
- **`hlc` asks before it deletes a bookmark.** openline's trash button
  deletes the selected tracker or bookmark without a question, and
  moves the bookmark file to the trash
  (`CMyTracker::RemoveSelectedItem` in `HotlineTracker.cp`). `hlc` asks
  first, as for a file, an account, or news, and removes the bookmark
  from the settings file.

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

See AGENTS.md for durable working conventions (verification loop, commit
hygiene, rustdoc style, reference-naming rules); this file only tracks
current status and open work.
