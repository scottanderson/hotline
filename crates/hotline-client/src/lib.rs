//! Async Hotline client session.
//!
//! `HotlineClient` owns the TCP connection, performs the `TRTP` handshake,
//! and runs a background read loop that demultiplexes incoming
//! transactions: replies are routed back to the matching `request()` call
//! by transaction ID, and server-initiated transactions (chat, user
//! join/leave, news posts, ...) are delivered through the
//! `mpsc::UnboundedReceiver<Transaction>` [`HotlineClient::connect`] returns
//! alongside the client.

mod client;
mod client_info_text;
mod error;
mod file_download;
mod file_upload;
mod flat_file;
mod folder_download;
mod folder_receiver;
mod folder_sender;
mod folder_upload;
mod login_result;

pub use client::HotlineClient;
pub use client_info_text::ClientInfoText;
pub use error::{ClientError, Result};
pub use file_download::FileDownload;
pub use file_upload::FileUpload;
pub use folder_download::FolderDownload;
pub use folder_receiver::FolderReceiver;
pub use folder_sender::FolderSender;
pub use folder_upload::FolderUpload;
pub use login_result::LoginResult;
