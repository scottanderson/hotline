//! Reads the `myTran_*`/`myField_*` constants out of `HLProtocol.md`.

const TEXT: &str = include_str!("../../../HLProtocol.md");

/// Constants `HLProtocol.md` omits, from openline's `HotlineClientServerCommon.h`.
///
/// Do not edit to match a variant. Each entry copies openline's own enum.
const OMITTED: &[(u32, &str)] = &[
    (214, "myTran_KillDownload"),
    (348, "myTran_GetUserList"),
    (349, "myTran_SetUserList"),
    (500, "myTran_KeepConnectionAlive"),
    (337, "myField_NewsArtRecurseDel"),
];

/// Every `(value, name)` pair whose name starts with `prefix`, from each
/// `**Title (N)**` heading and the `Constant:` line under it, plus
/// [`OMITTED`].
pub(crate) fn constants(prefix: &str) -> Vec<(u32, String)> {
    let lines: Vec<&str> = TEXT.lines().collect();
    lines
        .windows(2)
        .filter_map(|pair| {
            let name = pair[1].strip_prefix("Constant: ")?.replace("\\_", "_");
            let value = pair[0].trim_end_matches("**").rsplit_once('(')?.1;
            let value = value.strip_suffix(')')?.parse().ok()?;
            Some((value, name))
        })
        .chain(
            OMITTED
                .iter()
                .map(|&(value, name)| (value, name.to_owned())),
        )
        .filter(|(_, name)| name.starts_with(prefix))
        .collect()
}
