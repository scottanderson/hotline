//! A binary property list (`bplist00`) that holds one string, as macOS
//! keeps a Finder comment.

/// The magic number and version at the start of the list.
const MAGIC: &[u8; 8] = b"bplist00";
/// The length of the trailer at the end of the list.
const TRAILER_LEN: usize = 32;

/// `s` as a property list. An ASCII string is one byte for each
/// character, and another string is UTF-16BE.
pub fn encode_string(s: &str) -> Vec<u8> {
    let mut list = MAGIC.to_vec();
    if s.is_ascii() {
        push_marker(&mut list, 0x50, s.len());
        list.extend_from_slice(s.as_bytes());
    } else {
        let units: Vec<u16> = s.encode_utf16().collect();
        push_marker(&mut list, 0x60, units.len());
        list.extend(units.iter().flat_map(|unit| unit.to_be_bytes()));
    }
    let table_at = list.len() as u64;
    // The offset table: the one object starts after the magic number.
    list.push(MAGIC.len() as u8);
    list.extend_from_slice(&[0; 6]);
    list.extend_from_slice(&[1, 1]);
    list.extend_from_slice(&1u64.to_be_bytes());
    list.extend_from_slice(&0u64.to_be_bytes());
    list.extend_from_slice(&table_at.to_be_bytes());
    list
}

/// Push the marker of an object of `kind` with `count` units. A count of
/// 15 or more follows the marker as an integer object.
fn push_marker(list: &mut Vec<u8>, kind: u8, count: usize) {
    if count < 15 {
        list.push(kind | count as u8);
        return;
    }
    list.push(kind | 0x0F);
    match count {
        0..=0xFF => list.extend_from_slice(&[0x10, count as u8]),
        0x100..=0xFFFF => {
            list.push(0x11);
            list.extend_from_slice(&(count as u16).to_be_bytes());
        }
        0x1_0000..=0xFFFF_FFFF => {
            list.push(0x12);
            list.extend_from_slice(&(count as u32).to_be_bytes());
        }
        _ => {
            list.push(0x13);
            list.extend_from_slice(&(count as u64).to_be_bytes());
        }
    }
}

/// The string at the top of the property list `list`. `None` if `list` is
/// not a property list, or if its top object is not a string.
pub fn decode_string(list: &[u8]) -> Option<String> {
    if !list.starts_with(MAGIC) {
        return None;
    }
    let trailer = list.get(list.len().checked_sub(TRAILER_LEN)?..)?;
    let offset_size = usize::from(*trailer.get(6)?);
    let top = usize::try_from(be_uint(trailer.get(16..24)?)?).ok()?;
    let table_at = usize::try_from(be_uint(trailer.get(24..32)?)?).ok()?;
    let entry_at = table_at.checked_add(top.checked_mul(offset_size)?)?;
    let object_at = be_uint(list.get(entry_at..entry_at.checked_add(offset_size)?)?)?;
    let object = list.get(usize::try_from(object_at).ok()?..)?;
    let (&marker, rest) = object.split_first()?;
    let (count, rest) = match marker & 0x0F {
        0x0F => {
            let (&int_marker, rest) = rest.split_first()?;
            if int_marker & 0xF0 != 0x10 {
                return None;
            }
            let len = 1usize.checked_shl(u32::from(int_marker & 0x0F))?;
            let count = usize::try_from(be_uint(rest.get(..len)?)?).ok()?;
            (count, rest.get(len..)?)
        }
        count => (usize::from(count), rest),
    };
    match marker & 0xF0 {
        0x50 => {
            let bytes = rest.get(..count)?;
            bytes
                .is_ascii()
                .then(|| String::from_utf8_lossy(bytes).into_owned())
        }
        0x60 => {
            let bytes = rest.get(..count.checked_mul(2)?)?;
            let units: Vec<u16> = bytes
                .as_chunks()
                .0
                .iter()
                .map(|&pair| u16::from_be_bytes(pair))
                .collect();
            String::from_utf16(&units).ok()
        }
        _ => None,
    }
}

/// The big-endian unsigned integer in `bytes`, of at most 8 bytes.
fn be_uint(bytes: &[u8]) -> Option<u64> {
    (bytes.len() <= 8).then(|| {
        bytes
            .iter()
            .fold(0u64, |n, &byte| (n << 8) | u64::from(byte))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ascii_string_is_one_byte_for_each_character() {
        let mut expected = b"bplist00\x56a note\x08".to_vec();
        expected.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 1]);
        expected.extend_from_slice(&1u64.to_be_bytes());
        expected.extend_from_slice(&0u64.to_be_bytes());
        expected.extend_from_slice(&15u64.to_be_bytes());
        assert_eq!(encode_string("a note"), expected);
        assert_eq!(decode_string(&expected).as_deref(), Some("a note"));
    }

    #[test]
    fn another_string_is_utf16() {
        let list = encode_string("café");
        assert_eq!(&list[8..17], b"\x64\x00c\x00a\x00f\x00\xe9");
        assert_eq!(decode_string(&list).as_deref(), Some("café"));
    }

    #[test]
    fn a_long_string_has_its_count_as_an_integer() {
        let text = "x".repeat(300);
        let list = encode_string(&text);
        assert_eq!(&list[8..12], &[0x5F, 0x11, 0x01, 0x2C]);
        assert_eq!(decode_string(&list), Some(text));
    }

    #[test]
    fn a_list_that_is_not_a_string_is_none() {
        assert_eq!(decode_string(b"not a list"), None);
        let mut list = encode_string("a");
        list[8] = 0x08;
        assert_eq!(decode_string(&list), None);
    }
}
