//! Field tags: [`FieldId`] covers every field either direction sends.
//!
//! `HLProtocol.md` numbers fields with one flat `myField_*` list, no
//! client/server split, and a field-by-field audit against it found no
//! case where the same numeric tag means something structurally
//! different depending on direction, so one type covers both (a raw
//! [`Field`](crate::field::Field) doesn't know its own direction either;
//! see [`crate::transaction`]'s module docs). The one exception is
//! semantic, not structural: [`myField_Options`] (113) is a persisted-preference
//! bitmap in a client request but tags a bounce reply's kind (refusal
//! notice vs automatic response) in a server push; nothing in [`FieldId`]
//! itself encodes that, callers interpret the raw bits per transaction.
//!
//! Each variant is the exact tail of its `myField_*` constant, so
//! `myField_ServerName` is [`FieldId::ServerName`].
//!
//! `HLProtocol.md` does not list [`myField_NewsArtRecurseDel`] (337). Its
//! name comes from openline's `HotlineClientServerCommon.h`.
//!
//! [`myField_Options`]: crate::FieldId::Options
//! [`myField_NewsArtRecurseDel`]: crate::FieldId::NewsArtRecurseDel

use num_enum::{FromPrimitive, IntoPrimitive};

/// A field tag, as sent by either a client or a server.
#[repr(u16)]
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, FromPrimitive, IntoPrimitive)]
pub enum FieldId {
    /// Error message text, server-only.
    ///
    /// Original: `myField_ErrorText`.
    ErrorText = 100,
    /// Generic payload: chat text, an instant message body, a news post, or the server agreement, depending on the enclosing transaction.
    ///
    /// Original: `myField_Data`.
    Data = 101,
    /// A user's display name.
    ///
    /// Original: `myField_UserName`.
    UserName = 102,
    /// Socket/user ID.
    ///
    /// Original: `myField_UserID`.
    UserID = 103,
    /// Icon ID.
    ///
    /// Original: `myField_UserIconID`.
    UserIconID = 104,
    /// Account login (XOR-obfuscated on the wire).
    ///
    /// Original: `myField_UserLogin`.
    UserLogin = 105,
    /// Account password (XOR-obfuscated on the wire).
    ///
    /// Original: `myField_UserPassword`.
    UserPassword = 106,
    /// File transfer reference number, assigned by the server.
    ///
    /// Original: `myField_RefNum`.
    RefNum = 107,
    /// File transfer size in bytes.
    ///
    /// Original: `myField_TransferSize`.
    TransferSize = 108,
    /// Chat message style/formatting bitmap.
    ///
    /// Original: `myField_ChatOptions`.
    ChatOptions = 109,
    /// 64-bit access privilege bitmap. See [`crate::Access`].
    ///
    /// Original: `myField_UserAccess`.
    UserAccess = 110,
    /// A user's alias; unused in practice, always sent empty, client-only.
    ///
    /// Original: `myField_UserAlias`.
    UserAlias = 111,
    /// User status bitmap (away, admin, etc) / legacy color, server-only.
    ///
    /// Original: `myField_UserFlags`.
    UserFlags = 112,
    /// Contextual options bitmap: a persisted-preference bitmap in a
    /// client request, or a private message/chat-invite bounce's kind
    /// (refusal notice vs automatic response) in a server push. Also
    /// reused as a kick-to-ban flag.
    ///
    /// Original: `myField_Options`.
    Options = 113,
    /// Private chat room ID.
    ///
    /// Original: `myField_ChatID`.
    ChatID = 114,
    /// Chat room subject string.
    ///
    /// Original: `myField_ChatSubject`.
    ChatSubject = 115,
    /// Position in the download queue, server-only.
    ///
    /// Original: `myField_WaitingCount`.
    WaitingCount = 116,
    /// Raw banner image bytes, server-only.
    ///
    /// Original: `myField_ServerBanner`.
    ServerBanner = 151,
    /// Banner format tag (URL/JPEG/GIF/BMP/PICT), server-only.
    ///
    /// Original: `myField_ServerBannerType`.
    ServerBannerType = 152,
    /// Banner URL, when the type is URL, server-only.
    ///
    /// Original: `myField_ServerBannerUrl`.
    ServerBannerUrl = 153,
    /// Set when the server has no agreement to show, server-only.
    ///
    /// Original: `myField_NoServerAgreement`.
    NoServerAgreement = 154,
    /// Protocol version: the client's own version when client-sent, the server's version when server-sent.
    ///
    /// Original: `myField_Vers`.
    Vers = 160,
    /// Banner ID used to fetch the banner over HTTP, server-only.
    ///
    /// Original: `myField_CommunityBannerID`.
    CommunityBannerID = 161,
    /// Server's display name, server-only.
    ///
    /// Original: `myField_ServerName`.
    ServerName = 162,
    /// A packed file-listing entry, server-only.
    ///
    /// Original: `myField_FileNameWithInfo`.
    FileNameWithInfo = 200,
    /// A file or folder's name.
    ///
    /// Original: `myField_FileName`.
    FileName = 201,
    /// A directory path, client-only.
    ///
    /// Original: `myField_FilePath`.
    FilePath = 202,
    /// Resume-transfer descriptor.
    ///
    /// Original: `myField_FileResumeData`.
    FileResumeData = 203,
    /// File transfer options; also used for image preview requests, client-only.
    ///
    /// Original: `myField_FileXferOptions`.
    FileXferOptions = 204,
    /// Mac file type as a 4-character string, server-only.
    ///
    /// Original: `myField_FileTypeString`.
    FileTypeString = 205,
    /// Mac file creator as a 4-character string, server-only.
    ///
    /// Original: `myField_FileCreatorString`.
    FileCreatorString = 206,
    /// File size in bytes, server-only.
    ///
    /// Original: `myField_FileSize`.
    FileSize = 207,
    /// File creation date, server-only.
    ///
    /// Original: `myField_FileCreateDate`.
    FileCreateDate = 208,
    /// File modification date, server-only.
    ///
    /// Original: `myField_FileModifyDate`.
    FileModifyDate = 209,
    /// File/folder comment text.
    ///
    /// Original: `myField_FileComment`.
    FileComment = 210,
    /// Requested new name for a rename, client-only.
    ///
    /// Original: `myField_FileNewName`.
    FileNewName = 211,
    /// Requested destination path for a move/alias, client-only.
    ///
    /// Original: `myField_FileNewPath`.
    FileNewPath = 212,
    /// 4-character Mac type code, packed, server-only.
    ///
    /// Original: `myField_FileType`.
    FileType = 213,
    /// Message being quoted/replied to, on a private message (bounce).
    ///
    /// Original: `myField_QuotingMsg`.
    QuotingMsg = 214,
    /// Custom automatic-response text, sent when [`myField_Options`] (113)
    /// enables it, client-only.
    ///
    /// Original: `myField_AutomaticResponse`.
    ///
    /// [`myField_Options`]: crate::FieldId::Options
    AutomaticResponse = 215,
    /// Number of items in a folder.
    ///
    /// Original: `myField_FldrItemCount`.
    FldrItemCount = 220,
    /// A packed user-list entry, server-only.
    ///
    /// Original: `myField_UserNameWithInfo`.
    UserNameWithInfo = 300,
    /// Packed news category/folder listing entry (pre-1.5).
    ///
    /// Original: `myField_NewsCatListData`.
    NewsCatListData = 320,
    /// Packed news article listing entry.
    ///
    /// Original: `myField_NewsArtListData`.
    NewsArtListData = 321,
    /// A news category's name, client-only.
    ///
    /// Original: `myField_NewsCatName`.
    NewsCatName = 322,
    /// Packed news category/folder listing entry (1.5+), server-only.
    ///
    /// Original: `myField_NewsCatListData15`.
    NewsCatListData15 = 323,
    /// A news folder/category path.
    ///
    /// Original: `myField_NewsPath`.
    NewsPath = 325,
    /// News article/thread ID.
    ///
    /// Original: `myField_NewsArtID`.
    NewsArtID = 326,
    /// News article MIME type, e.g. "text/plain".
    ///
    /// Original: `myField_NewsArtDataFlav`.
    NewsArtDataFlav = 327,
    /// News article subject/title.
    ///
    /// Original: `myField_NewsArtTitle`.
    NewsArtTitle = 328,
    /// News article author name.
    ///
    /// Original: `myField_NewsArtPoster`.
    NewsArtPoster = 329,
    /// News article post date.
    ///
    /// Original: `myField_NewsArtDate`.
    NewsArtDate = 330,
    /// Previous article ID in thread order.
    ///
    /// Original: `myField_NewsArtPrevArt`.
    NewsArtPrevArt = 331,
    /// Next article ID in thread order.
    ///
    /// Original: `myField_NewsArtNextArt`.
    NewsArtNextArt = 332,
    /// News article body text.
    ///
    /// Original: `myField_NewsArtData`.
    NewsArtData = 333,
    /// News article flags, set on post, client-only.
    ///
    /// Original: `myField_NewsArtFlags`.
    NewsArtFlags = 334,
    /// Parent article ID.
    ///
    /// Original: `myField_NewsArtParentArt`.
    NewsArtParentArt = 335,
    /// First child article ID.
    ///
    /// Original: `myField_NewsArt1stChildArt`.
    NewsArt1stChildArt = 336,
    /// Whether deleting this article recursively deletes replies, client-only.
    ///
    /// Original: `myField_NewsArtRecurseDel`.
    NewsArtRecurseDel = 337,
    /// Not recognized by this build. Carries the raw wire value so
    /// callers can still log/forward it.
    #[num_enum(catch_all)]
    Unknown(u16),
}

/// Declares `FieldId::original_name`, the `myField_` constant each listed
/// variant is named for.
macro_rules! field_names {
    ($($variant:ident),* $(,)?) => {
        impl FieldId {
            /// The original constant name.
            pub const fn original_name(self) -> &'static str {
                match self {
                    $(Self::$variant => concat!("myField_", stringify!($variant)),)*
                    Self::Unknown(_) => "(unrecognized field)",
                }
            }

            #[cfg(test)]
            const ALL: &[Self] = &[$(Self::$variant),*];
        }
    };
}

field_names!(
    ErrorText,
    Data,
    UserName,
    UserID,
    UserIconID,
    UserLogin,
    UserPassword,
    RefNum,
    TransferSize,
    ChatOptions,
    UserAccess,
    UserAlias,
    UserFlags,
    Options,
    ChatID,
    ChatSubject,
    WaitingCount,
    ServerBanner,
    ServerBannerType,
    ServerBannerUrl,
    NoServerAgreement,
    Vers,
    CommunityBannerID,
    ServerName,
    FileNameWithInfo,
    FileName,
    FilePath,
    FileResumeData,
    FileXferOptions,
    FileTypeString,
    FileCreatorString,
    FileSize,
    FileCreateDate,
    FileModifyDate,
    FileComment,
    FileNewName,
    FileNewPath,
    FileType,
    QuotingMsg,
    AutomaticResponse,
    FldrItemCount,
    UserNameWithInfo,
    NewsCatListData,
    NewsArtListData,
    NewsCatName,
    NewsCatListData15,
    NewsPath,
    NewsArtID,
    NewsArtDataFlav,
    NewsArtTitle,
    NewsArtPoster,
    NewsArtDate,
    NewsArtPrevArt,
    NewsArtNextArt,
    NewsArtData,
    NewsArtFlags,
    NewsArtParentArt,
    NewsArt1stChildArt,
    NewsArtRecurseDel,
);

impl std::fmt::Debug for FieldId {
    /// Prints the wire value and the original constant name, e.g.
    /// `102 myField_UserName`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", u16::from(*self), self.original_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hl_protocol_md;

    #[test]
    fn every_variant_is_named_for_its_hlprotocol_md_constant() {
        let documented = hl_protocol_md::constants("myField_");
        for &field_id in FieldId::ALL {
            let value = u32::from(u16::from(field_id));
            let names: Vec<_> = documented.iter().filter(|(v, _)| *v == value).collect();
            assert!(!names.is_empty(), "field {value} has no constant");
            for (_, name) in names {
                assert_eq!(field_id.original_name(), name, "field {value}");
            }
        }
    }

    #[test]
    fn from_u16_round_trips() {
        assert_eq!(FieldId::from(102), FieldId::UserName);
        assert_eq!(u16::from(FieldId::UserName), 102);
        assert_eq!(FieldId::from(65535), FieldId::Unknown(65535));
    }

    #[test]
    fn name_handles_unknown_values() {
        assert_eq!(FieldId::from(65535).original_name(), "(unrecognized field)");
        assert_eq!(FieldId::from(102).original_name(), "myField_UserName");
    }

    #[test]
    fn file_type_213_does_not_collide_with_file_type_string_205() {
        assert_eq!(FieldId::from(213), FieldId::FileType);
        assert_eq!(FieldId::from(205), FieldId::FileTypeString);
    }

    #[test]
    fn debug_reconstructs_every_fields_original_name() {
        use FieldId::*;
        let cases = [
            ("100 myField_ErrorText", ErrorText),
            ("101 myField_Data", Data),
            ("102 myField_UserName", UserName),
            ("110 myField_UserAccess", UserAccess),
            ("113 myField_Options", Options),
            ("151 myField_ServerBanner", ServerBanner),
            ("162 myField_ServerName", ServerName),
            ("200 myField_FileNameWithInfo", FileNameWithInfo),
            ("205 myField_FileTypeString", FileTypeString),
            ("213 myField_FileType", FileType),
            ("300 myField_UserNameWithInfo", UserNameWithInfo),
            ("326 myField_NewsArtID", NewsArtID),
            ("337 myField_NewsArtRecurseDel", NewsArtRecurseDel),
            ("57005 (unrecognized field)", Unknown(0xDEAD)),
        ];
        for (expected, field_id) in cases {
            assert_eq!(format!("{field_id:?}"), expected);
        }
    }
}
