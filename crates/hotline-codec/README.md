# hotline-codec

A [`tokio_util`] codec that frames [Hotline] transactions.
`TransactionCodec` implements `Decoder` and `Encoder` for the
`Transaction` type of [`hotline-proto`]. Wrap a socket in
`tokio_util::codec::Framed` to get a `Stream` of transactions and a
`Sink` for them. The codec handles TCP fragmentation.

[`tokio_util`]: https://docs.rs/tokio-util
[Hotline]: https://en.wikipedia.org/wiki/Hotline_Communications
[`hotline-proto`]: https://crates.io/crates/hotline-proto

## Example

```rust
use bytes::BytesMut;
use hotline_codec::TransactionCodec;
use hotline_proto::{Field, FieldId, Opcode, Transaction};
use tokio_util::codec::{Decoder, Encoder};

let mut client = TransactionCodec::for_client(Default::default());
let mut server = TransactionCodec::for_server(Default::default());

let mut buf = BytesMut::new();
let request = Transaction::request(
    Opcode::ChatSend,
    1,
    vec![Field::string(FieldId::Data, "Hello")],
);
client.encode(request, &mut buf).unwrap();

let received = server.decode(&mut buf).unwrap().unwrap();
assert_eq!(received.field(FieldId::Data).unwrap().as_string(), "Hello");
```

Each codec reads its text encoding from an `EncodingHandle`. A
change to the handle applies to the next transaction.

## The other crates

This crate is part of [hotline-rs]. [`hotline-client`] and
[`hotline-server`] use it for their connections.

[hotline-rs]: https://github.com/scottanderson/hotline
[`hotline-client`]: https://crates.io/crates/hotline-client
[`hotline-server`]: https://crates.io/crates/hotline-server

## License

MIT or Apache-2.0, at your option.
