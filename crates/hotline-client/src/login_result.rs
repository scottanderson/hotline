/// Fields returned by a successful login, per the `myTran_Login` (107) reply.
#[derive(Debug, Clone, Default)]
pub struct LoginResult {
    /// Server's protocol version, echoed back.
    pub version: Option<u16>,
    /// Banner ID, used to fetch the banner over HTTP (version >= 151 only).
    pub banner_id: Option<u16>,
    /// Human-readable server name (version >= 151 only).
    pub server_name: Option<String>,
}
