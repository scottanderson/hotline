//! `serde` support for any `bitflags` type: `"ALL"` for every
//! currently-defined flag, otherwise an array of individual flag names,
//! and a hex string such as `"0x8000"` for the bits that no flag defines.
//! Plug in per field via `#[serde(with = "bitflags_serde")]`.
//!
//! A non-human-readable format (e.g. a binary encoding) falls through to
//! `bitflags::serde`'s own hex-string-of-bits representation instead.

use bitflags::parser::ParseHex;
use serde::de::{Error as _, SeqAccess, Unexpected, Visitor};
use serde::ser::Error as _;
use serde::{Deserializer, Serialize, Serializer};

/// `"ALL"` when every currently-defined flag is set and no other bit,
/// otherwise an array of individual flag names, then a hex string of the
/// undefined bits if any are set.
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
    let mut iter = value.iter_names();
    let mut items: Vec<String> = iter.by_ref().map(|(name, _)| name.to_owned()).collect();
    if !iter.remaining().is_empty() {
        let mut hex = String::new();
        bitflags::parser::to_writer(iter.remaining(), &mut hex).map_err(S::Error::custom)?;
        items.push(hex);
    }
    items.serialize(serializer)
}

/// Accepts `"ALL"` (every flag defined at parse time, so a future
/// addition needs no data-file edits) or an array of individual flag
/// names and hex strings, e.g. `["READ_CHAT", "SEND_CHAT", "0x8000"]`.
pub(crate) fn deserialize<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
    T: bitflags::Flags,
    T::Bits: ParseHex + serde::Deserialize<'de>,
    D: Deserializer<'de>,
{
    if !deserializer.is_human_readable() {
        return bitflags::serde::deserialize(deserializer);
    }

    struct AllOrFlags<T>(std::marker::PhantomData<T>);

    impl<'de, T> Visitor<'de> for AllOrFlags<T>
    where
        T: bitflags::Flags,
        T::Bits: ParseHex,
    {
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
                let flag = match name.strip_prefix("0x") {
                    Some(hex) => T::Bits::parse_hex(hex).ok().map(T::from_bits_retain),
                    None => T::from_name(&name),
                }
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
    fn undefined_bits_round_trip_as_a_hex_string() {
        let value = Toy::A | Toy::from_bits_retain(0x80);
        assert_eq!(serde_json::to_string(&value).unwrap(), "[\"A\",\"0x80\"]");
        assert_eq!(
            serde_json::from_str::<Toy>("[\"A\", \"0x80\"]").unwrap(),
            value
        );
    }

    #[test]
    fn all_with_an_undefined_bit_is_not_written_as_all() {
        let value = Toy::all() | Toy::from_bits_retain(0x80);
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            "[\"A\",\"B\",\"C\",\"0x80\"]"
        );
    }

    #[test]
    fn a_bad_hex_string_is_rejected() {
        assert!(serde_json::from_str::<Toy>("[\"0xzz\"]").is_err());
    }

    #[test]
    fn an_unknown_flag_name_is_rejected() {
        assert!(serde_json::from_str::<Toy>("[\"NOPE\"]").is_err());
    }
}
