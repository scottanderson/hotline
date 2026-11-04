//! Wire text encoding, pluggable per [`Encoding`].
//!
//! The classic client uses Mac OS Roman; [`DEFAULT`] matches that. Any
//! other [WHATWG](https://encoding.spec.whatwg.org/) label works via
//! [`Encoding::for_label`].

use std::sync::Arc;

use arc_swap::ArcSwap;

pub use encoding_rs::Encoding;

/// A connection's active text encoding, shared and swappable across clones.
#[derive(Clone, Debug)]
pub struct EncodingHandle(Arc<ArcSwap<&'static Encoding>>);

/// Default wire encoding: Mac OS Roman.
pub const DEFAULT: &Encoding = encoding_rs::MACINTOSH;

impl Default for EncodingHandle {
    fn default() -> Self {
        Self::from(DEFAULT)
    }
}

impl EncodingHandle {
    /// This handle's current encoding.
    pub fn get(&self) -> &'static Encoding {
        **self.0.load()
    }

    /// Switch every clone of this handle to `encoding` from the next [`EncodingHandle::get`] on.
    pub fn set(&self, encoding: &'static Encoding) {
        self.0.store(Arc::new(encoding));
    }
}

impl From<&'static Encoding> for EncodingHandle {
    fn from(value: &'static Encoding) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(value)))
    }
}

/// The encoding with the [WHATWG](https://encoding.spec.whatwg.org/)
/// label `label`, such as `macintosh` or `utf-8`. `Err` for an unknown
/// label, or for an encoding that encodes to another one, such as UTF-16.
pub fn from_label(label: &str) -> Result<&'static Encoding, String> {
    match Encoding::for_label(label.trim().as_bytes()) {
        Some(encoding) if encoding.output_encoding() == encoding => Ok(encoding),
        Some(encoding) => Err(format!("{} cannot encode text", encoding.name())),
        None => Err(format!("unknown encoding {label:?}")),
    }
}

/// Byte substituted for an unmappable codepoint.
const FALLBACK_BYTE: u8 = b'?';

/// Decode `bytes` as `encoding`.
pub fn decode(encoding: &'static Encoding, bytes: &[u8]) -> String {
    encoding.decode_without_bom_handling(bytes).0.into_owned()
}

/// The longest start of `s` that encodes to at most `max` bytes in
/// `encoding`, cut at a character boundary.
pub fn cut<'a>(encoding: &'static Encoding, s: &'a str, max: usize) -> &'a str {
    if encode(encoding, s).len() <= max {
        return s;
    }
    let mut len = 0_usize;
    let end = s
        .char_indices()
        .find(|&(_, c)| {
            len = len.saturating_add(encode(encoding, c.encode_utf8(&mut [0; 4])).len());
            len > max
        })
        .map_or(s.len(), |(i, _)| i);
    s.get(..end).unwrap_or(s)
}

/// Encode `s` as `encoding`. An unmappable codepoint becomes a single
/// `FALLBACK_BYTE`, not `encoding_rs`'s own multi-byte numeric
/// reference (`"&#127881;"`).
pub fn encode(encoding: &'static Encoding, s: &str) -> Vec<u8> {
    let mut encoder = encoding.new_encoder();
    let mut out = Vec::new();
    let mut remaining = s;
    loop {
        // Reserve worst case up front so `OutputFull` only recurs if `remaining` grew.
        out.reserve(
            encoder
                .max_buffer_length_from_utf8_without_replacement(remaining.len())
                .unwrap_or(remaining.len()),
        );
        let (result, read) =
            encoder.encode_from_utf8_to_vec_without_replacement(remaining, &mut out, true);
        remaining = remaining.get(read..).unwrap_or_default();
        match result {
            encoding_rs::EncoderResult::InputEmpty => break,
            encoding_rs::EncoderResult::OutputFull => continue,
            encoding_rs::EncoderResult::Unmappable(_) => out.push(FALLBACK_BYTE),
        }
    }
    out
}

/// Encode `s` as [`encode`] does, cut at a whole character to at most `max` bytes.
pub fn encode_truncated(encoding: &'static Encoding, s: &str, max: usize) -> Vec<u8> {
    let mut encoder = encoding.new_encoder();
    let mut out = vec![0; max];
    let mut written = 0;
    let mut remaining = s;
    loop {
        let rest = out.get_mut(written..).unwrap_or_default();
        let (result, read, wrote) =
            encoder.encode_from_utf8_without_replacement(remaining, rest, true);
        remaining = remaining.get(read..).unwrap_or_default();
        written = written.saturating_add(wrote);
        match (result, out.get_mut(written)) {
            (encoding_rs::EncoderResult::Unmappable(_), Some(byte)) => {
                *byte = FALLBACK_BYTE;
                written = written.saturating_add(1);
            }
            _ => break,
        }
    }
    out.truncate(written);
    out
}

/// `s` with each CRLF or LF as `\r`, the Hotline line separator.
pub fn to_wire_lines(s: &str) -> String {
    s.replace("\r\n", "\r").replace('\n', "\r")
}

/// `s` with each `\r` as `\n`.
pub fn from_wire_lines(s: &str) -> String {
    s.replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_wire_lines_ends_each_line_with_cr() {
        assert_eq!(to_wire_lines("a\r\nb\nc\rd"), "a\rb\rc\rd");
    }

    #[test]
    fn from_wire_lines_ends_each_line_with_lf() {
        assert_eq!(from_wire_lines("a\rb"), "a\nb");
    }

    #[test]
    fn cut_keeps_whole_characters_within_the_encoded_limit() {
        assert_eq!(cut(DEFAULT, "caf\u{e9}s", 4), "caf\u{e9}");
        assert_eq!(cut(encoding_rs::UTF_8, "caf\u{e9}s", 4), "caf");
        assert_eq!(cut(DEFAULT, "short", 31), "short");
    }

    #[test]
    fn from_label_reads_a_known_label() {
        assert_eq!(from_label("macintosh"), Ok(DEFAULT));
        assert_eq!(from_label("UTF-8"), Ok(encoding_rs::UTF_8));
        assert_eq!(from_label("shift_jis"), Ok(encoding_rs::SHIFT_JIS));
    }

    #[test]
    fn from_label_rejects_an_unknown_label_and_utf16() {
        assert!(from_label("klingon").is_err());
        assert!(from_label("utf-16le").is_err());
        assert!(from_label("replacement").is_err());
    }

    #[test]
    fn default_is_mac_os_roman() {
        assert_eq!(DEFAULT.name(), "macintosh");
    }

    #[test]
    fn for_label_accepts_macintosh_case_insensitively() {
        assert_eq!(Encoding::for_label(b"MACINTOSH"), Some(DEFAULT));
        assert_eq!(Encoding::for_label(b"macintosh"), Some(DEFAULT));
    }

    #[test]
    fn for_label_accepts_the_other_three_requested_labels() {
        assert_eq!(Encoding::for_label(b"utf-8").unwrap().name(), "UTF-8");
        assert_eq!(
            Encoding::for_label(b"shift-jis").unwrap().name(),
            "Shift_JIS"
        );
        // WHATWG maps both labels to windows-1252, not strict ISO 8859-1.
        assert_eq!(
            Encoding::for_label(b"iso-8859-1").unwrap().name(),
            "windows-1252"
        );
        assert_eq!(
            Encoding::for_label(b"latin1").unwrap().name(),
            "windows-1252"
        );
    }

    #[test]
    fn for_label_rejects_an_unrecognized_label() {
        assert_eq!(Encoding::for_label(b"bogus-encoding"), None);
    }

    #[test]
    fn mac_os_roman_round_trips_ascii_and_high_bytes() {
        assert_eq!(decode(DEFAULT, b"RustHL 123!"), "RustHL 123!");
        assert_eq!(encode(DEFAULT, "RustHL 123!"), b"RustHL 123!");
        // e-acute (0x8E) has a Mac OS Roman encoding, so it round-trips exactly.
        let bytes = encode(DEFAULT, "h\u{e9}llo");
        assert_eq!(bytes, vec![b'h', 0x8E, b'l', b'l', b'o']);
        assert_eq!(decode(DEFAULT, &bytes), "h\u{e9}llo");
    }

    #[test]
    fn mac_os_roman_falls_back_to_question_mark_for_an_unmappable_codepoint() {
        assert_eq!(encode(DEFAULT, "hi \u{1f389}"), b"hi ?");
    }

    #[test]
    fn shift_jis_round_trips_japanese_text() {
        let sjis = Encoding::for_label(b"shift-jis").unwrap();
        let bytes = encode(sjis, "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}"); // "こんにちは"
        assert_eq!(
            decode(sjis, &bytes),
            "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}"
        );
    }

    #[test]
    fn shift_jis_falls_back_to_question_mark_for_an_unmappable_codepoint() {
        let sjis = Encoding::for_label(b"shift-jis").unwrap();
        assert_eq!(encode(sjis, "hi \u{1f389}"), b"hi ?");
    }

    #[test]
    fn latin1_round_trips_its_accented_range() {
        let latin1 = Encoding::for_label(b"latin1").unwrap();
        let bytes = encode(latin1, "caf\u{e9}");
        assert_eq!(decode(latin1, &bytes), "caf\u{e9}");
    }

    #[test]
    fn an_escape_based_encoding_round_trips_without_hanging() {
        // ISO-2022-JP's escapes can make output longer than the input byte count.
        let jp = Encoding::for_label(b"iso-2022-jp").unwrap();
        let s = "hi \u{3053}\u{3093}\u{306b}\u{3061}\u{306f} bye";
        let bytes = encode(jp, s);
        assert_eq!(decode(jp, &bytes), s);
    }

    #[test]
    fn utf8_round_trips_anything_including_emoji() {
        let utf8 = Encoding::for_label(b"utf-8").unwrap();
        let s = "hi \u{1f389}";
        assert_eq!(encode(utf8, s), s.as_bytes());
        assert_eq!(decode(utf8, s.as_bytes()), s);
    }

    #[test]
    fn encode_truncated_keeps_text_that_fits() {
        assert_eq!(encode_truncated(DEFAULT, "caf\u{e9}", 10), b"caf\x8e");
    }

    #[test]
    fn encode_truncated_cuts_at_a_whole_utf8_character() {
        let utf8 = Encoding::for_label(b"utf-8").unwrap();
        assert_eq!(encode_truncated(utf8, "a\u{e9}", 2), b"a");
        assert_eq!(encode_truncated(utf8, "a\u{e9}", 3), "a\u{e9}".as_bytes());
    }

    #[test]
    fn encode_truncated_cuts_at_a_whole_shift_jis_character() {
        let sjis = Encoding::for_label(b"shift-jis").unwrap();
        let bytes = encode_truncated(sjis, "\u{3042}\u{3044}", 3);
        assert_eq!(decode(sjis, &bytes), "\u{3042}");
    }

    #[test]
    fn encode_truncated_falls_back_for_an_unmappable_codepoint() {
        assert_eq!(encode_truncated(DEFAULT, "a\u{1f389}b", 2), b"a?");
    }
}
