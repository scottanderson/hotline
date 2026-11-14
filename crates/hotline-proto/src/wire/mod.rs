//! Readers and writers of wire buffers.
//!
//! [`Writer`] writes a buffer in a chain of calls. Its [`Storage`] is
//! [`Fixed`], a `[u8; N]` on the stack, when the size is known at compile
//! time, or [`Growable`], a `Vec<u8>`, when it is not. Most call sites name
//! [`ArrayWriter`] or [`VecWriter`].
//!
//! A second type parameter records if the writer has a text encoding.
//! [`Writer::pstring`] exists only after [`Writer::with_encoding`], so a
//! `pstring` without an encoding does not compile.
//!
//! [`ArrayReader`] reads an `N`-byte array. [`SliceReader`] reads a
//! borrowed buffer of unknown length, such as one that [`VecWriter`] wrote.

mod array_reader;
mod fixed;
mod growable;
mod no_encoding;
mod pstring_too_long;
mod slice_reader;
mod storage;
mod with_encoding;
mod writer;

pub use array_reader::ArrayReader;
pub use fixed::Fixed;
pub use growable::Growable;
pub use no_encoding::NoEncoding;
pub use pstring_too_long::PStringTooLong;
pub use slice_reader::SliceReader;
pub use storage::Storage;
pub use with_encoding::WithEncoding;
pub use writer::{ArrayWriter, VecWriter, Writer};
