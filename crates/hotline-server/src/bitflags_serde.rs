//! `serde` support for any `bitflags` type: `"ALL"` for every
//! currently-defined flag, otherwise an array of individual flag names.
//! Plug in per field via `#[serde(with = "bitflags_serde")]`.
//!
//! A non-human-readable format (e.g. a binary encoding) falls through to
//! `bitflags::serde`'s own hex-string-of-bits representation instead.

use serde::de::{Error as _, SeqAccess, Unexpected, Visitor};
use serde::{Deserializer, Serialize, Serializer};

/// `"ALL"` when every currently-defined flag is set, otherwise an array
/// of individual flag names.
pub(crate) fn serialize<T, S>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    T: bitflags::Flags,
    T::Bits: bitflags::parser::WriteHex + Serialize,
    S: Serializer,
{
    if !serializer.is_human_readable() {
        return bitflags::serde::serialize(value, serializer);
    }
    if value.bits() == T::all().bits() {
        return serializer.serialize_str("ALL");
    }
    let names: Vec<&'static str> = value.iter_names().map(|(name, _)| name).collect();
    names.serialize(serializer)
}

/// Accepts `"ALL"` (every flag defined at parse time, so a future
/// addition needs no data-file edits) or an array of individual flag
/// names, e.g. `["READ_CHAT", "SEND_CHAT"]`.
pub(crate) fn deserialize<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
    T: bitflags::Flags,
    T::Bits: bitflags::parser::ParseHex + serde::Deserialize<'de>,
    D: Deserializer<'de>,
{
    if !deserializer.is_human_readable() {
        return bitflags::serde::deserialize(deserializer);
    }

    struct AllOrFlags<T>(std::marker::PhantomData<T>);

    impl<'de, T: bitflags::Flags> Visitor<'de> for AllOrFlags<T> {
        type Value = T;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("\"ALL\" or an array of flag names")
        }

        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<T, E> {
            if v == "ALL" {
                Ok(T::all())
            } else {
                Err(E::invalid_value(Unexpected::Str(v), &self))
            }
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<T, A::Error> {
            let mut value = T::empty();
            while let Some(name) = seq.next_element::<String>()? {
                let flag = T::from_name(&name)
                    .ok_or_else(|| A::Error::custom(format!("unknown flag {name:?}")))?;
                value.insert(flag);
            }
            Ok(value)
        }
    }

    deserializer.deserialize_any(AllOrFlags(std::marker::PhantomData))
}

#[cfg(test)]
mod tests {
    bitflags::bitflags! {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        struct Toy: u8 {
            const A = 1 << 0;
            const B = 1 << 1;
            const C = 1 << 2;
        }
    }

    impl serde::Serialize for Toy {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            super::serialize(self, serializer)
        }
    }

    impl<'de> serde::Deserialize<'de> for Toy {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            super::deserialize(deserializer)
        }
    }

    #[test]
    fn all_round_trips_through_every_flag() {
        assert_eq!(serde_json::to_string(&Toy::all()).unwrap(), "\"ALL\"");
        assert_eq!(serde_json::from_str::<Toy>("\"ALL\"").unwrap(), Toy::all());
    }

    #[test]
    fn a_partial_set_round_trips_as_an_array_of_names() {
        let value = Toy::A | Toy::C;
        assert_eq!(serde_json::to_string(&value).unwrap(), "[\"A\",\"C\"]");
        assert_eq!(
            serde_json::from_str::<Toy>("[\"A\", \"C\"]").unwrap(),
            value
        );
    }

    #[test]
    fn empty_round_trips_as_an_empty_array() {
        assert_eq!(serde_json::to_string(&Toy::empty()).unwrap(), "[]");
        assert_eq!(serde_json::from_str::<Toy>("[]").unwrap(), Toy::empty());
    }

    #[test]
    fn an_unknown_flag_name_is_rejected() {
        assert!(serde_json::from_str::<Toy>("[\"NOPE\"]").is_err());
    }
}
