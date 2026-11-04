# hotline-server

An async [Hotline] server on Tokio. `Server` accepts connections and
gives each one a `Session`. A second listener on the next port takes
the `HTXF` file transfers. The application supplies the storage
through traits, so this crate keeps no data of its own.

[Hotline]: https://en.wikipedia.org/wiki/Hotline_Communications

## Example

```rust,no_run
use hotline_server::{Providers, Server, ServerConfig};

#[tokio::main]
async fn main() -> hotline_server::Result<()> {
    let config = ServerConfig {
        bind_addr: "0.0.0.0:5500".into(),
        ..ServerConfig::default()
    };
    Server::new(config, Providers::default()).run().await
}
```

`Providers::default()` keeps all data in memory. To keep the data on
disk, put your own implementations in `Providers`.

## Features

- Login with named accounts, and account administration.
- Chat, private messages, and private chat rooms.
- File transfer: lists, downloads, uploads, folders, resume, and a
  download queue.
- Classic news, and threaded news.
- Privilege checks against each account's access bits.
- Bans, flood control, connection limits, and a login timeout.
- The server banner, console commands in chat, and activity logs.

## Providers

`Providers` holds one implementation of each trait:

| Field            | Trait                   | Default                   |
|------------------|-------------------------|---------------------------|
| `accounts`       | `AccountProvider`       | `InMemoryAccountProvider` |
| `files`          | `FileProvider`          | `InMemoryFileProvider`    |
| `flat_news`      | `FlatNewsProvider`      | `InMemoryNewsProvider`    |
| `threaded_news`  | `ThreadedNewsProvider`  | `InMemoryNewsProvider`    |
| `account_stores` | `AccountStores`         | `NoAccountStores`         |
| `bans`           | `BanProvider`           | `InMemoryBanProvider`     |
| `flood`          | `FloodPolicy`           | `NoFloodPolicy`           |
| `banner`         | `BannerProvider`        | `NoBanner`                |
| `commands`       | `ChatCommandDispatcher` | `DefaultChatCommands`     |
| `activity`       | `ActivityLog`           | `NoActivityLog`           |
| `policy`         | `ServerPolicy`          | `OpenlinePolicy`          |

`AccountStores` can give an account its own files and threaded news,
like openline's `Users/<login>/Files` and `Users/<login>/News`
folders. An account without its own store uses `files` and
`threaded_news`. An error from `AccountStores` refuses the request. It
does not fall back to the shared store. Flat news is the same for every
account.

## Design

- `Session` holds the state of one connection: the user ID, the name,
  and the privileges. `SharedState` holds the state that all sessions
  see: the user list, the chat rooms, and a channel for each session.
- A session reaches another session only through that channel. The
  receiving session checks its own privileges before it sends a push.
- `ServerPolicy` holds the choices where Hotline servers differ.
  `OpenlinePolicy` matches openline.

## The other crates

This crate is part of [hotline-rs]. [`hotline-daemon`] is a server
program on top of it, with storage on disk.

[hotline-rs]: https://github.com/scottanderson/hotline
[`hotline-daemon`]: https://crates.io/crates/hotline-daemon

## License

MIT or Apache-2.0, at your option.
