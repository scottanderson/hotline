//! Article bodies: a text flavor is UTF-8 in storage and the connection's
//! encoding on the wire.

use hotline_proto::text_encoding::{self, Encoding};

/// Whether `mime_type` names a text flavor.
fn is_text(mime_type: &str) -> bool {
    mime_type.starts_with("text/")
}

/// A body from the wire, as stored.
pub fn body_from_wire(mime_type: &str, data: Vec<u8>, encoding: &'static Encoding) -> Vec<u8> {
    if is_text(mime_type) {
        text_encoding::decode(encoding, &data).into_bytes()
    } else {
        data
    }
}

/// A stored body, as sent on the wire.
pub fn body_to_wire(mime_type: &str, data: &[u8], encoding: &'static Encoding) -> Vec<u8> {
    if is_text(mime_type) {
        text_encoding::encode(encoding, &String::from_utf8_lossy(data))
    } else {
        data.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_from_wire_stores_text_as_utf8() {
        let stored = body_from_wire("text/plain", b"caf\x8e".to_vec(), text_encoding::DEFAULT);
        assert_eq!(stored, "caf\u{e9}".as_bytes());
    }

    #[test]
    fn body_from_wire_keeps_other_flavors_as_bytes() {
        let stored = body_from_wire("image/gif", b"\x8e".to_vec(), text_encoding::DEFAULT);
        assert_eq!(stored, b"\x8e");
    }

    #[test]
    fn body_to_wire_encodes_text_in_the_connection_encoding() {
        let wire = body_to_wire("text/plain", "caf\u{e9}".as_bytes(), text_encoding::DEFAULT);
        assert_eq!(wire, b"caf\x8e");
    }
}
