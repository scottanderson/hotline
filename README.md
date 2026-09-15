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
│   ├── consts/                          # AccessPrivileges (64-bit myField_UserAccess bitmap)
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
│   └── ban/, banner/, console/, flood/  # further pluggable providers (bans, server
│                                        # banner, console commands, flood control)
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
storage, bans, the server banner, console commands, and flood control
are each behind their own small trait (`AccountProvider`, `FileProvider`,
`FlatNewsProvider`, ...); the embedding application supplies the concrete
backend for each to `Server::new`, so `hotline-server` itself never
owns any of that data directly.

## Status

Transaction framing, field encoding, the handshake, login (against
pluggable named accounts, not just guest), chat (including private,
invitation-gated chat rooms), file listing, file transfer (`HTXF`, the
second-port transfer handshake), account CRUD, and threaded news
(bundles, categories, and article threads) are wired up end to end,
including server-side permission checks against `myField_UserAccess` (110). Not yet
implemented: the tracker (`HTRK`) beyond its wire-format types.

## Getting started

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace
cargo fmt --all

# terminal 1
cargo run -p hotline-daemon -- --port 5500

# terminal 2
cargo run -p hotline-cli -- --host 127.0.0.1 --port 5500 --login guest --nick RustHL
```
