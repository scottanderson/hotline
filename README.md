# Hotline

A Rust workspace for the [Hotline] protocol (`TRTP`): a client, a
server, and the tracker protocol.

[Hotline]: https://en.wikipedia.org/wiki/Hotline_Communications

## Crates

| Crate                                         | Contents                                         |
|-----------------------------------------------|--------------------------------------------------|
| [`hotline-proto`](crates/hotline-proto)       | the wire format, with no I/O and no async        |
| [`hotline-codec`](crates/hotline-codec)       | a Tokio codec that frames transactions           |
| [`hotline-client`](crates/hotline-client)     | an async client session                          |
| [`hotline-server`](crates/hotline-server)     | an async server, with storage behind traits      |
| [`hotline-tracker`](crates/hotline-tracker)   | the `HTRK` tracker protocol                      |
| [`hotline-cli`](crates/hotline-cli)           | `hlc`, a terminal client                         |
| [`hotline-daemon`](crates/hotline-daemon)     | `hls`, a server with its data on disk            |

Each crate's README tells how to use it. `HLProtocol.md` is the
protocol reference.

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
* `hlc`, a terminal client that sends every client transaction except
  the banner download.

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

## License

MIT or Apache-2.0, at your option.
