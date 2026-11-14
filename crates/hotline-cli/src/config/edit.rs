//! Changes to the text of the settings file that keep its comments and
//! its layout.

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, TableLike, Value};

/// `old` with the settings of `new`. Each setting that stays the same
/// keeps its text, its comments, and its place. A changed value keeps the
/// comment on its line. `None` when `old` or `new` is not TOML.
pub fn update(old: &str, new: &str) -> Option<String> {
    let mut old: DocumentMut = old.parse().ok()?;
    let new: DocumentMut = new.parse().ok()?;
    merge_table(old.as_table_mut(), new.as_table());
    Some(old.to_string())
}

/// Make `old` hold the keys and the values of `new`.
fn merge_table(old: &mut dyn TableLike, new: &dyn TableLike) {
    let gone: Vec<String> = old
        .iter()
        .map(|(key, _)| key)
        .filter(|key| !new.contains_key(key))
        .map(str::to_owned)
        .collect();
    for key in gone {
        old.remove(&key);
    }
    for (key, item) in new.iter() {
        match old.get_mut(key) {
            Some(old) => merge_item(old, item),
            None => {
                old.insert(key, unplaced(item));
            }
        }
    }
}

/// Make `old` hold the value of `new`.
fn merge_item(old: &mut Item, new: &Item) {
    if let (Some(old), Some(new)) = (old.as_table_like_mut(), new.as_table_like()) {
        merge_table(old, new);
    } else if let (Some(old), Some(new)) = (old.as_array_of_tables_mut(), new.as_array_of_tables())
    {
        merge_array(old, new);
    } else if let (Some(old), Some(new)) = (old.as_value_mut(), new.as_value()) {
        if !same_value(old, new) {
            let decor = old.decor().clone();
            *old = new.clone();
            *old.decor_mut() = decor;
        }
    } else if old.is_value() {
        *old = unplaced(new)
            .into_value()
            .map_or_else(|item| item, Item::Value);
    } else {
        *old = unplaced(new);
    }
}

/// Make `old` hold the tables of `new`. An old table that is the same as a
/// new one stays as it is. Each other new table changes the old table
/// that has the most of its values, if one has any.
fn merge_array(old: &mut ArrayOfTables, new: &ArrayOfTables) {
    let mut left: Vec<Option<Table>> = std::mem::take(old).into_iter().map(Some).collect();
    let mut tables: Vec<Option<Table>> = new
        .iter()
        .map(|new| {
            left.iter_mut()
                .find(|old| old.as_ref().is_some_and(|old| same_table(old, new)))
                .and_then(Option::take)
        })
        .collect();
    for (table, new) in tables.iter_mut().zip(new.iter()) {
        if table.is_some() {
            continue;
        }
        let closest = left
            .iter_mut()
            .filter_map(|old| Some((shared_values(old.as_ref()?, new), old)))
            .filter(|(shared, _)| *shared > 0)
            .rev()
            .max_by_key(|(shared, _)| *shared);
        *table = Some(match closest.and_then(|(_, old)| old.take()) {
            Some(mut old) => {
                merge_table(&mut old, new);
                old
            }
            None => unplaced_table(new),
        });
    }
    old.extend(tables.into_iter().flatten());
}

/// The number of keys that have the same value in `a` and `b`.
fn shared_values(a: &dyn TableLike, b: &dyn TableLike) -> usize {
    a.iter()
        .filter(|(key, a)| b.get(key).is_some_and(|b| same_item(a, b)))
        .count()
}

/// `true` if `a` and `b` hold the same keys and values.
fn same_table(a: &dyn TableLike, b: &dyn TableLike) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|(key, a)| b.get(key).is_some_and(|b| same_item(a, b)))
}

/// `true` if `a` and `b` hold the same value.
fn same_item(a: &Item, b: &Item) -> bool {
    match (a, b) {
        (Item::Value(a), Item::Value(b)) => same_value(a, b),
        (Item::ArrayOfTables(a), Item::ArrayOfTables(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same_table(a, b))
        }
        _ => match (a.as_table_like(), b.as_table_like()) {
            (Some(a), Some(b)) => same_table(a, b),
            _ => false,
        },
    }
}

/// `true` if `a` and `b` hold the same string, integer, boolean, array, or
/// table.
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(a), Value::String(b)) => a.value() == b.value(),
        (Value::Integer(a), Value::Integer(b)) => a.value() == b.value(),
        (Value::Boolean(a), Value::Boolean(b)) => a.value() == b.value(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same_value(a, b))
        }
        (Value::InlineTable(a), Value::InlineTable(b)) => same_table(a, b),
        _ => false,
    }
}

/// `item` with no place in the file, so that each of its tables follows
/// the table before it.
fn unplaced(item: &Item) -> Item {
    match item {
        Item::Table(table) => Item::Table(unplaced_table(table)),
        Item::ArrayOfTables(array) => {
            Item::ArrayOfTables(array.iter().map(unplaced_table).collect())
        }
        _ => item.clone(),
    }
}

/// `table` with no place in the file. See [`unplaced`].
fn unplaced_table(table: &Table) -> Table {
    let mut new: Table = table
        .iter()
        .map(|(key, item)| (key, unplaced(item)))
        .collect();
    new.set_implicit(table.is_implicit());
    new.set_dotted(table.is_dotted());
    new
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_keeps_the_comments_and_the_layout_of_each_setting() {
        let old =
            "# my settings\nname   = 'Alice'  # me\nicon = 128\n\n[bell]\n# quiet\nchat = false\n";
        let new = "name = \"Alice\"\nicon = 128\n\n[bell]\nchat = false\n";
        assert_eq!(update(old, new).unwrap(), old);
    }

    #[test]
    fn update_changes_a_value_and_keeps_the_comment_on_its_line() {
        let old = "# my settings\nname = 'Alice'  # me\nicon = 128\n";
        let new = "name = \"Bob\"\nicon = 128\n";
        assert_eq!(
            update(old, new).unwrap(),
            "# my settings\nname = \"Bob\"  # me\nicon = 128\n"
        );
    }

    #[test]
    fn update_adds_and_removes_settings() {
        let old = "name = \"Alice\"\n\n[bell]\nchat = false\n";
        let new = "icon = 5\n\n[bell]\nchat = false\n\n[keys.pane.files]\ndelete = [\"Delete\"]\n";
        assert_eq!(
            update(old, new).unwrap(),
            "icon = 5\n\n[bell]\nchat = false\n\n[keys.pane.files]\ndelete = [\"Delete\"]\n"
        );
    }

    #[test]
    fn update_keeps_the_comments_of_each_bookmark_that_stays() {
        let old = "# home\n[[bookmarks]]\nname = \"a\"\naddress = \"a.example\"\n\n# work\n[[bookmarks]]\nname = \"b\"\naddress = \"b.example\"\n\n# play\n[[bookmarks]]\nname = \"c\"\naddress = \"c.example\"\n";
        let new = "[[bookmarks]]\nname = \"a\"\naddress = \"a.example\"\n\n[[bookmarks]]\nname = \"c\"\naddress = \"c.example\"\nlogin = \"alice\"\n\n[[bookmarks]]\nname = \"d\"\naddress = \"d.example\"\n";
        assert_eq!(
            update(old, new).unwrap(),
            "# home\n[[bookmarks]]\nname = \"a\"\naddress = \"a.example\"\n\n# play\n[[bookmarks]]\nname = \"c\"\naddress = \"c.example\"\nlogin = \"alice\"\n\n[[bookmarks]]\nname = \"d\"\naddress = \"d.example\"\n"
        );
    }

    #[test]
    fn update_refuses_a_file_that_is_not_toml() {
        assert_eq!(update("name = ", "name = \"Alice\"\n"), None);
    }
}
