//! Wire-level implementation of the Hotline (`TRTP`) protocol.
//!
//! This crate is intentionally I/O-free: it only encodes/decodes byte
//! buffers into protocol structures (handshake, transaction header,
//! fields, and the composite types the field table refers to). The async
//! socket handling lives in `hotline-client` / `hotline-server`.

pub mod consts;
pub mod error;
pub mod field;
pub mod field_id;
pub mod handshake;
pub mod htxf;
pub mod numbers;
pub mod opcode;
pub mod transaction;
pub mod types;
pub mod wire;

pub use consts::{Access, RequireFlags};
pub use error::{ProtoError, Result};
pub use field::{Field, FieldValue};
pub use field_id::{ClientFieldId, ServerFieldId};
pub use handshake::{ClientHandshake, ServerHandshake, SUB_PROTOCOL_HOTLINE};
pub use htxf::{ForkHeader, HtxfHeader, HtxfType};
pub use opcode::{ClientOpcode, ServerOpcode, TransactionType};
pub use transaction::{Transaction, TransactionHeader};
pub use wire::{PStringTooLong, PacketWriter, VecWriter};
