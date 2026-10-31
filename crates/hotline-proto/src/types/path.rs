//! [`HlPath`]: a file/news path field.

use crate::error::{ProtoError, Result};
use crate::text_encoding::{self, Encoding};
use crate::wire::{SliceReader, VecWriter};

/// A file/news path: a sequence of directory-level names from root to leaf.
/// Wire shape per the older guide: `levels(2)` then, per level,
/// `reserved(2)=0 | name_len(1) | name`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HlPath {
    /// Directory components, root-to-leaf (does not include the file name
    /// itself for file operations, that's a separate [`myField_FileName`] (201) field).
    ///
    /// [`myField_FileName`]: crate::FieldId::FileName
    pub components: Vec<String>,
}

impl HlPath {
    /// Decoder-side safety bound on component count, well beyond any real
    /// client's directory depth, to avoid an oversized allocation from a
    /// malformed `levels` count.
    const MAX_COMPONENTS: usize = 256;

    /// Build a path from components.
    pub fn new(components: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            components: components.into_iter().map(Into::into).collect(),
        }
    }

    /// Encode to bytes suitable for a [`myField_FilePath`] (202) or [`myField_NewsPath`] (325)
    /// field.
    ///
    /// Fails if any component name is longer than 255 bytes, since each
    /// is written with a 1-byte length prefix.
    ///
    /// [`myField_FilePath`]: crate::FieldId::FilePath
    /// [`myField_NewsPath`]: crate::FieldId::NewsPath
    pub fn encode(&self, encoding: &'static Encoding) -> Result<Vec<u8>> {
        let mut w = VecWriter::new()
            .u16(self.components.len() as u16)
            .with_encoding(encoding);
        for name in &self.components {
            w = w.u16(0).pstring(name)?;
        }
        Ok(w.finish())
    }

    /// Decode a path from a field's raw data.
    pub fn decode(buf: &[u8], encoding: &'static Encoding) -> Result<Self> {
        let mut r = SliceReader::new(buf);
        let levels = usize::from(r.u16()?);
        if levels > Self::MAX_COMPONENTS {
            return Err(ProtoError::Malformed(format!(
                "path has {levels} components, more than the max of {}",
                Self::MAX_COMPONENTS
            )));
        }
        let mut components = Vec::with_capacity(levels);
        for _ in 0..levels {
            // 2 reserved bytes, then a 1-byte length prefix (pstring-style).
            let [_, _, name_len] = r.array()?;
            components.push(text_encoding::decode(encoding, r.bytes(name_len.into())?));
        }
        Ok(Self { components })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let path = HlPath::new(["Games", "Emulators"]);
        let bytes = path.encode(encoding).unwrap();
        assert_eq!(HlPath::decode(&bytes, encoding).unwrap(), path);
    }

    #[test]
    fn empty_path_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let path = HlPath::default();
        assert_eq!(
            HlPath::decode(&path.encode(encoding).unwrap(), encoding).unwrap(),
            path
        );
    }

    #[test]
    fn path_encode_rejects_an_oversized_component() {
        let encoding = text_encoding::DEFAULT;
        let path = HlPath::new(["x".repeat(256)]);
        assert!(matches!(
            path.encode(encoding),
            Err(ProtoError::StringTooLong(_))
        ));
    }

    #[test]
    fn path_decode_rejects_too_many_components() {
        let encoding = text_encoding::DEFAULT;
        let mut buf = ((HlPath::MAX_COMPONENTS + 1) as u16).to_be_bytes().to_vec();
        buf.extend(std::iter::repeat_n(0u8, HlPath::MAX_COMPONENTS + 1));
        assert!(matches!(
            HlPath::decode(&buf, encoding),
            Err(ProtoError::Malformed(_))
        ));
    }
}
