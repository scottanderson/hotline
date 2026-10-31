//! The name a new file or folder is stored under.

/// The longest stored name, in UTF-8 bytes.
const MAX_NAME_LEN: usize = 255;

/// Names Windows reserves for devices, with or without an extension.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// `name` as a new file or folder stores it on this platform. `Err` for an
/// empty name.
pub fn validate_file_name(name: &str) -> Result<String, &'static str> {
    if name.is_empty() {
        Err("invalid file name.")?;
    }
    Ok(if cfg!(windows) {
        windows_name(name)
    } else {
        unix_name(name)
    })
}

/// `name` with each `/`, `\`, and NUL mapped to `-`, and `.` and `..`
/// mapped to dashes, cut to 255 bytes.
fn unix_name(name: &str) -> String {
    let mut mapped = name.replace(['/', '\\', '\0'], "-");
    if mapped == "." || mapped == ".." {
        mapped = "-".repeat(mapped.len());
    }
    truncate(&mut mapped);
    mapped
}

/// A control character, `\ / : * ? " < > |`, and a leading `.` become `-`,
/// and a reserved device name such as `CON` gets a `-` after it. The name
/// is cut to 255 bytes, then each trailing dot and space becomes `-`.
fn windows_name(name: &str) -> String {
    let mut mapped: String = name
        .chars()
        .map(|c| match c {
            '\0'..='\x1f'
            | '\u{80}'..='\u{9f}'
            | '\\'
            | '/'
            | ':'
            | '*'
            | '?'
            | '"'
            | '<'
            | '>'
            | '|' => '-',
            c => c,
        })
        .collect();
    if mapped.starts_with('.') {
        mapped.replace_range(..1, "-");
    }
    let stem = mapped
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    if RESERVED
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        mapped.insert(stem.len(), '-');
    }
    truncate(&mut mapped);
    let kept = mapped.trim_end_matches(['.', ' ']).len();
    let dashes = "-".repeat(mapped.len() - kept);
    mapped.replace_range(kept.., &dashes);
    mapped
}

/// Cut `name` to [`MAX_NAME_LEN`] bytes at a character boundary.
fn truncate(name: &mut String) {
    let mut cut = MAX_NAME_LEN.min(name.len());
    while !name.is_char_boundary(cut) {
        cut -= 1;
    }
    name.truncate(cut);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_name_keeps_a_plain_name() {
        assert_eq!(windows_name("Read Me.txt"), "Read Me.txt");
    }

    #[test]
    fn windows_name_separators_and_wildcards_become_dashes() {
        assert_eq!(
            windows_name("AC/DC: \"Live\" <*?|\\>.mp3"),
            "AC-DC- -Live- ------.mp3"
        );
    }

    #[test]
    fn windows_name_a_control_character_becomes_a_dash() {
        assert_eq!(windows_name("a\tb\rc"), "a-b-c");
        assert_eq!(windows_name("a\u{81}b\u{9e}"), "a-b-");
    }

    #[test]
    fn windows_name_a_leading_dot_becomes_a_dash() {
        assert_eq!(windows_name(".htaccess"), "-htaccess");
        assert_eq!(windows_name(".."), "--");
        assert_eq!(windows_name("."), "-");
    }

    #[test]
    fn windows_name_trailing_dots_and_spaces_become_dashes() {
        assert_eq!(windows_name("notes. . "), "notes----");
        assert_eq!(windows_name("a b.c"), "a b.c");
    }

    #[test]
    fn windows_name_a_reserved_device_name_gets_a_dash() {
        assert_eq!(windows_name("CON"), "CON-");
        assert_eq!(windows_name("nul.txt"), "nul-.txt");
        assert_eq!(windows_name("COM10"), "COM10");
        assert_eq!(windows_name("console"), "console");
        assert_eq!(windows_name("CON "), "CON--");
        assert_eq!(windows_name("aux .txt"), "aux- .txt");
    }

    #[test]
    fn windows_name_a_long_name_is_cut_at_a_character_boundary() {
        let name = "\u{e9}".repeat(200);
        let stored = windows_name(&name);
        assert_eq!(stored.len(), 254);
        assert!(stored.chars().all(|c| c == '\u{e9}'));
    }

    #[test]
    fn windows_name_maps_a_name_of_spaces() {
        assert_eq!(windows_name("  "), "--");
    }

    #[test]
    fn an_empty_name_is_an_error() {
        assert!(validate_file_name("").is_err());
    }

    #[test]
    fn unix_name_maps_only_slashes() {
        assert_eq!(
            unix_name("AC/DC: \"Live\" <*?|\\>.mp3"),
            "AC-DC: \"Live\" <*?|->.mp3"
        );
        assert_eq!(unix_name(".htaccess"), ".htaccess");
        assert_eq!(unix_name("CON. "), "CON. ");
    }

    #[test]
    fn unix_name_maps_a_nul() {
        assert_eq!(unix_name("a\0b"), "a-b");
    }

    #[test]
    fn unix_name_maps_the_dot_names() {
        assert_eq!(unix_name("."), "-");
        assert_eq!(unix_name(".."), "--");
        assert_eq!(unix_name("..."), "...");
        assert_eq!(unix_name("../etc"), "..-etc");
    }

    #[test]
    fn unix_name_keeps_a_sidecar_prefix() {
        assert_eq!(unix_name("._report.txt"), "._report.txt");
    }

    #[test]
    fn unix_name_is_cut_at_a_character_boundary() {
        let stored = unix_name(&"\u{e9}".repeat(200));
        assert_eq!(stored.len(), 254);
    }
}
