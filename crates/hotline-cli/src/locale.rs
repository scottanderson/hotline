//! The user's locale for dates.

use chrono::Locale;

/// The locale for dates: `LC_ALL`, then `LC_TIME`, then `LANG`, then the
/// system's locale. `POSIX` when none is known.
pub fn user_locale() -> Locale {
    ["LC_ALL", "LC_TIME", "LANG"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
        .or_else(sys_locale::get_locale)
        .and_then(|value| posix_locale(&value))
        .unwrap_or(Locale::POSIX)
}

/// The locale of a POSIX value such as `en_US.UTF-8`, or of a BCP 47 tag
/// such as `en-US`.
fn posix_locale(value: &str) -> Option<Locale> {
    match value.split(['.', '@']).next()? {
        "C" => Some(Locale::POSIX),
        name => Locale::try_from(name.replace('-', "_").as_str()).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_locale_reads_posix_values_and_tags() {
        assert_eq!(posix_locale("C"), Some(Locale::POSIX));
        assert_eq!(posix_locale("POSIX.UTF-8"), Some(Locale::POSIX));
        assert_eq!(posix_locale("en_US.UTF-8"), Some(Locale::en_US));
        assert_eq!(posix_locale("de-DE"), Some(Locale::de_DE));
        assert_eq!(posix_locale("fr_CA@euro"), Some(Locale::fr_CA));
    }
}
