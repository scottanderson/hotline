use crate::error::Result;
use crate::text_encoding::Encoding;
use crate::wire::VecWriter;

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
        name: String,
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
        name: String,
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
}
