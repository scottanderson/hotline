//! [`PacketBuilder`]: one generic, consuming-builder-style writer for
//! wire buffers, over a pluggable [`Storage`] backing: [`Fixed`] (a
//! stack-allocated `[u8; N]`, no heap allocation, panics on overflow)
//! when the total size is known at compile time, [`Growable`] (a
//! `Vec<u8>`) when it isn't (e.g. a variable number of strings).
//! [`PacketWriter`]/[`VecWriter`] are the two instantiations most call
//! sites actually name.
//!
//! A second, typestate type parameter tracks whether a text encoding has
//! been attached: [`PacketBuilder::pstring`] only exists once
//! [`PacketBuilder::with_encoding`] has run. A chain that writes a
//! `pstring` without ever choosing an encoding is a compile error, not a
//! silent default.
//!
//! [`PacketReader`] is the read-side counterpart, always fixed-size: you
//! always know how many bytes you're about to read.

mod fixed;
mod growable;
mod no_encoding;
mod packet_builder;
mod packet_reader;
mod pstring_too_long;
mod storage;
mod with_encoding;

pub use fixed::Fixed;
pub use growable::Growable;
pub use no_encoding::NoEncoding;
pub use packet_builder::{PacketBuilder, PacketWriter, VecWriter};
pub use packet_reader::PacketReader;
pub use pstring_too_long::PStringTooLong;
pub use storage::Storage;
pub use with_encoding::WithEncoding;
