/// The selection and scroll position of one list.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ListCursor {
    /// The first row on screen, as the last draw left it.
    pub offset: usize,
    /// The index of the selected row.
    pub selected: usize,
}

impl ListCursor {
    /// Select the row above.
    pub const fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Select the row below, up to the last of `len` rows.
    pub fn down(&mut self, len: usize) {
        self.selected = (self.selected + 1).min(len.saturating_sub(1));
    }

    /// The index of the row on screen line `row` of a list of `len` rows.
    pub fn row_at(&self, row: u16, len: usize) -> Option<usize> {
        let index = self.offset + usize::from(row);
        (index < len).then_some(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn down_stops_at_the_last_row() {
        let mut cursor = ListCursor::default();
        cursor.down(2);
        cursor.down(2);
        assert_eq!(cursor.selected, 1);
        cursor.up();
        cursor.up();
        assert_eq!(cursor.selected, 0);
    }

    #[test]
    fn row_at_adds_the_offset() {
        let cursor = ListCursor {
            offset: 3,
            selected: 0,
        };
        assert_eq!(cursor.row_at(1, 5), Some(4));
        assert_eq!(cursor.row_at(2, 5), None);
    }
}
