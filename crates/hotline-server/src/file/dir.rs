use std::collections::HashMap;

use crate::file::Node;

#[derive(Clone, Default)]
pub(crate) struct Dir {
    pub(crate) entries: HashMap<String, Node>,
}

impl Dir {
    pub(crate) fn get_dir<'a>(&'a self, components: &[String]) -> Option<&'a Dir> {
        let mut current = self;
        for component in components {
            match current.entries.get(component)? {
                Node::Dir(d) => current = d,
                Node::File(_) => return None,
            }
        }
        Some(current)
    }

    /// Like [`Self::get_dir`], but mutable.
    pub(crate) fn get_dir_mut<'a>(&'a mut self, components: &[String]) -> Option<&'a mut Dir> {
        let mut current = self;
        for component in components {
            current = match current.entries.get_mut(component)? {
                Node::Dir(d) => d,
                Node::File(_) => return None,
            };
        }
        Some(current)
    }

    /// Like [`Self::get_dir_mut`], but creates each missing directory.
    /// `None` when a component names a file.
    pub(crate) fn make_dirs<'a>(&'a mut self, components: &[String]) -> Option<&'a mut Dir> {
        let mut current = self;
        for component in components {
            current = match current
                .entries
                .entry(component.clone())
                .or_insert_with(|| Node::Dir(Dir::default()))
            {
                Node::Dir(d) => d,
                Node::File(_) => return None,
            };
        }
        Some(current)
    }
}
