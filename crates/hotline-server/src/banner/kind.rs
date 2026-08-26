/// A configured banner's shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BannerKind {
    /// Show a URL to the client directly; nothing to fetch over `HTXF`.
    Url(String),
    /// A fetchable image; `format` is the 4-byte type tag sent as
    /// `myField_ServerBannerType`, e.g. `*b"JPEG"`, `*b"GIFf"`, `*b"PICT"`,
    /// or `*b"BMP "`.
    Image {
        /// The 4-byte type tag, e.g. `*b"JPEG"`.
        format: [u8; 4],
    },
}

impl BannerKind {
    /// The 4-byte tag sent as `myField_ServerBannerType`.
    pub const fn type_tag(&self) -> [u8; 4] {
        match self {
            Self::Url(_) => *b"URL ",
            Self::Image { format } => *format,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_banner_type_tag_is_its_format() {
        let kind = BannerKind::Image { format: *b"JPEG" };
        assert_eq!(&kind.type_tag(), b"JPEG");
    }
}
