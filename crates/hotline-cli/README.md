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
| `--host`         | `127.0.0.1` | the server                                     |
| `--port`         | 5500        | the server's port                              |
| `--login`        | (guest)     | the account login                              |
| `--password`     | (none)      | the password, also read from `HLC_PASSWORD`    |
| `--name`         | `RustHL`    | the name in the user list                      |
| `--icon`         | 0           | the icon ID                                    |
| `--download-dir` | `.`         | the folder that downloads go to                |
| `--encoding`     | `macintosh` | the text encoding, the same as the server's    |
| `--tracker`      |             | list a tracker's servers and exit, no login    |
| `--no-bell`      | off         | do not ring the terminal bell                  |

Use `HLC_PASSWORD` in place of `--password` when other users share the
host, because they can read a command line.

The line at the bottom of the screen shows the keys of the focused
pane.
The tabs are Chat, News, Files, and Threaded News, then Admin with the
account privileges. Each tab has a number, which the tab bar shows. F1
to F9, or Alt with 1 to 9, opens a tab. When you have joined a private
chat room, a row under the tab bar of Chat and of each room shows
Public, the public chat, then each room. Alt-, and Alt-. move to the
previous and the next entry of that row. Each tab has a main pane and
the user list. Tab moves between them, and Esc goes back to Chat. On
Chat, Esc asks whether to quit. Ctrl-C asks too, from anywhere, and a
second Ctrl-C quits.
When the server ends the connection, a box shows its message, or that
the connection closed. Enter, Esc, or Ctrl-C closes the box and quits.

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
| Alt-,  | Chat, a room         | the previous room, or Public                    |
| Alt-.  | Chat, a room         | the next room, or Public                        |
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
opens a menu that also starts a private chat. Only the palette lists
"Cancel a transfer".

On macOS, Option with a letter types a symbol by default. Set the
Option key to send Meta for the Alt keys. In Terminal, turn on "Use
Option as Meta key". In iTerm2, set the left Option key to "Esc+".

On the Files tab, Enter opens a folder or downloads a file, and
Backspace goes up. `d` downloads, `u` uploads a local file or folder,
`n` creates a folder, `x` deletes, `i` shows info, `r` renames, `c`
sets the comment, `m` moves, and `a` makes an alias. A download goes
to `name.hpf` first. If one is there, `hlc` asks whether to resume it.

The Threaded News tab has no user list. It has three panes: a tree of
bundles (🗞️) and categories (📰) on the left, the threads of the
selected category on the top right, and the selected article on the
bottom right. Tab moves from pane to pane. In the tree, → or Enter
opens a bundle, and ← closes it or goes to the bundle that holds it.
Enter on a category moves to its threads. Each move in the thread list
shows that article in the preview, and Enter opens it. ← goes back to
the tree. `p` posts, `r` replies to the open article, `x` deletes, and
`b` and `c` create a bundle or a category in the selected bundle, or in
the bundle that holds the selected category.

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
