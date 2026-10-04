use std::net::IpAddr;
use std::time::SystemTime;

/// One address's ban: when it was imposed and, unless permanent, when it lifts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BanRecord {
    /// The banned address.
    pub ip: IpAddr,
    /// `None` means the ban never expires.
    #[serde(with = "until_rfc3339")]
    pub until: Option<SystemTime>,
    /// Why the address is banned. Empty when no one gave a reason.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl BanRecord {
    /// A ban on `ip` until `until`, or a permanent one if `None`, with no note.
    pub const fn new(ip: IpAddr, until: Option<SystemTime>) -> Self {
        Self {
            ip,
            until,
            note: String::new(),
        }
    }

    /// Whether the ban still applies now.
    pub fn is_active(&self) -> bool {
        self.until.is_none_or(|until| until > SystemTime::now())
    }
}

/// Serializes `Option<SystemTime>` as an RFC 3339 (ISO 8601) timestamp
/// string, so `bans.json` reads as a human date instead of raw epoch seconds.
mod until_rfc3339 {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::SystemTime;

    pub fn serialize<S: Serializer>(
        until: &Option<SystemTime>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        until
            .map(|t| humantime::format_rfc3339_seconds(t).to_string())
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<SystemTime>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|s| humantime::parse_rfc3339(&s).map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn addr() -> IpAddr {
        "192.0.2.1".parse().unwrap()
    }

    #[test]
    fn until_serializes_as_an_rfc3339_timestamp_and_round_trips() {
        let record = BanRecord::new(
            addr(),
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        );
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(json, r#"{"ip":"192.0.2.1","until":"2023-11-14T22:13:20Z"}"#);
        assert_eq!(serde_json::from_str::<BanRecord>(&json).unwrap(), record);
    }

    #[test]
    fn a_permanent_ban_serializes_until_as_null() {
        let record = BanRecord::new(addr(), None);
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(json, r#"{"ip":"192.0.2.1","until":null}"#);
        assert_eq!(serde_json::from_str::<BanRecord>(&json).unwrap(), record);
    }

    #[test]
    fn a_note_round_trips() {
        let record = BanRecord {
            note: "spam".into(),
            ..BanRecord::new(addr(), None)
        };
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(json, r#"{"ip":"192.0.2.1","until":null,"note":"spam"}"#);
        assert_eq!(serde_json::from_str::<BanRecord>(&json).unwrap(), record);
    }
}
