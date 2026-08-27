/// A pending `HTXF` transfer's reference number, sent as
/// `myField_RefNum` (107) and echoed back in the `HTXF` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransferRef(pub u32);
