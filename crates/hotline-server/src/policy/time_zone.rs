use chrono::{DateTime, Datelike, Local, Timelike};
use hotline_proto::types::{days_from_civil, HlDate};

/// The time zone of each date that a server sends.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TimeZone {
    /// The server's local time, with no offset on the wire.
    #[default]
    Local,
    /// UTC.
    Utc,
}

impl TimeZone {
    /// `date`, a UTC date, in this time zone.
    pub fn convert(self, date: HlDate) -> HlDate {
        match self {
            Self::Utc => date,
            Self::Local => in_zone(date, &Local),
        }
    }
}

/// The UTC `date` in `zone`. Out of range keeps `date`.
fn in_zone(date: HlDate, zone: &impl chrono::TimeZone) -> HlDate {
    let seconds = days_from_civil(date.year.into(), 1, 1) * 86400 + i64::from(date.seconds);
    let nanos = u32::from(date.milliseconds) * 1_000_000;
    let Some(utc) = DateTime::from_timestamp(seconds, nanos) else {
        return date;
    };
    let local = utc.with_timezone(zone).naive_local();
    let Ok(year) = u16::try_from(local.year()) else {
        return date;
    };
    HlDate {
        year,
        milliseconds: date.milliseconds,
        seconds: local.ordinal0() * 86400 + local.num_seconds_from_midnight(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::*;

    #[test]
    fn utc_keeps_the_date() {
        let date = HlDate {
            year: 2024,
            milliseconds: 5,
            seconds: 59 * 86400 + 3600,
        };
        assert_eq!(TimeZone::Utc.convert(date), date);
    }

    #[test]
    fn in_zone_moves_the_date_by_the_offset() {
        let date = HlDate {
            year: 2024,
            milliseconds: 5,
            seconds: 59 * 86400 + 3600,
        };
        let east = FixedOffset::east_opt(5 * 3600 + 1800).unwrap();
        let west = FixedOffset::west_opt(3 * 3600 + 1800).unwrap();

        let ahead = HlDate {
            seconds: 59 * 86400 + 6 * 3600 + 1800,
            ..date
        };
        let behind = HlDate {
            seconds: 58 * 86400 + 21 * 3600 + 1800,
            ..date
        };
        assert_eq!(in_zone(date, &east), ahead);
        assert_eq!(in_zone(date, &west), behind);

        let new_year_eve = HlDate {
            year: 2024,
            milliseconds: 5,
            seconds: 365 * 86400 + 23 * 3600,
        };
        let new_year = HlDate {
            year: 2025,
            milliseconds: 5,
            seconds: 4 * 3600 + 1800,
        };
        assert_eq!(in_zone(new_year_eve, &east), new_year);
    }
}
