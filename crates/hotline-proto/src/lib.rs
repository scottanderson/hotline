//! Wire-level implementation of the Hotline (`TRTP`) protocol.
//!
//! This crate is intentionally I/O-free: it only encodes/decodes byte
//! buffers into protocol structures (handshake, transaction header,
//! fields, and the composite types the field table refers to). The async
//! socket handling lives in `hotline-client` / `hotline-server`.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

pub mod consts;
pub mod error;
pub mod field;
pub mod handshake;
#[cfg(test)]
mod hl_protocol_md;
pub mod htxf;
pub mod numbers;
pub mod opcode;
pub mod text_encoding;
pub mod transaction;
pub mod types;
pub mod wire;

pub use consts::{Access, RequireFlags};
pub use error::{ProtoError, Result};
pub use field::{Built, Field, FieldId, FieldList, FieldValue, Received};
pub use handshake::{ClientHandshake, ServerHandshake, SUB_PROTOCOL_HOTLINE};
pub use htxf::{ForkHeader, HtxfHeader, HtxfType};
pub use opcode::{Opcode, TransactionType};
pub use text_encoding::Encoding;
pub use transaction::{TransCounter, Transaction, TransactionHeader};
pub use wire::{PStringTooLong, PacketWriter, VecWriter};

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
