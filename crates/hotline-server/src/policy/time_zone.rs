use chrono::{DateTime, Datelike, Local, NaiveDateTime, Timelike};
use hotline_proto::types::{HlDate, days_from_civil};

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
    to_naive(date)
        .map(|utc| utc.and_utc().with_timezone(zone).naive_local())
        .and_then(|local| from_naive(local, date.milliseconds))
        .unwrap_or(date)
}

/// The `date` in `zone` as UTC. The earlier of two times counts. Out of
/// range, or a time that `zone` skips, keeps `date`.
pub(crate) fn to_utc(date: HlDate, zone: &impl chrono::TimeZone) -> HlDate {
    to_naive(date)
        .and_then(|local| zone.from_local_datetime(&local).earliest())
        .and_then(|utc| from_naive(utc.naive_utc(), date.milliseconds))
        .unwrap_or(date)
}

/// `date` with no time zone.
fn to_naive(date: HlDate) -> Option<NaiveDateTime> {
    let seconds = days_from_civil(date.year.into(), 1, 1)
        .saturating_mul(86400)
        .saturating_add(date.seconds.into());
    let nanos = u32::from(date.milliseconds).checked_mul(1_000_000)?;
    DateTime::from_timestamp(seconds, nanos).map(|utc| utc.naive_utc())
}

/// The [`HlDate`] of `naive`, with `milliseconds`.
fn from_naive(naive: NaiveDateTime, milliseconds: u16) -> Option<HlDate> {
    Some(HlDate {
        year: u16::try_from(naive.year()).ok()?,
        milliseconds,
        seconds: naive
            .ordinal0()
            .saturating_mul(86400)
            .saturating_add(naive.num_seconds_from_midnight()),
    })
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

    #[test]
    fn to_utc_keeps_a_date_with_too_many_milliseconds() {
        let date = HlDate {
            year: 2024,
            milliseconds: u16::MAX,
            seconds: 3600,
        };
        let east = FixedOffset::east_opt(3600).unwrap();
        assert_eq!(to_utc(date, &east), date);
    }

    #[test]
    fn to_utc_undoes_in_zone() {
        let date = HlDate {
            year: 2024,
            milliseconds: 5,
            seconds: 365 * 86400 + 23 * 3600,
        };
        let east = FixedOffset::east_opt(5 * 3600 + 1800).unwrap();
        let west = FixedOffset::west_opt(3 * 3600 + 1800).unwrap();

        assert_eq!(to_utc(in_zone(date, &east), &east), date);
        assert_eq!(to_utc(in_zone(date, &west), &west), date);
    }
}
