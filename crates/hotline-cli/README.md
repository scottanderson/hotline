# hotline-cli

`hlc`, a terminal [Hotline] client. It runs [`hotline-client`] in a
[ratatui] interface.

[Hotline]: https://en.wikipedia.org/wiki/Hotline_Communications
[`hotline-client`]: https://crates.io/crates/hotline-client
[ratatui]: https://ratatui.rs

## Install

You need Rust 1.96 or later.

```sh
cargo install hotline-cli
```

## Usage

```sh
HLC_PASSWORD=secret hlc --host example.com --login alice --name Alice
```

| Flag             | Default     | Effect                                         |
|------------------|-------------|------------------------------------------------|
| `--host`         |             | the server to connect to at the start          |
| `--port`         | 5500        | the server's port, with `--host`               |
| `--login`        | (guest)     | the account login                              |
| `--password`     | (none)      | the password, also read from `HLC_PASSWORD`    |
| `--name`         | `RustHL`    | the name in the user list                      |
| `--icon`         | 414         | the icon ID                                    |
| `--download-dir` | `.`         | the folder that downloads go to                |
| `--encoding`     | `macintosh` | the text encoding, the same as the server's    |
| `--tracker`      |             | list a tracker's servers and exit, no login    |
| `--no-bell`      | off         | do not ring the terminal bell                  |

Use `HLC_PASSWORD` in place of `--password` when other users share the
host, because they can read a command line.

Without `--host`, hlc starts with no connection and shows the Connect
to form.

The line at the bottom of the screen shows the keys of the focused
pane.
The tabs are Chat, News, Files, and Tasks, then Admin with the account
privileges. Each tab has a number, which the tab bar shows. F1 to F9,
or Alt with 1 to 9, opens a tab. When you have joined a private chat
room, a row under the tab bar of Chat and of each room shows Public,
the public chat, then each room. A row under the News tab shows
Message Board, the classic news, and Threaded News. Alt-, and Alt-.
move to the previous and the next entry of that row. Each tab has a
main pane.
Chat and each room also have the user list, and Tab moves between it
and the main pane. Esc goes back to Chat. On Chat, Esc asks whether to
quit. Ctrl-C asks too, from anywhere, and a second Ctrl-C quits.
When the server ends the connection, a box shows its message, or that
the connection closed. `r` in the box logs in again to the same
server, `c` asks for another server, `q` or Ctrl-C quits, and Enter or
Esc closes the box. hlc then stays open with the chat lines. It drops
the users, the news, the files, the rooms, the accounts, and the tasks
of the connection, and stops each transfer. "Disconnect" in the
palette ends the connection in the same way, and "Reconnect" logs in
again. "Connect to" in the palette asks for a server, as `host` or
`host:port`, a login, and a password. It shows the last ones. Tab
moves to the next row, and Shift-Tab to the previous row. Enter ends
the connection, if there is one, and connects to the new server.
Reconnect then uses the new server.

A tab label turns yellow when the tab changes while another tab shows.
The Chat tab and each room change with each new line, and the News
tab changes with each new post. The Chat tab also turns yellow when a
room changes. The label turns back when you open the tab.

The terminal bell rings on each chat line in a room that you cannot
see: another tab or room shows, or the terminal does not have the
focus. It also rings on a private message, an administrator message,
a broadcast, an invitation to a private chat, and the end of a file
transfer. `--no-bell` turns it off.

A key, a command, a menu item, or a tab that needs a privilege you do
not have shows in gray, and it does nothing. hlc gets the privileges
from the server. A server older than version 1.8.2 has no privileges
for private chat, messages, or folder downloads. With such a server,
hlc allows private chat and messages, and the file download privilege
also allows folder downloads. The Chat tab needs
the read chat or the send chat privilege, and Enter in a chat needs
the send chat privilege. The News and Threaded News tabs need the
read news privilege, and hlc asks the server for nothing without it.
To delete a news bundle, a user needs the delete bundle and the delete
category privileges. On the Files tab, a key needs the file privilege
or the folder privilege, for the kind of the selected item.

| Key    | Where                | Effect                                          |
|--------|----------------------|-------------------------------------------------|
| Ctrl-P | outside a dialog     | the commands for this place; type to filter     |
| Ctrl-O | a room, user list    | the actions of the room or the selected user    |
| Alt-A  | outside a dialog     | a broadcast                                     |
| Alt-O  | outside a dialog     | your name, icon, and options                    |
| Alt-T  | outside a dialog     | the Tasks tab, or back to the tab before it     |
| Alt-,  | Chat, a room, News   | the previous entry of the row under the tab bar |
| Alt-.  | Chat, a room, News   | the next entry of the row under the tab bar     |
| Alt-I  | a room               | invite a user                                   |
| Alt-S  | a room               | set the subject                                 |
| Alt-W  | a room               | leave the room                                  |
| ⇧Enter | a text box           | a new line (also Alt-Enter or Ctrl-J)           |
| ← →    | a text box           | move one character (also Ctrl-B and Ctrl-F)     |
| Alt-B  | a text box           | move to the start of a word (also Ctrl-←)       |
| Alt-F  | a text box           | move to the end of a word (also Ctrl-→)         |
| Ctrl-A | a text box           | move to the start of the line (also Home)       |
| Ctrl-E | a text box           | move to the end of the line (also End)          |
| Ctrl-W | a text box           | cut back to a space                             |
| Alt-⌫  | a text box           | cut back to the start of a word                 |
| Alt-D  | a text box           | cut to the end of a word                        |
| Ctrl-D | a text box           | delete the next character (also Delete)         |
| Ctrl-U | a text box           | cut to the start of the line                    |
| Ctrl-K | a text box           | cut to the end of the line                      |
| Ctrl-Y | a text box           | paste the last cut                              |
| Ctrl-_ | a text box           | undo                                            |
| Enter  | user list            | send a private message                          |
| `i`    | user list            | the user's info                                 |
| `k`    | user list            | disconnect the user                             |

A text box moves and deletes by grapheme cluster. An accented letter
or an emoji sequence is one character. Cuts in a row join into one
cut. Each text box keeps its own cut. Undo removes a run of typed
text in one step.

Alt-X also opens the palette, for a terminal that takes Ctrl-P.
Shift-F10 and the Menu key also open the actions. The palette and
the action menu show the key of each entry. A right click on a user
opens a menu that also starts a private chat.

Two panes side by side share one border. Drag the left border of the
right column to make it wider or narrower. On the Threaded News tab, drag
the borders between the three panes. The sizes last until `hlc` quits.

On macOS, Option with a letter types a symbol by default. Set the
Option key to send Meta for the Alt keys. In Terminal, turn on "Use
Option as Meta key". In iTerm2, set the left Option key to "Esc+".

On the Files tab, Enter opens a folder or downloads a file, and
Backspace goes up. `d` downloads, `u` uploads a local file or folder,
`n` creates a folder, `x` deletes, `i` shows info, `r` renames, `c`
sets the comment, `m` moves, and `a` makes an alias. A download goes
to `name.hpf` first. If one is there, `hlc` asks whether to resume it.

Each row of the file list starts with an emoji for the file's Mac
type, such as 📁 for a folder, 📝 for text, and 📷 for an image. The
size has its own column, in openline's form, such as `<1K`, `12K`, or
`1.5M`. A folder shows its item count, such as `(3)`. An alias shows
`-`, and its row is gray.

The Tasks tab shows each transfer with its progress, and each request
that waits for its reply, in the order the user started them. A
transfer shows its rate, its bytes, and the time left, such as
`12K/sec 1.5M/2M 0:42`. A gauge under a transfer of known size shows
how much is done. A download that waits in the server's queue shows
its place, and its gauge fills as the queue moves. A task that ends
well stays in its place with a full gauge, and goes 500 ms after the
last task ends. A task that fails or that the user cancels goes at
once. The error line in the chat tells why a task failed. A scroll
bar on the right border shows when the tasks do not all fit. A click
selects a task. Enter cancels the selected download, upload, or
request that only reads. Enter does nothing on a request that changes
the server. Enter on the `Logging in...` step of the login ends the
connection. A cancelled download that waits in the server's queue
leaves the queue. A cancelled upload closes its connection.

Each transfer waits until each older transfer ends. A waiting transfer
shows `(queued)`, and the list title counts these transfers. `s` starts
the selected transfer at once. The `queue transfers` row of the options
form (Alt-O) turns the queue off, and then each transfer starts at
once. The queue is on when `hlc` starts.

The Threaded News tab has three panes: a tree of bundles (📚) and
categories (📰) on the left, the threads of the selected category on the
top right, and the selected article on the bottom right. Tab moves from
pane to pane. In the tree, → or Enter opens a bundle, and ← closes it or
goes to the bundle that holds it. Enter on a category moves to its
threads. Each move in the thread list shows that article in the preview,
and Enter opens it. ← goes back to the tree. `p` posts, `r` replies to
the open article, `x` deletes, and `b` and `c` create a bundle or a
category in the selected bundle, or in the bundle that holds the
selected category.

On the Admin tab, Enter edits an account, `n` creates one, `o` opens
one by login, and `x` deletes one. In the account form, Space turns
the selected privilege on or off. The tab shows with the open user or
the create user privilege. Without the open user privilege, it lists
no accounts and `o` is gray. `n` needs the create user privilege, and
`x` needs the delete user privilege. The form saves a new account with
the create user privilege, and a changed account with the modify user
privilege.

## License

MIT or Apache-2.0, at your option.
