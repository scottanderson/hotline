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
HLC_PASSWORD=secret hlc --host example.com --login alice --nick Alice
```

| Flag             | Default     | Effect                                         |
|------------------|-------------|------------------------------------------------|
| `--host`         | `127.0.0.1` | the server                                     |
| `--port`         | 5500        | the server's port                              |
| `--login`        | (guest)     | the account login                              |
| `--password`     | (none)      | the password, also read from `HLC_PASSWORD`    |
| `--nick`         | `RustHL`    | the name in the user list                      |
| `--icon`         | 0           | the icon ID                                    |
| `--download-dir` | `.`         | the folder that downloads go to                |
| `--encoding`     | `macintosh` | the text encoding, the same as the server's    |
| `--tracker`      |             | list a tracker's servers and exit, no login    |

Use `HLC_PASSWORD` in place of `--password` when other users share the
host, because they can read a command line.

The line at the bottom of the screen shows the keys that work now.
The tabs are Chat, News, Files, and Threaded News, then Admin with the
account privileges, then one for each private chat room. Each tab has
a number, which the tab bar shows. F1 to F9, or Alt with
1 to 9, opens a tab. Each tab has a main pane and the user list. Tab
moves between them, and Esc goes back to Chat. On Chat, Esc asks
whether to quit. Ctrl-C asks too, from anywhere, and a second Ctrl-C
quits.

A tab label turns yellow when the tab changes while another tab shows.
The Chat tab and each room tab change with each new line, and the News
tab changes with each new post. The label turns back when you open the
tab.

| Key    | Where                | Effect                                          |
|--------|----------------------|-------------------------------------------------|
| Ctrl-P | outside a dialog     | the commands that work now; type to filter      |
| Ctrl-O | a room, user list    | the actions of the room or the selected user    |
| Alt-A  | outside a dialog     | a broadcast, with the broadcast privilege       |
| Alt-O  | outside a dialog     | your name, icon, and options                    |
| Alt-I  | a room               | invite a user                                   |
| Alt-S  | a room               | set the subject                                 |
| Alt-W  | a room               | leave the room                                  |
| ⇧Enter | a text box           | a new line (also Alt-Enter or Ctrl-J)           |
| Ctrl-W | a text box           | delete the last word (also Alt-⌫)               |
| Ctrl-U | a text box           | delete the whole text                           |
| Enter  | user list            | send a private message                          |
| `i`    | user list            | the user's info                                 |
| `k`    | user list            | disconnect the user, with the privilege         |

Alt-X also opens the palette, for a terminal that takes Ctrl-P.
Shift-F10 and the Menu key also open the actions. The palette and
the action menu show the key of each entry. A right click on a user
opens a menu that also starts a private chat. Only the palette lists
"Cancel a download".

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
the selected privilege on or off.

## License

MIT or Apache-2.0, at your option.
