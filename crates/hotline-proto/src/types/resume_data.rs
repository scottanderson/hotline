use crate::error::{ProtoError, Result};
use crate::wire::{ArrayReader, ArrayWriter, SliceReader};

/// Byte length of the fixed prefix, before the fork entries:
/// `format(4) | version(2) | reserved(34) | fork_count(2)`.
const PREFIX_LEN: usize = 42;

/// Byte length of one fork entry:
/// `fork_type(4) | data_size(4) | reserved(4) | reserved(4)`.
const ENTRY_LEN: usize = 16;

/// A `FileResumeData` blob, [`myField_FileResumeData`] (203): how many bytes
/// of each fork the receiver already has.
///
/// [`myField_FileResumeData`]: crate::FieldId::FileResumeData
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResumeData {
    /// Bytes of the `DATA` fork.
    pub data: u32,
    /// Bytes of the `MACR` fork.
    pub resource: u32,
}

impl ResumeData {
    /// Byte length of [`Self::encode`]'s output: the prefix and two entries.
    pub const LEN: usize = PREFIX_LEN + 2 * ENTRY_LEN;

    /// Decode the blob. The last entry of each fork wins, and a missing
    /// `MACR` entry gives 0. A missing `DATA` entry is an error.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut reader = SliceReader::new(buf);
        let prefix: [u8; PREFIX_LEN] = reader.array()?;
        let mut r = ArrayReader::new(prefix);
        let magic = r.take();
        let version = r.u16();
        let _reserved = r.take::<34>();
        let fork_count = r.u16();
        r.finish();
        if magic != *b"RFLT" {
            return Err(ProtoError::BadMagic {
                expected: *b"RFLT",
                got: magic,
            });
        }
        if version != 1 {
            return Err(ProtoError::Malformed(format!(
                "unsupported RFLT version: {version}"
            )));
        }

        let mut data = None;
        let mut resource = 0;
        for _ in 0..fork_count {
            let entry: [u8; ENTRY_LEN] = reader.array()?;
            let mut er = ArrayReader::new(entry);
            let fork_type = er.take();
            let data_size = er.u32();
            let _reserved = er.take::<8>();
            er.finish();
            match &fork_type {
                b"DATA" => data = Some(data_size),
                b"MACR" => resource = data_size,
                _ => {}
            }
        }
        let data = data.ok_or_else(|| {
            ProtoError::Malformed("RFLT resume data has no DATA fork entry".to_string())
        })?;
        Ok(Self { data, resource })
    }

    /// Encode the blob with a `DATA` entry and a `MACR` entry.
    pub fn encode(&self) -> [u8; Self::LEN] {
        ArrayWriter::new()
            .bytes(b"RFLT")
            .u16(1) // version
            .bytes(&[0; 34]) // reserved
            .u16(2) // fork count
            .bytes(b"DATA")
            .u32(self.data)
            .u32(0) // reserved
            .u32(0) // reserved
            .bytes(b"MACR")
            .u32(self.resource)
            .u32(0) // reserved
            .u32(0) // reserved
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::VecWriter;

    fn build(entries: &[([u8; 4], u32)]) -> Vec<u8> {
        let mut w = VecWriter::with_capacity(PREFIX_LEN + entries.len() * ENTRY_LEN);
        w = w
            .bytes(b"RFLT")
            .u16(1)
            .bytes(&[0; 34])
            .u16(entries.len() as u16);
        for (fork_type, data_size) in entries {
            w = w.bytes(fork_type).u32(*data_size).u32(0).u32(0);
        }
        w.finish()
    }

    #[test]
    fn decode_finds_the_data_fork_entry() {
        let buf = build(&[(*b"DATA", 1234)]);
        assert_eq!(
            ResumeData::decode(&buf).unwrap(),
            ResumeData {
                data: 1234,
                resource: 0
            }
        );
    }

    #[test]
    fn decode_reads_entries_in_any_order() {
        let buf = build(&[(*b"MACR", 99), (*b"INFO", 7), (*b"DATA", 555)]);
        assert_eq!(
            ResumeData::decode(&buf).unwrap(),
            ResumeData {
                data: 555,
                resource: 99
            }
        );
    }

    #[test]
    fn decode_takes_the_last_entry_of_a_fork() {
        let buf = build(&[(*b"DATA", 1), (*b"DATA", 2)]);
        assert_eq!(ResumeData::decode(&buf).unwrap().data, 2);
    }

    #[test]
    fn decode_rejects_bad_magic() {
        let mut buf = build(&[(*b"DATA", 1)]);
        buf[0] = b'X';
        assert!(matches!(
            ResumeData::decode(&buf),
            Err(ProtoError::BadMagic { .. })
        ));
    }

    #[test]
    fn decode_rejects_a_missing_data_fork_entry() {
        let buf = build(&[(*b"MACR", 99)]);
        assert!(matches!(
            ResumeData::decode(&buf),
            Err(ProtoError::Malformed(_))
        ));
    }

    #[test]
    fn decode_rejects_a_short_entry_list() {
        let mut buf = build(&[(*b"DATA", 1)]);
        buf.truncate(buf.len() - 1);
        assert!(matches!(
            ResumeData::decode(&buf),
            Err(ProtoError::UnexpectedEof { .. })
        ));
    }

    #[test]
    fn encode_round_trips_through_decode() {
        let rd = ResumeData {
            data: 4321,
            resource: 12,
        };
        assert_eq!(ResumeData::decode(&rd.encode()).unwrap(), rd);
    }
}
