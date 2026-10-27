# hotline-proto

The wire format of the [Hotline] protocol (`TRTP`). It encodes and
decodes transactions, fields, the handshakes, and the `HTXF` file
transfer framing. It has no I/O and no async, so you can use it in a
dissector, a fuzzer, or a synchronous program.

[Hotline]: https://en.wikipedia.org/wiki/Hotline_Communications

## Example

```rust
use hotline_proto::{text_encoding, Field, FieldId, Opcode, Transaction};

let encoding = text_encoding::DEFAULT;
let request = Transaction::request(
    Opcode::ChatSend,
    1,
    vec![Field::string(FieldId::Data, "Hello")],
);
let bytes = request.encode(encoding).unwrap();

let received = Transaction::decode_from_client(&bytes, encoding).unwrap();
assert_eq!(received.field(FieldId::Data).unwrap().as_string(), "Hello");
```

## Design

- `Opcode` and `FieldId` each cover both directions. Their variants
  use the names of the `myTran_*` and `myField_*` constants in the
  protocol reference, and `Debug` prints those names.
  `TransactionType::Request` and `TransactionType::Push` give the
  direction.
- `Transaction` does not store the two length fields or the field
  count. `encode` calculates them. `TransactionHeader` is the 20-byte
  prefix, which a streaming decoder reads before the body.
- Every text conversion takes the connection's `&'static Encoding`.
  The default is Mac OS Roman.

## The other crates

This crate is part of [hotline-rs]. [`hotline-codec`] frames
transactions on a Tokio stream. [`hotline-client`] and
[`hotline-server`] are async sessions on top of it.

[hotline-rs]: https://github.com/scottanderson/hotline
[`hotline-codec`]: https://crates.io/crates/hotline-codec
[`hotline-client`]: https://crates.io/crates/hotline-client
[`hotline-server`]: https://crates.io/crates/hotline-server

## License

MIT or Apache-2.0, at your option.
