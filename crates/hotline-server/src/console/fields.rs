//! The `field=value` grammar of `/access` and `/0wn`, parsed like mhxd.

use hotline_proto::consts::Access;

use crate::account::AccessExtra;
use crate::console::strtoul;

/// `names` comma separated, `"none"` if empty.
fn join_flag_names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let names: Vec<&str> = names.collect();
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

/// `name` cut to the 31 bytes mhxd's name buffer holds.
fn truncate_name(name: &str) -> &str {
    name.get(..name.floor_char_boundary(31)).unwrap_or(name)
}

/// Every set flag's console config key, or its Rust name if it has none.
pub(crate) fn format_access(value: Access) -> String {
    join_flag_names(
        value
            .iter_names()
            .map(|(name, flag)| flag.console_name().unwrap_or(name)),
    )
}

/// Same, for [`AccessExtra`].
pub(crate) fn format_access_extra(value: AccessExtra) -> String {
    join_flag_names(
        value
            .iter_names()
            .map(|(name, flag)| flag.console_name().unwrap_or(name)),
    )
}

/// Applies `/access` fields to `access` and `access_extra`, like mhxd's
/// `cmd_access`. `Err` names the first unknown key.
pub(crate) fn parse_access_fields(
    fields: &str,
    access: &mut Access,
    access_extra: &mut AccessExtra,
) -> Result<(), String> {
    let mut rest = fields;
    while let Some((name, after)) = rest.split_once('=') {
        let value = after.starts_with('1');
        let extra_flag = AccessExtra::from_console_name(name);
        let flag = Access::from_console_name(name);
        if extra_flag.is_none() && flag.is_none() {
            return Err(format!("unknown argument \"{name}\""));
        }
        if let Some(extra_flag) = extra_flag {
            access_extra.set(extra_flag, value);
        }
        if let Some(flag) = flag {
            access.set(flag, value);
        }
        // mhxd steps past the value's first character, then the word.
        let mut after = after.chars();
        after.next();
        rest = after
            .as_str()
            .trim_start_matches(|c| c != ' ')
            .trim_start_matches(' ');
    }
    Ok(())
}

/// The icon and name from `/0wn` fields, like mhxd's `cmd_0wn`. A key
/// matches by prefix, and `name=` takes the rest of the line.
pub(crate) fn parse_0wn_fields(fields: &str) -> (Option<u16>, Option<String>) {
    let mut icon = None;
    let mut name = None;
    let mut rest = fields;
    while let Some((key, after)) = rest.split_once('=') {
        if key.starts_with("icon") {
            icon = Some(strtoul(after) as u16);
        } else if key.starts_with("name") {
            name = Some(truncate_name(after).to_string());
            break;
        }
        // `color`, `g0away`, and `visible` set state this server lacks.
        rest = after
            .trim_start_matches(|c| c != ' ')
            .trim_start_matches(' ');
    }
    (icon, name)
}
