//! [`Fixed`]: a stack-allocated [`crate::wire::Storage`] backing.

use crate::wire::Storage;

/// Fixed-size backing: a stack-allocated `[u8; N]`, no heap allocation.
#[derive(Debug)]
pub struct Fixed<const N: usize> {
    pub(crate) buf: [u8; N],
    pub(crate) pos: usize,
}

impl<const N: usize> Default for Fixed<N> {
    fn default() -> Self {
        Self {
            buf: [0; N],
            pos: 0,
        }
    }
}

impl<const N: usize> Storage for Fixed<N> {
    fn append(&mut self, bytes: &[u8]) {
        let len = bytes.len();
        self.buf[self.pos..self.pos + len].copy_from_slice(bytes);
        self.pos += len;
    }
}
