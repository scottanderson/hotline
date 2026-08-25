//! `serde` support for [`HlDate`] as an ISO 8601 string, e.g.
//! `"2024-01-15T12:34:56.789Z"`. Plug in via `#[serde(with = "iso_date")]`.

use hotline_proto::types::HlDate;
use serde::{Deserialize, Deserializer, Serializer};

/// Days since 1970-01-01 for a proleptic Gregorian `(year, month, day)`.
/// Howard Hinnant's `days_from_civil`, a standard, well known algorithm.
pub(crate) fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Inverse of [`days_from_civil`]: proleptic Gregorian `(year, month,
/// day)` for days since 1970-01-01.
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

pub(crate) fn serialize<S: Serializer>(date: &HlDate, serializer: S) -> Result<S::Ok, S::Error> {
    let days = date.seconds / 86400;
    let secs_of_day = date.seconds % 86400;
    let (_, month, day) = civil_from_days(days_from_civil(date.year as i64, 1, 1) + days as i64);
    let (hour, minute, second) = (secs_of_day / 3600, secs_of_day / 60 % 60, secs_of_day % 60);
    serializer.serialize_str(&format!(
        "{:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        date.year, date.milliseconds,
    ))
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<HlDate, D::Error> {
    let s = String::deserialize(deserializer)?;
    let err = || serde::de::Error::custom(format!("expected an ISO 8601 date-time, got {s:?}"));

    let (date_part, rest) = s.split_once('T').ok_or_else(err)?;
    let time_part = rest.strip_suffix('Z').ok_or_else(err)?;
    let mut date_fields = date_part.splitn(3, '-');
    let year: u16 = date_fields
        .next()
        .ok_or_else(err)?
        .parse()
        .map_err(|_| err())?;
    let month: u32 = date_fields
        .next()
        .ok_or_else(err)?
        .parse()
        .map_err(|_| err())?;
    let day: u32 = date_fields
        .next()
        .ok_or_else(err)?
        .parse()
        .map_err(|_| err())?;

    let (hms_part, millis_part) = time_part.split_once('.').ok_or_else(err)?;
    let mut hms_fields = hms_part.splitn(3, ':');
    let hour: u32 = hms_fields
        .next()
        .ok_or_else(err)?
        .parse()
        .map_err(|_| err())?;
    let minute: u32 = hms_fields
        .next()
        .ok_or_else(err)?
        .parse()
        .map_err(|_| err())?;
    let second: u32 = hms_fields
        .next()
        .ok_or_else(err)?
        .parse()
        .map_err(|_| err())?;
    let milliseconds: u16 = millis_part.parse().map_err(|_| err())?;

    let days_since_jan1 =
        days_from_civil(year as i64, month, day) - days_from_civil(year as i64, 1, 1);
    Ok(HlDate {
        year,
        milliseconds,
        seconds: days_since_jan1 as u32 * 86400 + hour * 3600 + minute * 60 + second,
    })
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, days_from_civil, HlDate};

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Wrapper(#[serde(with = "super")] HlDate);

    #[test]
    fn days_from_civil_round_trips_through_civil_from_days() {
        for days in [0, 1, 365, 366, 10957, 19723, -1, -365] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "for {days} days");
        }
    }

    #[test]
    fn epoch_is_1970_01_01() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn date_round_trips_through_an_iso_8601_string() {
        // 2024-03-05 13:14:15, day 64 (0-indexed) into the leap year.
        let date = HlDate {
            year: 2024,
            milliseconds: 789,
            seconds: 64 * 86400 + 47655,
        };

        let json = serde_json::to_value(Wrapper(date)).unwrap();

        assert_eq!(json, "2024-03-05T13:14:15.789Z", "{json}");
        let Wrapper(round_tripped) = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped, date);
    }
}
