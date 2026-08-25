/// A category's listing fields: everything [`crate::news::NewsBundle::list_dir`] needs
/// besides the item's name.
#[derive(Debug, Clone)]
pub struct NewsCategorySummary {
    pub count: u16,
    pub guid: [u8; 16],
    pub add_sn: u32,
    pub del_sn: u32,
}
