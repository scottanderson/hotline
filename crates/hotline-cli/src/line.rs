/// One line in the scrollback: a chat message, a system notice (user
/// joined/left, a permission-denied notice, ...), or an error.
pub struct Line {
    pub text: Box<str>,
    pub kind: LineKind,
}

impl Line {
    pub fn new(text: impl Into<Box<str>>, kind: LineKind) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }

    pub fn system(text: impl Into<Box<str>>) -> Self {
        Self::new(text, LineKind::System)
    }

    pub fn error(text: impl Into<Box<str>>) -> Self {
        Self::new(text, LineKind::Error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Chat,
    System,
    Error,
}

/// The number of newest lines that a scrollback keeps when it trims.
pub const MAX_LINES: usize = 5000;

/// Append `line` to `lines`. Past 1.25 times [`MAX_LINES`], keep only the newest [`MAX_LINES`].
pub fn push_line(lines: &mut Vec<Line>, line: Line) {
    lines.push(line);
    if lines.len() > MAX_LINES + MAX_LINES / 4 {
        lines.drain(..lines.len() - MAX_LINES);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_line_keeps_the_newest_lines() {
        let mut lines = Vec::new();
        for i in 0..MAX_LINES * 2 {
            let text = i.to_string().into();
            push_line(
                &mut lines,
                Line {
                    text,
                    kind: LineKind::Chat,
                },
            );
        }
        assert!(lines.len() <= MAX_LINES + MAX_LINES / 4);
        assert_eq!(
            &*lines.last().unwrap().text,
            (MAX_LINES * 2 - 1).to_string()
        );
    }
}
