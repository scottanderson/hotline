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

| Key    | Where                | Effect                                          |
|--------|----------------------|-------------------------------------------------|
| Ctrl-G | anywhere             | a broadcast, with the broadcast privilege       |
| Ctrl-O | anywhere             | your name, icon, and options                    |
| Ctrl-K | anywhere             | cancel a download                               |
| ⇧Enter | a text box           | a new line (also Alt-Enter or Ctrl-J)           |
| Enter  | user list            | send a private message                          |
| `i`    | user list            | the user's info                                 |
| `k`    | user list            | disconnect the user, with the privilege         |
| Ctrl-A | a room               | invite a user                                   |
| Ctrl-T | a room               | set the subject                                 |
| Ctrl-W | a room               | leave the room                                  |

A right click on a user opens a menu that also starts a private chat.

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
