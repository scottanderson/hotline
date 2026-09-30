# Hotline

A Rust workspace implementing the [Hotline](https://en.wikipedia.org/wiki/Hotline_Communications)
client/server wire protocol (`TRTP`).

## Layout

Client and server share transaction framing, field encoding, and the
login/chat/file/news transaction vocabulary. Only the behavior differs,
so the wire format lives in one crate with zero I/O opinions, and the
client/server crates each build an async (Tokio) session layer on top of
it. Framing a `Transaction` over a socket is itself shared behavior, so
it lives in its own crate rather than being duplicated in both.

```
Cargo.toml                               # workspace manifest
crates/
├── hotline-proto/                       # wire format only, no I/O, no async
│   ├── wire/                            # PacketWriter/PacketReader: builder-style fixed-buffer cursors
│   ├── numbers.rs                       # signed int encoding (EncodedInt, no heap allocation)
│   ├── field/                           # Field { id, data } + FieldList, FieldId (one type, both directions)
│   ├── opcode.rs                        # Opcode (one type, both directions) + TransactionType
│   ├── transaction/                     # Transaction + TransactionHeader
│   ├── types/                           # HlDate, HlPath, FileNameWithInfo, UserNameWithInfo
│   ├── consts/                          # Access (64-bit myField_UserAccess bitmap) + RequireFlags
│   ├── handshake/                       # ClientHandshake / ServerHandshake (the TRTP handshake)
│   ├── htxf/                            # HtxfHeader (file transfer handshake) + flat file object framing
│   ├── text_encoding.rs                 # per-connection wire text encoding (Mac Roman by default)
│   └── error.rs                         # ProtoError
│
├── hotline-codec/                       # tokio_util Decoder/Encoder for Transaction
│
├── hotline-client/                      # async client session (Tokio): connect, handshake,
│                                        # login, request/reply matching, broadcast stream
│                                        # for server-initiated transactions
│
├── hotline-server/                      # async server framework (Tokio)
│   ├── server/                          # listener, spawns one Session per connection,
│   │                                    # plus the second (HTXF) listener for file transfer
│   ├── session/                         # per-connection handshake + transaction dispatch
│   ├── state/                           # SharedState: cross-session user table, chat rooms,
│   │                                    # and the channels sessions use to reach each other
│   ├── account/                         # Account + AccountProvider trait; storage itself is
│   │                                    # supplied by the embedding application, not owned here
│   ├── file/                            # FileProvider trait, same plug-in shape as AccountProvider
│   ├── news/                            # FlatNewsProvider/ThreadedNewsProvider: bundles, categories, threads
│   └── activity/, ban/, banner/,        # further pluggable providers (activity logs,
│       console/, flood/                 # bans, server banner, console commands, flood control)
│
├── hotline-tracker/                     # HTRK tracker protocol (client query + server beacon)
│
├── hotline-cli/                         # `hlc`, terminal client binary (ratatui) using hotline-client
│
└── hotline-daemon/                      # `hls`, server binary using hotline-server
```

`hotline-proto` has no `tokio` dependency: it only turns bytes into
`Transaction`/`Field` values and back, so it can be unit tested without a
socket and reused by non-async consumers (a packet-capture dissector, a
fuzzer, ...).

### `FieldId` and `Opcode`

`HLProtocol.md` numbers both fields and opcodes with one flat list each
(`myField_*`, `myTran_*`), with no client/server split. A tag-by-tag
audit against it found no case where the same numeric value means
something structurally different depending on direction. So `FieldId`
and `Opcode` each cover both directions as one type, instead of the
separate `ClientFieldId`/`ServerFieldId` and `ClientOpcode`/`ServerOpcode`
types an earlier version of this crate used. `TransactionType::Request`/
`Push` still carry direction, since a raw wire value alone doesn't say
whether it came from a client or a server. Both types' `Debug` impls
print the original constant name.

### `Transaction` / `TransactionHeader`

`Transaction` holds `r#type`, `trans`, `flag`, and `fields`; it doesn't
store `len`/`len2`, since `encode()` recomputes them from `fields` every
time. The field count isn't stored either, it's always `fields.len()`.
Streaming decode needs `len2` before fields are readable, so
`TransactionHeader` is a separate, smaller type holding just the raw
20-byte prefix; it turns into a full `Transaction` once the body bytes
are in hand.

### `hotline-codec`

`TransactionCodec` implements `tokio_util::codec::{Decoder, Encoder}` for
`Transaction`, used via `tokio_util::codec::Framed` in both the client
and server. A connection is a `Stream<Item = Result<Transaction, _>>` +
`Sink<Transaction>` - `reader.next().await` / `writer.send(tx).await` -
handling TCP fragmentation without a hand-rolled read loop.

### Server session/state split

`Session` holds per-connection data (the socket's assigned user ID,
current name, granted access privileges, away status) and does the
actual request handling. `SharedState` holds everything visible across
connections: the connected user table, chat rooms, and the channels
sessions use to reach each other. Account storage, file storage, news
storage, bans, the server banner, console commands, flood control, and
activity logs are each behind their own small trait (`AccountProvider`,
`FileProvider`, `FlatNewsProvider`, ...); the embedding application bundles its concrete
backends into a `Providers` struct and passes that to `Server::new`, so
`hotline-server` itself never owns any of that data directly.

## Status

These parts work end to end:

* Transaction framing, field encoding, and the handshake.
* Login against named accounts, with account CRUD.
* Chat, including private chat rooms that need an invitation.
* File listing, and file and folder transfer over `HTXF`, the second
  port. Downloads queue past a limit, and an interrupted transfer can
  resume.
* Classic news, and threaded news with bundles, categories, and article
  threads.
* The server banner.
* Server-side permission checks against `myField_UserAccess` (110).
* Bans, and flood control that bans with openline's rules.
* Connection limits in total and per address, and a login timeout.
* In-chat console commands, behind an opt-in prefix.
* Activity logs for downloads, uploads, logins, and account changes.
* The `HTRK` tracker. `hls --tracker` registers over UDP, and
  `hlc --tracker` gets a server list over TCP.

`plan.md` lists open work and known differences from openline.

## Getting started

You need Rust 1.96 or later.

```sh
git clone https://github.com/scottanderson/hotline
cd hotline

cargo build --workspace
cargo test --workspace
cargo clippy --workspace
cargo fmt --all

# terminal 1
cargo run -p hotline-daemon -- --port 5500

# terminal 2
cargo run -p hotline-cli -- --host 127.0.0.1 --port 5500 --login guest --nick RustHL
```

## Running `hls`

`hls` keeps its data in the current directory by default:

| Path            | Contents                         | Flag                |
|-----------------|----------------------------------|---------------------|
| `accounts/`     | one JSON file for each account   | `--accounts-dir`    |
| `files/`        | the files clients see            | `--files-dir`       |
| `comments.json` | file and folder comments         | `--comments-file`   |
| `news.txt`      | classic news                     | `--news-file`       |
| `news.json`     | threaded news                    | `--news-tree-file`  |
| `bans.json`     | address bans                     | `--ban-file`        |
| `tracker-id`    | the ID that trackers list you by | `--tracker-id-file` |

Clients connect on `--port` (5500 by default). File transfers use the
next port, so open both in your firewall.

### First run

When the accounts directory is empty, `hls` creates two accounts:

* `guest`, with no password and basic access.
* `admin`, with full access and a random password.

`hls` prints the admin password once, on the first run:

```text
created account "admin" with password "3f9c0a7b12e45d68" in accounts
```

Record it, because `hls` does not print it again. Log in as `admin` to
change it, or to add accounts. Account files keep passwords as plain
text, so keep the accounts directory private.

### Limits

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

### Activity logs

Each log is off until you give it a file:

```sh
hls --download-log downloads.log --upload-log uploads.log \
    --connect-log connects.log --account-log accounts.log
```

Each line has tab-separated columns, like openline's logs. The first
line of each file names the columns.

### Console commands

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

### Trackers and banner

`--tracker host[:port]` registers the server with a tracker. Repeat it
for more than one tracker. `--banner` takes an image file, or an
`http://` or `https://` URL.
