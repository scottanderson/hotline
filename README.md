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
Cargo.toml                     # workspace manifest
crates/
├── hotline-proto/             # wire format only, no I/O, no async
│   ├── wire.rs                # PacketWriter/PacketReader: builder-style fixed-buffer cursors
│   ├── numbers.rs             # signed int encoding (EncodedInt, no heap allocation)
│   ├── field.rs               # Field { id, data } + fixed/generic constructors
│   ├── field_id.rs            # ClientFieldId / ServerFieldId - see below
│   ├── opcode.rs              # ClientOpcode / ServerOpcode / Opcode
│   ├── transaction.rs         # Transaction: fixed fields + fields, one type
│   ├── types.rs               # HlDate, HlPath, FileNameWithInfo, UserNameWithInfo
│   ├── consts.rs              # AccessPrivileges (64-bit USER_ACCESS bitmap)
│   ├── handshake.rs           # ClientHello / ServerHello (the TRTP handshake)
│   └── error.rs               # ProtoError
│
├── hotline-codec/             # tokio_util Decoder/Encoder for Transaction
│
├── hotline-client/            # async client session (Tokio)
│   └── client.rs              # connect, handshake, login, request/reply matching,
│                              # broadcast stream for server-initiated transactions
│
├── hotline-server/            # async server framework (Tokio)
│   ├── server.rs              # listener, shared state, spawns one Session per connection
│   └── session.rs             # per-connection handshake + transaction dispatch
│
├── hotline-tracker/           # HTRK tracker protocol (client query + server beacon)
│
├── hotline-cli/               # `hlc` - terminal client binary using hotline-client
│
└── hotline-daemon/            # `hls` - server binary using hotline-server
```

`hotline-proto` has no `tokio` dependency: it only turns bytes into
`Transaction`/`Field` values and back, so it can be unit tested without a
socket and reused by non-async consumers (a packet-capture dissector, a
fuzzer, ...).

### `ClientFieldId` / `ServerFieldId`

The same numeric field tag can mean different things - or not exist at
all - depending on which direction sent it. Field tags are two distinct,
non-overlapping types rather than one shared enum: a tag only exists as
a constant on the type for the direction that actually sends it. Both
types' `Debug` impls print the original constant name. Since a bare
`Field` doesn't know its own direction, `Transaction`'s `Debug` is where
that gets resolved (it knows via `opcode()` whether it's a request,
reply, or push, and picks the matching table).

### `Transaction`

One type carries every wire field directly (`type_`, `trans`, `flag`,
`len`, `len2`, `fields`) rather than a separate header type wrapping a
payload. The field count (`hc`) isn't stored either - it's always
`fields.len()`. Streaming decode ([`Transaction::decode_prefix`] +
[`Transaction::finish_decode`]) still splits the read in two, since
knowing `len2` (and therefore how many more bytes to read) requires
reading the fixed prefix first.

### `hotline-codec`

`TransactionCodec` implements `tokio_util::codec::{Decoder, Encoder}` for
`Transaction`, used via `tokio_util::codec::Framed` in both the client
and server. A connection is a `Stream<Item = Result<Transaction, _>>` +
`Sink<Transaction>` - `reader.next().await` / `writer.send(tx).await` -
handling TCP fragmentation without a hand-rolled read loop.

## Status

Transaction framing, field encoding, the handshake, login, chat, and
file listing are wired up end-to-end (`hlc` can connect, log in, list
files, and chat with `hls`), including server-side permission checks
against `USER_ACCESS`. Not yet implemented: file transfer (`HTXF`, the
second-port transfer handshake), private chat rooms, news threading, and
the tracker (`HTRK`) beyond its wire-format types.

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
