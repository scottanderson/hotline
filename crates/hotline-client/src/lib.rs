//! Async Hotline client session.
//!
//! `HotlineClient` owns the TCP connection, performs the `TRTP` handshake,
//! and runs a background read loop that demultiplexes incoming
//! transactions: replies are routed back to the matching `request()` call
//! by transaction ID, and server-initiated transactions (chat, user
//! join/leave, news posts, ...) are delivered through the
//! `mpsc::UnboundedReceiver<Transaction>` [`HotlineClient::connect`] returns
//! alongside the client.
//!
//! ```no_run
//! use hotline_client::{ClientUserInfo, HotlineClient};
//!
//! # async fn run() -> hotline_client::Result<()> {
//! let (client, mut events) = HotlineClient::connect("127.0.0.1:5500").await?;
//! client.login("guest", "").await?;
//! let info = ClientUserInfo {
//!     name: "RustHL".into(),
//!     ..ClientUserInfo::default()
//! };
//! client.agree(&info).await?;
//! client.send_chat("Hello").await?;
//! while let Some(push) = events.recv().await {
//!     println!("{push:?}");
//! }
//! # Ok(())
//! # }
//! ```

mod accounts;
mod ban;
mod client;
mod client_info_text;
mod client_user_info;
mod error;
mod file_download;
mod file_upload;
mod files;
mod flat_file;
mod folder_download;
mod folder_receiver;
mod folder_sender;
mod folder_upload;
mod joined_chat;
mod login_result;
mod news;

pub use accounts::{AccountChange, Password, UserAccount};
pub use ban::Ban;
pub use client::{HotlineClient, REQUEST_TIMEOUT};
pub use client_info_text::ClientInfoText;
pub use client_user_info::ClientUserInfo;
pub use error::{ClientError, Result};
pub use file_download::FileDownload;
pub use file_upload::FileUpload;
pub use files::FileInfo;
pub use folder_download::FolderDownload;
pub use folder_receiver::FolderReceiver;
pub use folder_sender::FolderSender;
pub use folder_upload::FolderUpload;
pub use joined_chat::JoinedChat;
pub use login_result::LoginResult;
pub use news::{NewsArticle, TEXT_PLAIN};
