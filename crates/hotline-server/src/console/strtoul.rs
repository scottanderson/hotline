/// C's `strtoul(s, 0, 0)`: leading whitespace, an optional sign, then a
/// `0x` hex, `0` octal, or decimal number, up to the first other character.
pub(crate) fn strtoul(s: &str) -> u64 {
    let s = s.trim_start();
    let (negative, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let (radix, digits) = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => (16, hex),
        None if s.starts_with('0') => (8, s),
        None => (10, s),
    };
    let value = digits
        .chars()
        .map_while(|c| c.to_digit(radix))
        .fold(0u64, |n, d| {
            n.wrapping_mul(u64::from(radix)).wrapping_add(u64::from(d))
        });
    if negative {
        value.wrapping_neg()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_like_c() {
        assert_eq!(strtoul("42 name=x"), 42);
        assert_eq!(strtoul("  7"), 7);
        assert_eq!(strtoul("0x1f"), 31);
        assert_eq!(strtoul("010"), 8);
        assert_eq!(strtoul("5abc"), 5);
        assert_eq!(strtoul("bob"), 0);
        assert_eq!(strtoul(""), 0);
        assert_eq!(strtoul("-1") as u16, u16::MAX);
    }
}
