//! Field tags: [`FieldId`] covers every field either direction sends.
//!
//! `HLProtocol.md` numbers fields with one flat `myField_*` list, no
//! client/server split, and a field-by-field audit against it found no
//! case where the same numeric tag means something structurally
//! different depending on direction, so one type covers both (a raw
//! [`Field`](crate::field::Field) doesn't know its own direction either;
//! see [`crate::transaction`]'s module docs). The one exception is
//! semantic, not structural: `Options` (113) is a persisted-preference
//! bitmap in a client request but tags a bounce reply's kind (refusal
//! notice vs automatic response) in a server push; nothing in [`FieldId`]
//! itself encodes that, callers interpret the raw bits per transaction.
//!
//! Variant names track the tail of the original wire constant (e.g.
//! `myField_ServerName` -> `Servername`) rather than being split into
//! idiomatic multi-word Rust names, so a variant is always one grep away
//! from its source.

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
    Text = 101,
    /// A user's display name.
    ///
    /// Original: `myField_UserName`.
    Name = 102,
    /// Socket/user ID.
    ///
    /// Original: `myField_UserID`.
    UserId = 103,
    /// Icon ID.
    ///
    /// Original: `myField_UserIconID`.
    Icon = 104,
    /// Account login (XOR-obfuscated on the wire).
    ///
    /// Original: `myField_UserLogin`.
    Login = 105,
    /// Account password (XOR-obfuscated on the wire).
    ///
    /// Original: `myField_UserPassword`.
    Password = 106,
    /// File transfer reference number, assigned by the server.
    ///
    /// Original: `myField_RefNum`.
    TransferId = 107,
    /// File transfer size in bytes.
    ///
    /// Original: `myField_TransferSize`.
    TransferSize = 108,
    /// Chat message style/formatting bitmap.
    ///
    /// Original: `myField_ChatOptions`.
    Style = 109,
    /// 64-bit access privilege bitmap. See [`crate::Access`].
    ///
    /// Original: `myField_UserAccess`.
    Access = 110,
    /// A user's alias; unused in practice, always sent empty, client-only.
    ///
    /// Original: `myField_UserAlias`.
    UserAlias = 111,
    /// User status bitmap (away, admin, etc) / legacy color, server-only.
    ///
    /// Original: `myField_UserFlags`.
    Color = 112,
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
    ChatId = 114,
    /// Chat room subject string.
    ///
    /// Original: `myField_ChatSubject`.
    ChatSubject = 115,
    /// Position in the download queue, server-only.
    ///
    /// Original: `myField_WaitingCount`.
    QueuePosition = 116,
    /// Raw banner image bytes, server-only.
    ///
    /// Original: `myField_ServerBanner`.
    Banner = 151,
    /// Banner format tag (URL/JPEG/GIF/BMP/PICT), server-only.
    ///
    /// Original: `myField_ServerBannerType`.
    BannerType = 152,
    /// Banner URL, when the type is URL, server-only.
    ///
    /// Original: `myField_ServerBannerUrl`.
    BannerUrl = 153,
    /// Set when the server has no agreement to show, server-only.
    ///
    /// Original: `myField_NoServerAgreement`.
    Noagreement = 154,
    /// Protocol version: the client's own version when client-sent, the server's version when server-sent.
    ///
    /// Original: `myField_Vers`.
    Version = 160,
    /// Banner ID used to fetch the banner over HTTP, server-only.
    ///
    /// Original: `myField_CommunityBannerID`.
    Bannerid = 161,
    /// Server's display name, server-only.
    ///
    /// Original: `myField_ServerName`.
    Servername = 162,
    /// A packed file-listing entry, server-only.
    ///
    /// Original: `myField_FileNameWithInfo`.
    FileList = 200,
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
    ResumeFileTransfer = 203,
    /// File transfer options; also used for image preview requests, client-only.
    ///
    /// Original: `myField_FileXferOptions`.
    FileXferOptions = 204,
    /// Mac file type as a 4-character string, server-only.
    ///
    /// Original: `myField_FileTypeString`.
    FileType = 205,
    /// Mac file creator as a 4-character string, server-only.
    ///
    /// Original: `myField_FileCreatorString`.
    FileCreator = 206,
    /// File size in bytes, server-only.
    ///
    /// Original: `myField_FileSize`.
    FileSize = 207,
    /// File creation date, server-only.
    ///
    /// Original: `myField_FileCreateDate`.
    FileDateCreate = 208,
    /// File modification date, server-only.
    ///
    /// Original: `myField_FileModifyDate`.
    FileDateModify = 209,
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
    FileIcon = 213,
    /// Message being quoted/replied to, on a private message (bounce).
    ///
    /// Original: `myField_QuotingMsg`.
    QuotingMsg = 214,
    /// Custom automatic-response text, sent when `Options` (113) enables it, client-only.
    ///
    /// Original: `myField_AutomaticResponse`.
    AutomaticResponse = 215,
    /// Number of items in a folder.
    ///
    /// Original: `myField_FldrItemCount`.
    FolderItemCount = 220,
    /// A packed user-list entry, server-only.
    ///
    /// Original: `myField_UserNameWithInfo`.
    UserList = 300,
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
    NewsDirlistExtended = 323,
    /// A news folder/category path.
    ///
    /// Original: `myField_NewsPath`.
    NewsPath = 325,
    /// News article/thread ID.
    ///
    /// Original: `myField_NewsArtID`.
    NewsArtId = 326,
    /// News article MIME type, e.g. "text/plain".
    ///
    /// Original: `myField_NewsArtDataFlav`.
    NewsArtDataFlavor = 327,
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

impl FieldId {
    /// The original constant name.
    pub const fn original_name(self) -> &'static str {
        match self {
            Self::ErrorText => "myField_ErrorText",
            Self::Text => "myField_Data",
            Self::Name => "myField_UserName",
            Self::UserId => "myField_UserID",
            Self::Icon => "myField_UserIconID",
            Self::Login => "myField_UserLogin",
            Self::Password => "myField_UserPassword",
            Self::TransferId => "myField_RefNum",
            Self::TransferSize => "myField_TransferSize",
            Self::Style => "myField_ChatOptions",
            Self::Access => "myField_UserAccess",
            Self::UserAlias => "myField_UserAlias",
            Self::Color => "myField_UserFlags",
            Self::Options => "myField_Options",
            Self::ChatId => "myField_ChatID",
            Self::ChatSubject => "myField_ChatSubject",
            Self::QueuePosition => "myField_WaitingCount",
            Self::Banner => "myField_ServerBanner",
            Self::BannerType => "myField_ServerBannerType",
            Self::BannerUrl => "myField_ServerBannerUrl",
            Self::Noagreement => "myField_NoServerAgreement",
            Self::Version => "myField_Vers",
            Self::Bannerid => "myField_CommunityBannerID",
            Self::Servername => "myField_ServerName",
            Self::FileList => "myField_FileNameWithInfo",
            Self::FileName => "myField_FileName",
            Self::FilePath => "myField_FilePath",
            Self::ResumeFileTransfer => "myField_FileResumeData",
            Self::FileXferOptions => "myField_FileXferOptions",
            Self::FileType => "myField_FileTypeString",
            Self::FileCreator => "myField_FileCreatorString",
            Self::FileSize => "myField_FileSize",
            Self::FileDateCreate => "myField_FileCreateDate",
            Self::FileDateModify => "myField_FileModifyDate",
            Self::FileComment => "myField_FileComment",
            Self::FileNewName => "myField_FileNewName",
            Self::FileNewPath => "myField_FileNewPath",
            Self::FileIcon => "myField_FileType",
            Self::QuotingMsg => "myField_QuotingMsg",
            Self::AutomaticResponse => "myField_AutomaticResponse",
            Self::FolderItemCount => "myField_FldrItemCount",
            Self::UserList => "myField_UserNameWithInfo",
            Self::NewsCatListData => "myField_NewsCatListData",
            Self::NewsArtListData => "myField_NewsArtListData",
            Self::NewsCatName => "myField_NewsCatName",
            Self::NewsDirlistExtended => "myField_NewsCatListData15",
            Self::NewsPath => "myField_NewsPath",
            Self::NewsArtId => "myField_NewsArtID",
            Self::NewsArtDataFlavor => "myField_NewsArtDataFlav",
            Self::NewsArtTitle => "myField_NewsArtTitle",
            Self::NewsArtPoster => "myField_NewsArtPoster",
            Self::NewsArtDate => "myField_NewsArtDate",
            Self::NewsArtPrevArt => "myField_NewsArtPrevArt",
            Self::NewsArtNextArt => "myField_NewsArtNextArt",
            Self::NewsArtData => "myField_NewsArtData",
            Self::NewsArtFlags => "myField_NewsArtFlags",
            Self::NewsArtParentArt => "myField_NewsArtParentArt",
            Self::NewsArt1stChildArt => "myField_NewsArt1stChildArt",
            Self::NewsArtRecurseDel => "myField_NewsArtRecurseDel",
            Self::Unknown(_) => "(unrecognized field)",
        }
    }
}

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

    #[test]
    fn from_u16_round_trips() {
        assert_eq!(FieldId::from(102), FieldId::Name);
        assert_eq!(u16::from(FieldId::Name), 102);
        assert_eq!(FieldId::from(65535), FieldId::Unknown(65535));
    }

    #[test]
    fn name_handles_unknown_values() {
        assert_eq!(FieldId::from(65535).original_name(), "(unrecognized field)");
        assert_eq!(FieldId::from(102).original_name(), "myField_UserName");
    }

    #[test]
    fn file_icon_213_does_not_collide_with_file_type_205() {
        assert_eq!(FieldId::from(213), FieldId::FileIcon);
        assert_eq!(FieldId::from(205), FieldId::FileType);
    }

    #[test]
    fn debug_reconstructs_every_fields_original_name() {
        use FieldId::*;
        let cases = [
            ("100 myField_ErrorText", ErrorText),
            ("101 myField_Data", Text),
            ("102 myField_UserName", Name),
            ("110 myField_UserAccess", Access),
            ("113 myField_Options", Options),
            ("151 myField_ServerBanner", Banner),
            ("162 myField_ServerName", Servername),
            ("200 myField_FileNameWithInfo", FileList),
            ("205 myField_FileTypeString", FileType),
            ("213 myField_FileType", FileIcon),
            ("300 myField_UserNameWithInfo", UserList),
            ("326 myField_NewsArtID", NewsArtId),
            ("337 myField_NewsArtRecurseDel", NewsArtRecurseDel),
            ("57005 (unrecognized field)", Unknown(0xDEAD)),
        ];
        for (expected, field_id) in cases {
            assert_eq!(format!("{field_id:?}"), expected);
        }
    }
}
