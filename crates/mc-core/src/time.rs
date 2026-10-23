//! UTC date formatting from epoch milliseconds, without a date library.

/// (year, month, day, hour, minute, second, millisecond) in UTC.
fn civil(ms: i64) -> (i64, i64, i64, i64, i64, i64, i64) {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (
        year,
        month,
        day,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        millis,
    )
}

/// `YYYY-MM-DD HH:MM` (UTC), for listings.
pub fn fmt_utc(ms: i64) -> String {
    let (y, mo, d, h, mi, ..) = civil(ms);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}")
}

/// `2026-10-20T12:34:56.789+00:00`: Python's `isoformat(timespec="milliseconds")`.
pub fn utc_iso_ms(ms: i64) -> String {
    let (y, mo, d, h, mi, s, milli) = civil(ms);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}+00:00")
}

/// `20261020T123456Z`: the timestamp part of a session folder name.
pub fn dir_stamp(ms: i64) -> String {
    let (y, mo, d, h, mi, s, _) = civil(ms);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python_isoformat() {
        // datetime.fromtimestamp(1700000000.123, tz=utc).isoformat(timespec="milliseconds")
        assert_eq!(
            utc_iso_ms(1_700_000_000_123),
            "2023-11-14T22:13:20.123+00:00"
        );
        assert_eq!(utc_iso_ms(0), "1970-01-01T00:00:00.000+00:00");
        assert_eq!(utc_iso_ms(951_782_400_000), "2000-02-29T00:00:00.000+00:00");
        assert_eq!(utc_iso_ms(-1), "1969-12-31T23:59:59.999+00:00");
    }

    #[test]
    fn folder_and_listing_formats() {
        assert_eq!(dir_stamp(1_700_000_000_000), "20231114T221320Z");
        assert_eq!(fmt_utc(1_700_000_000_000), "2023-11-14 22:13");
    }
}
