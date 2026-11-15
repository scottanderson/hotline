# hotline-daemon

`hls`, a [Hotline] server. It runs [`hotline-server`] with its data on
disk.

[Hotline]: https://en.wikipedia.org/wiki/Hotline_Communications
[`hotline-server`]: https://crates.io/crates/hotline-server

## Install

You need Rust 1.96 or later.

```sh
cargo install hotline-daemon
hls --port 5500
```

`hls --help` lists all flags.

## Files

`hls` keeps its data in the current directory by default. `-C DIR`
(`--directory`) runs `hls` in `DIR`, so each relative path is in `DIR`:

| Path            | Contents                         | Flag                |
|-----------------|----------------------------------|---------------------|
| `accounts/`     | one folder for each account      | `--accounts-dir`    |
| `trash/`        | the folders of deleted accounts  | `--trash-dir`       |
| `files/`        | the files clients see            | `--files-dir`       |
| `news.txt`      | classic news                     | `--news-file`       |
| `news.json`     | threaded news                    | `--news-tree-file`  |
| `bans.json`     | address bans                     | `--ban-file`        |
| `tracker-id`    | the ID that trackers list you by | `--tracker-id-file` |

The Finder info, the comment, and the resource fork of `files/a.txt` are
in its AppleDouble file, `files/._a.txt`. Clients do not see these files.
hls changes these files as macOS does. The comment is the extended
attribute `com.apple.metadata:kMDItemFinderComment`, and a change keeps
the other extended attributes. hls does not read or change a `._` file
that is not empty and not a valid AppleDouble file, or whose owner is
not the owner of its item. A new `._` file gets the owner of its item
only when hls runs as that user or as root.
`--fork-store none` keeps no Finder info, comment, or resource fork. Then
a client sees and can name each `._` file.

Use `--fork-store none` where the folder already has other `._` files.
On macOS, a volume with no native extended attributes, such as SMB or
exFAT, keeps its own `._` files with the same names. A share that
refuses `._` names, such as a Samba share with `veto files = ._*`,
cannot keep the sidecars.

A change of `--fork-store` on existing files can lose data. With
`none`, a client sees each sidecar and can change or replace it, and a
rename leaves the sidecar behind. With `apple-double`, a client file
whose name starts with `._` becomes a hidden sidecar, and an upload of
its item deletes it.

Text in these files is UTF-8. On the wire, text uses Mac OS Roman by
default. `--encoding utf-8` changes the wire encoding of every
connection. It takes a WHATWG label, such as `shift_jis`. Every client
must use the same encoding, for example `hlc --encoding utf-8`.

Each account is a folder, `accounts/<login>/`, as on openline. The
folder holds the account in `account.json`. A new account fails if its
folder exists. A rename of the account renames its folder. A delete
moves the folder into `trash/`, as openline moves it to the trash. If
`trash/` holds a folder of that name, the folder gets the first free
name with a number from 1 at the end, such as `alice1`. The move does
not copy, so put `--trash-dir` on the file system of `--accounts-dir`.
`--no-trash` removes the folder with all that it holds.

An account can have its own files and threaded news, as on openline.
Make the folder `accounts/<login>/files/` for its own files, and the
file `accounts/<login>/news.json` for its own threaded news. An empty
`{}` starts an empty tree. The account then sees only these, and other
accounts see the shared ones. Classic news is the same for every
account. A `files` item that is not a folder, or a `news.json` that is
not a file, is not a store, so the account gets the shared one. If the
account's store exists but does not open, such as a `news.json` that
does not parse, each request to that store gets an error, and `hls` logs
it at the debug level. The account does not get the shared store.

`hls --import-news <folder>` reads the threaded news of an openline
server: its `News` folder. It writes the tree to `--news-tree-file`,
then exits. It does not overwrite a file that exists. The text in the
folder uses `--encoding`.

Clients connect on `--port` (5500 by default). File transfers use the
next port, so open both in your firewall.

## First run

When the accounts directory is empty, `hls` creates two accounts:

* `guest`, with no password and basic access.
* `admin`, with full access and a random password.

`hls` prints the admin password once, on the first run:

```text
created account "admin" with password "3f9c0a7b12e45d68" in accounts
```

Record it, because `hls` does not print it again. Log in as `admin` to
change it, or to add accounts. Account files keep passwords as plain
text. On Unix, `hls` makes a new accounts directory and each account
file readable only by the user that runs it.

## Limits

| Flag                            | Default | Effect                                               |
|---------------------------------|---------|------------------------------------------------------|
| `--max-connections`             | 500     | open connections in total (0 means no limit)         |
| `--max-connections-per-address` | 5       | open connections from one address (0 means no limit) |
| `--max-downloads`               | 100     | downloads at the same time, before others queue      |
| `--max-downloads-per-client`    | 10      | downloads for one user, before others queue          |

A client that does not log in within 60 seconds is disconnected. A
client that floods the server is banned for 30 minutes. This includes
too many transactions, messages, chat bytes, name changes, repeated
lines, or connections.

## Logging

`hls` logs at INFO to standard output. `-v` adds debug lines and `-vv`
trace lines. `-q` logs warnings, `-qq` only errors, and `-qqq` nothing.
`RUST_LOG` overrides both flags with a list of `target=level`
directives, such as `RUST_LOG=hotline_server=debug,info`. An invalid
`RUST_LOG` prints a note, and the flags apply.

## Activity logs

Each log is off until you give it a file:

```sh
hls --download-log downloads.log --upload-log uploads.log \
    --connect-log connects.log --account-log accounts.log
```

Each line has tab-separated columns, like openline's logs. The first
line of each file names the columns.

## Console commands

`--console-prefix /` lets users type commands in chat. Without it, all
chat goes to the room.

| Command                                  | Effect                               |
|------------------------------------------|--------------------------------------|
| `/users`                                 | lists connected users                |
| `/access <uid\|nick> [FIELD=0\|1 ...]`   | shows or changes a user's privileges |
| `/0wn <uid\|nick> icon=<n> name=<text>`  | changes a user's icon or name        |
| `/unban <address>`                       | removes an address ban               |

`/unban` needs the privilege to disconnect users. The others need the
matching server-only account flag.

## Trackers and banner

`--tracker host[:port]` registers the server with a tracker. Repeat it
for more than one tracker. `--banner` takes an image file, or an
`http://` or `https://` URL.

## Stopping

Ctrl-C or SIGTERM stops `hls`. When a user is connected and stdin is
a terminal, `hls` asks for a quit message, as openline's quit window
does. An empty message stops `hls` at once. `hls` sends a message to
each user, and a client shows it and disconnects. From then on, `hls`
closes each new connection on each of its ports. `hls` then waits for
each connection to close. A second Ctrl-C or SIGTERM, at the prompt
or during the wait, stops it at once. When each user leaves while
`hls` asks, it stops at once too.
A user whose queue is full does not get the message, and `hls` waits
for that connection too. Without a terminal, `hls` stops at once.

## License

MIT or Apache-2.0, at your option.
