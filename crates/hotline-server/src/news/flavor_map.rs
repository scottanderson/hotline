//! Serializes [`NewsArticle::data`](super::NewsArticle::data) as a JSON
//! object keyed by MIME type. Plug in via `#[serde(with = "flavor_map")]`,
//! or use [`FlavorMapRef`] in a hand-written `Serialize` impl.
//!
//! A body serializes as a string when it is valid UTF-8, otherwise as a
//! byte array, regardless of its MIME type.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserializer, Serialize, Serializer};

/// Each body, keyed by its MIME type.
type Flavors = BTreeMap<Box<str>, Box<[u8]>>;

pub(crate) fn serialize<S: Serializer>(data: &Flavors, serializer: S) -> Result<S::Ok, S::Error> {
    let mut map = serializer.serialize_map(Some(data.len()))?;
    for (mime_type, body) in data {
        match std::str::from_utf8(body) {
            Ok(text) => map.serialize_entry(mime_type, text)?,
            Err(_) => map.serialize_entry(mime_type, body)?,
        }
    }
    map.end()
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Flavors, D::Error> {
    struct MapVisitor;

    impl<'de> Visitor<'de> for MapVisitor {
        type Value = Flavors;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a map of MIME type to a string or an array of byte values")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some((mime_type, Body(body))) = map.next_entry()? {
                out.insert(mime_type, body);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_map(MapVisitor)
}

/// One flavor's body: a string for text, or a byte array otherwise.
struct Body(Box<[u8]>);

impl<'de> serde::Deserialize<'de> for Body {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BodyVisitor;

        impl<'de> Visitor<'de> for BodyVisitor {
            type Value = Body;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string or an array of byte values")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Body, E> {
                Ok(Body(v.as_bytes().into()))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Body, A::Error> {
                let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(byte) = seq.next_element()? {
                    out.push(byte);
                }
                Ok(Body(out.into()))
            }
        }

        deserializer.deserialize_any(BodyVisitor)
    }
}

/// Borrows [`NewsArticle::data`](super::NewsArticle::data) for a
/// hand-written `Serialize` impl.
pub(crate) struct FlavorMapRef<'a>(pub(crate) &'a Flavors);

impl Serialize for FlavorMapRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize(self.0, serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Wrapper(#[serde(with = "super")] BTreeMap<Box<str>, Box<[u8]>>);

    fn map(mime_type: &str, data: &[u8]) -> BTreeMap<Box<str>, Box<[u8]>> {
        [(mime_type.into(), data.into())].into()
    }

    #[test]
    fn ascii_data_serializes_as_a_string_regardless_of_mime_type() {
        let json =
            serde_json::to_value(Wrapper(map("application/octet-stream", b"hello"))).unwrap();
        assert_eq!(json["application/octet-stream"], "hello");
    }

    #[test]
    fn text_mime_typed_utf8_data_serializes_as_a_string() {
        let json = serde_json::to_value(Wrapper(map("text/plain", "café".as_bytes()))).unwrap();
        assert_eq!(json["text/plain"], "café");
    }

    #[test]
    fn non_text_mime_typed_utf8_data_still_serializes_as_a_string() {
        // Any data that decodes cleanly as UTF-8 is stored as a string, regardless of MIME type.
        let json =
            serde_json::to_value(Wrapper(map("application/octet-stream", "café".as_bytes())))
                .unwrap();
        assert_eq!(json["application/octet-stream"], "café");
    }

    #[test]
    fn non_ascii_non_text_data_serializes_as_a_byte_array() {
        let data = [0xff, 0x00, 0x80];
        let json = serde_json::to_value(Wrapper(map("application/octet-stream", &data))).unwrap();
        assert!(json["application/octet-stream"].is_array(), "{json}");
    }

    #[test]
    fn data_round_trips_through_either_representation() {
        for (mime_type, data) in [
            ("text/plain", b"hello world".as_slice()),
            ("application/octet-stream", &[0xff, 0x00, 0x80]),
        ] {
            let original = map(mime_type, data);
            let json = serde_json::to_string(&Wrapper(original.clone())).unwrap();
            let Wrapper(round_tripped) = serde_json::from_str(&json).unwrap();
            assert_eq!(round_tripped, original, "for mime type {mime_type}");
        }
    }
}
