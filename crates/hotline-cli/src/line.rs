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

/// The lines of a chat or a room, and a count of its pushes.
#[derive(Default)]
pub struct Scrollback {
    lines: Vec<Line>,
    revision: u64,
}

impl Scrollback {
    /// Append `line`, one row for each `\r`-separated part. Past 1.25
    /// times [`MAX_LINES`], keep only the newest [`MAX_LINES`].
    pub fn push(&mut self, line: Line) {
        let lines = &mut self.lines;
        lines.extend(line.text.split('\r').map(|part| Line::new(part, line.kind)));
        if lines.len() > MAX_LINES + MAX_LINES / 4 {
            lines.drain(..lines.len() - MAX_LINES);
        }
        self.revision += 1;
    }

    /// The number of pushes.
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

impl From<Line> for Scrollback {
    fn from(line: Line) -> Self {
        let mut lines = Self::default();
        lines.push(line);
        lines
    }
}

impl std::ops::Deref for Scrollback {
    type Target = [Line];

    fn deref(&self) -> &Self::Target {
        &self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_keeps_the_newest_lines() {
        let mut lines = Scrollback::default();
        for i in 0..MAX_LINES * 2 {
            let text = i.to_string().into();
            lines.push(Line {
                text,
                kind: LineKind::Chat,
            });
        }
        assert!(lines.len() <= MAX_LINES + MAX_LINES / 4);
        assert_eq!(
            &*lines.last().unwrap().text,
            (MAX_LINES * 2 - 1).to_string()
        );
    }

    #[test]
    fn push_gives_each_cr_part_its_own_row() {
        let mut lines = Scrollback::default();
        lines.push(Line::error("one\rtwo"));
        let texts: Vec<_> = lines.iter().map(|line| &*line.text).collect();
        assert_eq!(texts, ["one", "two"]);
        assert!(lines.iter().all(|line| line.kind == LineKind::Error));
    }

    #[test]
    fn push_counts_each_push() {
        let mut lines = Scrollback::default();
        lines.push(Line::system("one\rtwo"));
        lines.push(Line::system("three"));
        assert_eq!(lines.revision(), 2);
    }
}
