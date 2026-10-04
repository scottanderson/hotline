use crate::error::{ProtoError, Result};
use crate::text_encoding::{self, Encoding};
use crate::wire::{SliceReader, VecWriter};

/// One entry of a [`myField_NewsCatListData15`] (323) news bundle/category
/// listing: a plain folder, or a leaf category holding a thread of
/// articles.
///
/// [`myField_NewsCatListData15`]: crate::FieldId::NewsCatListData15
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewsCategoryListEntry {
    /// A folder containing other bundles/categories.
    Bundle {
        /// Number of items directly inside this bundle.
        count: u16,
        /// Bundle name.
        name: Box<str>,
    },
    /// A leaf category holding a thread of articles.
    Category {
        /// Number of articles currently in this category's thread.
        count: u16,
        /// The category's persistent identifier.
        guid: [u8; 16],
        /// Incremented each time an article is added to this category.
        add_sn: u32,
        /// This category's deletion serial number.
        del_sn: u32,
        /// Category name.
        name: Box<str>,
    },
}

impl NewsCategoryListEntry {
    /// Encode to the packed wire representation.
    pub fn encode(&self, encoding: &'static Encoding) -> Result<Vec<u8>> {
        Ok(match self {
            Self::Bundle { count, name } => VecWriter::new()
                .u16(2)
                .u16(*count)
                .with_encoding(encoding)
                .pstring(name)?
                .finish(),
            Self::Category {
                count,
                guid,
                add_sn,
                del_sn,
                name,
            } => VecWriter::new()
                .u16(3)
                .u16(*count)
                .bytes(guid)
                .u32(*add_sn)
                .u32(*del_sn)
                .with_encoding(encoding)
                .pstring(name)?
                .finish(),
        })
    }

    /// Decode from a field's raw data.
    pub fn decode(buf: &[u8], encoding: &'static Encoding) -> Result<Self> {
        let mut r = SliceReader::new(buf);
        let kind = r.u16()?;
        let count = r.u16()?;
        match kind {
            2 => Ok(Self::Bundle {
                count,
                name: r.with_encoding(encoding).pstring()?.into(),
            }),
            3 => {
                let guid = r.array()?;
                let add_sn = r.u32()?;
                let del_sn = r.u32()?;
                Ok(Self::Category {
                    count,
                    guid,
                    add_sn,
                    del_sn,
                    name: r.with_encoding(encoding).pstring()?.into(),
                })
            }
            kind => Err(ProtoError::Malformed(format!(
                "unknown news item type: {kind}"
            ))),
        }
    }

    /// Decode a [`myField_NewsCatListData`] (320) entry: a type byte, then
    /// the name. Its counts are 0. `None` unless the type is a bundle (1)
    /// or a category (10).
    ///
    /// [`myField_NewsCatListData`]: crate::FieldId::NewsCatListData
    pub fn decode_old(buf: &[u8], encoding: &'static Encoding) -> Result<Option<Self>> {
        let (&kind, name) = buf
            .split_first()
            .filter(|(_, name)| !name.is_empty())
            .ok_or(ProtoError::UnexpectedEof {
                needed: 2,
                available: buf.len(),
            })?;
        let name = text_encoding::decode(encoding, &name[..name.len().min(255)]).into();
        Ok(match kind {
            1 => Some(Self::Bundle { count: 0, name }),
            10 => Some(Self::Category {
                count: 0,
                guid: [0; 16],
                add_sn: 0,
                del_sn: 0,
                name,
            }),
            _ => None,
        })
    }

    /// The bundle's or category's name.
    pub fn name(&self) -> &str {
        match self {
            Self::Bundle { name, .. } | Self::Category { name, .. } => name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_encoding;

    #[test]
    fn a_bundle_and_a_category_round_trip() {
        let encoding = text_encoding::DEFAULT;
        let entries = [
            NewsCategoryListEntry::Bundle {
                count: 2,
                name: "Folder".into(),
            },
            NewsCategoryListEntry::Category {
                count: 5,
                guid: [7; 16],
                add_sn: 9,
                del_sn: 1,
                name: "General".into(),
            },
        ];
        for entry in entries {
            let buf = entry.encode(encoding).unwrap();
            assert_eq!(
                NewsCategoryListEntry::decode(&buf, encoding).unwrap(),
                entry
            );
        }
    }

    #[test]
    fn decode_old_reads_a_bundle_and_a_category() {
        let encoding = text_encoding::DEFAULT;
        assert_eq!(
            NewsCategoryListEntry::decode_old(b"\x01Folder", encoding).unwrap(),
            Some(NewsCategoryListEntry::Bundle {
                count: 0,
                name: "Folder".into(),
            })
        );
        assert_eq!(
            NewsCategoryListEntry::decode_old(b"\x0aGeneral", encoding).unwrap(),
            Some(NewsCategoryListEntry::Category {
                count: 0,
                guid: [0; 16],
                add_sn: 0,
                del_sn: 0,
                name: "General".into(),
            })
        );
    }

    #[test]
    fn decode_old_skips_another_type() {
        let encoding = text_encoding::DEFAULT;
        assert_eq!(
            NewsCategoryListEntry::decode_old(b"\xffReadMe", encoding).unwrap(),
            None
        );
    }

    #[test]
    fn decode_old_rejects_an_entry_without_a_name() {
        let encoding = text_encoding::DEFAULT;
        assert!(NewsCategoryListEntry::decode_old(b"\x01", encoding).is_err());
        assert!(NewsCategoryListEntry::decode_old(b"", encoding).is_err());
    }

    #[test]
    fn decode_old_keeps_255_bytes_of_the_name() {
        let encoding = text_encoding::DEFAULT;
        let mut buf = vec![1];
        buf.extend([b'a'; 300]);
        let entry = NewsCategoryListEntry::decode_old(&buf, encoding)
            .unwrap()
            .unwrap();
        assert_eq!(entry.name().len(), 255);
    }
}
