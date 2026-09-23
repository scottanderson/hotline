//! File storage backing `FILE_LIST`/`FILE_GET`/`FILE_PUT`.
//!
//! [`FileProvider`] is the storage interface; `hotline-server` only
//! defines it and provides [`InMemoryFileProvider`] as a default. The
//! embedding application supplies the real backend to
//! [`crate::Server::new`] the same way it supplies an [`AccountProvider`]
//! (see that trait's module docs). Every call carries a [`FileRequester`].
//! A backend can use it to show different logins different files: a
//! per-account home directory, a members-only folder, and so on.
//!
//! [`AccountProvider`]: crate::AccountProvider

mod dir;
mod folder_item;
mod memory;
mod node;
mod provider;
mod requester;
mod writer;

use tokio::io::{AsyncRead, AsyncWrite};

pub use folder_item::FolderItem;
pub use memory::InMemoryFileProvider;
pub use provider::FileProvider;
pub use requester::FileRequester;

use dir::Dir;
use node::Node;

/// A boxed, already-open async byte source, the shape [`FileProvider::open_read`]
/// returns so implementations aren't tied to `tokio::fs::File` specifically.
pub type BoxedReader = Box<dyn AsyncRead + Send + Unpin>;

/// A boxed, already-open async byte sink. See [`BoxedReader`].
pub type BoxedWriter = Box<dyn AsyncWrite + Send + Unpin>;

/// Suffix an upload's on-disk name carries until it's finished. Matches
/// openline's non-HFS `HTft`/`HTLC` "still uploading" marker convention,
/// via [`type_and_creator_for_name`].
pub const PARTIAL_SUFFIX: &str = ".hpf";

/// [`type_and_creator_for_name`]'s result for [`PARTIAL_SUFFIX`], the
/// marker a real client's own directory listing checks to decide
/// whether to offer resuming an upload.
pub const PARTIAL_TYPE: [u8; 4] = *b"HTft";

/// See [`PARTIAL_TYPE`].
pub const PARTIAL_CREATOR: [u8; 4] = *b"HTLC";

/// The on-disk name `name`'s upload is stored under until
/// [`FileProvider::finish_upload`] renames it. See [`PARTIAL_SUFFIX`].
pub fn partial_upload_name(name: &str) -> String {
    format!("{name}{PARTIAL_SUFFIX}")
}

/// `file_type`/`creator` inferred from a file's extension, the same
/// fallback openline's non-HFS (Windows) build uses in place of real Mac
/// type/creator metadata (`_FSWinExtToMacTypeCode` in `UFileSys(W).cp`).
pub fn type_and_creator_for_name(name: &str) -> ([u8; 4], [u8; 4]) {
    let ext = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    match ext.to_ascii_uppercase().as_str() {
        "TXT" => (*b"TEXT", *b"ttxt"),
        "SIT" => (*b"SITD", *b"SIT!"),
        "DAT" => (*b"BINA", *b"hDmp"),
        "EXE" => (*b"DEXE", *b"CWIE"),
        "ZIP" => (*b"ZIP ", *b"ZIP "),
        "DOC" => (*b"W6BN", *b"MSWD"),
        "CPP" | "C" | "CP" | "H" | "HPP" => (*b"TEXT", *b"CWIE"),
        "HPF" => (PARTIAL_TYPE, PARTIAL_CREATOR),
        "HBM" => (*b"HTbm", *b"HTLC"),
        "HNZ" => (*b"HLNZ", *b"HTLC"),
        "GIF" => (*b"GIFf", *b"GKON"),
        "JPG" => (*b"JPEG", *b"GKON"),
        "DIB" | "BMP" => (*b"BMP ", *b"GKON"),
        "PCT" => (*b"PICT", *b"GKON"),
        "FPX" => (*b"FPix", *b"TVOD"),
        "MAC" | "PNT" => (*b"PNTG", *b"TVOD"),
        "PSD" => (*b"8BPS", *b"TVOD"),
        "PNG" => (*b"PNGf", *b"TVOD"),
        "SGI" => (*b"SGI ", *b"TVOD"),
        "TGA" => (*b"TPIC", *b"TVOD"),
        "TIF" => (*b"TIFF", *b"GKON"),
        "QT" | "MOV" => (*b"MooV", *b"TVOD"),
        "MPA" | "MP2" | "MPG" => (*b"MPEG", *b"TVOD"),
        "AVI" => (*b"VfW ", *b"TVOD"),
        "DV" | "DIF" => (*b"dvc!", *b"TVOD"),
        "SWF" => (*b"SWFL", *b"SWF2"),
        "FLC" | "FLI" => (*b"FLI ", *b"TVOD"),
        "QD3" | "3DM" => (*b"3DMF", *b"TVOD"),
        "MP3" => (*b"MP3 ", *b"MAmp"),
        "RAM" | "RM" | "RA" | "RAE" => (*b"RAE ", *b"REAL"),
        "AIF" => (*b"AIFF", *b"TVOD"),
        "AU" | "SND" | "ULW" => (*b"ULAW", *b"TVOD"),
        "WAV" => (*b"WAVE", *b"TVOD"),
        "SD2" => (*b"Sd2f", *b"TVOD"),
        "SMF" | "KAR" | "MID" => (*b"MiDi", *b"TVOD"),
        _ => (*b"BINA", *b"dosa"),
    }
}
