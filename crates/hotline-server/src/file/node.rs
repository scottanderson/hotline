use super::dir::Dir;

#[derive(Clone)]
pub(crate) enum Node {
    File(Vec<u8>),
    Dir(Dir),
}
