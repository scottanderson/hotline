//! [`ForkHeader`]: one fork's header inside a flattened file object.

use crate::error::Result;
use crate::wire::{ArrayReader, ArrayWriter, SliceReader};

/// One fork's header, read between the `FILP` prefix and its data
/// (`INFO`, `DATA`, or `MACR`). Compression is always `0` (uncompressed)
/// in every flattened object this server has seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForkHeader {
    /// Fork tag, e.g. `b"DATA"`.
    pub fork_type: [u8; 4],
    /// Number of bytes of fork data that follow this header.
    pub data_size: u32,
}

impl ForkHeader {
    /// Wire length in bytes: `fork_type(4) | compression_type(4) |
    /// reserved(4) | data_size(4)`.
    pub const LEN: usize = 16;

    /// Encode one fork header, uncompressed.
    pub fn encode(&self) -> [u8; Self::LEN] {
        ArrayWriter::new()
            .bytes(&self.fork_type)
            .u32(0) // compression type
            .u32(0) // reserved
            .u32(self.data_size)
            .finish()
    }

    /// Decode one fork header.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let buf: [u8; Self::LEN] = SliceReader::new(buf).array()?;
        let mut r = ArrayReader::new(buf);
        let fork_type = r.take();
        let _compression_type = r.u32();
        let _reserved = r.u32();
        let data_size = r.u32();
        r.finish();
        Ok(Self {
            fork_type,
            data_size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::htxf::{FLAT_FILE_PREFIX_LEN, INFO_DATA_LEN, empty_info, flat_file_header};

    #[test]
    fn fork_header_round_trips_through_flat_file_header() {
        let header = flat_file_header(&empty_info(), 42);
        let info = ForkHeader::decode(&header[FLAT_FILE_PREFIX_LEN..]).unwrap();
        assert_eq!(&info.fork_type, b"INFO");
        assert_eq!(info.data_size, INFO_DATA_LEN as u32);

        let data_fork_offset = FLAT_FILE_PREFIX_LEN + ForkHeader::LEN + INFO_DATA_LEN;
        let data = ForkHeader::decode(&header[data_fork_offset..]).unwrap();
        assert_eq!(&data.fork_type, b"DATA");
        assert_eq!(data.data_size, 42);
    }
}
