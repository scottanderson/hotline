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
        let end = self.pos.saturating_add(bytes.len());
        self.buf[self.pos..end].copy_from_slice(bytes);
        self.pos = end;
    }

    fn written(&self) -> usize {
        self.pos
    }

    fn zeros(&mut self, len: usize) {
        let end = self.pos.saturating_add(len);
        assert!(end <= N, "write past the end of a Fixed buffer");
        self.pos = end;
    }
}
