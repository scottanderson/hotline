# hotline-tracker

The [Hotline] tracker protocol (`HTRK`). A client asks a tracker for its
server list over TCP. A server sends a beacon to a tracker over UDP to
stay in the list. This crate encodes and decodes those messages. It has
no I/O.

[Hotline]: https://en.wikipedia.org/wiki/Hotline_Communications

## Example

```rust
use hotline_proto::text_encoding;
use hotline_tracker::{encode_client_request_v1, ServerBeacon};

let encoding = text_encoding::DEFAULT;

// The first bytes that a client sends on the TCP port.
let request = encode_client_request_v1();
assert_eq!(&request[..4], b"HTRK");

// The datagram that a server sends on the UDP port.
let beacon = ServerBeacon {
    port: 5500,
    user_count: 3,
    pass_id: 0,
    name: "My Server".into(),
    description: "A Hotline server".into(),
    password: "".into(),
};
let datagram = beacon.encode(encoding).unwrap();
assert!(!datagram.is_empty());
```

A client reads the reply with `decode_reply_header`, then
`decode_server_list`. `TCP_PORT` and `UDP_PORT` are the default ports.

## The other crates

This crate is part of [hotline-rs]. [`hotline-daemon`] sends beacons,
and [`hotline-cli`] gets server lists.

[hotline-rs]: https://github.com/scottanderson/hotline
[`hotline-daemon`]: https://crates.io/crates/hotline-daemon
[`hotline-cli`]: https://crates.io/crates/hotline-cli

## License

MIT or Apache-2.0, at your option.
