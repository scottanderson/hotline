//! [`Growable`]: a `Vec<u8>`-backed [`crate::wire::Storage`].

use crate::wire::Storage;

/// Growable backing: a `Vec<u8>`, for wire structures whose total size
/// isn't known until a variable number of pieces have been written.
#[derive(Debug, Default)]
pub struct Growable(pub(crate) Vec<u8>);

impl Storage for Growable {
    fn append(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }

    fn written(&self) -> usize {
        self.0.len()
    }

    fn zeros(&mut self, len: usize) {
        self.0.resize(self.0.len().saturating_add(len), 0);
    }
}
