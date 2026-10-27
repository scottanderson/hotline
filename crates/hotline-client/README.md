# hotline-client

An async [Hotline] client session on Tokio. `HotlineClient` connects,
does the `TRTP` handshake, and logs in. Each request returns the
server's reply. The server's pushes, such as chat and user changes,
come through a channel.

[Hotline]: https://en.wikipedia.org/wiki/Hotline_Communications

## Example

```rust,no_run
use hotline_client::{ClientUserInfo, HotlineClient};

#[tokio::main]
async fn main() -> hotline_client::Result<()> {
    let (client, mut events) = HotlineClient::connect("127.0.0.1:5500").await?;
    client.login("guest", "").await?;
    let info = ClientUserInfo {
        name: "RustHL".into(),
        ..ClientUserInfo::default()
    };
    client.agree(&info).await?;
    client.send_chat("Hello").await?;
    while let Some(push) = events.recv().await {
        println!("{push:?}");
    }
    Ok(())
}
```

## Features

- Chat, private messages, and private chat rooms.
- The user list and user info.
- File lists, file and folder downloads and uploads over `HTXF`, and
  resumed downloads. A download keeps the resource fork and the
  Finder info.
- Classic news, and threaded news with bundles, categories, and
  articles.
- Account administration.

## The other crates

This crate is part of [hotline-rs]. [`hotline-cli`] is a terminal
client on top of it. [`hotline-proto`] has the wire types.

[hotline-rs]: https://github.com/scottanderson/hotline
[`hotline-cli`]: https://crates.io/crates/hotline-cli
[`hotline-proto`]: https://crates.io/crates/hotline-proto

## License

MIT or Apache-2.0, at your option.
