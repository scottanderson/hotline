/// Where a [`super::Writer`] writes its bytes.
pub trait Storage: Default {
    /// Append `bytes` to the end of what's been written so far.
    ///
    /// # Panics
    /// Implementations that can't grow (e.g. [`Fixed`](super::Fixed)) panic
    /// if this would write past their capacity.
    fn append(&mut self, bytes: &[u8]);
    /// The number of bytes written so far.
    fn written(&self) -> usize;
    /// Append `len` zero bytes.
    ///
    /// # Panics
    /// Like [`Storage::append`].
    fn zeros(&mut self, len: usize);
}
