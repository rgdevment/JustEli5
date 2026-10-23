//! Edge cases of the UTC formatting: leap days, century years, times before
//! 1970, and the ends of the range. Expected values come from Python's datetime.

use mc_core::time::{dir_stamp, fmt_utc, utc_iso_ms};

#[test]
fn leap_day_2024() {
    assert_eq!(
        utc_iso_ms(1_709_208_000_000),
        "2024-02-29T12:00:00.000+00:00"
    );
    assert_eq!(
        utc_iso_ms(1_709_251_199_999),
        "2024-02-29T23:59:59.999+00:00"
    );
    assert_eq!(
        utc_iso_ms(1_709_251_200_000),
        "2024-03-01T00:00:00.000+00:00"
    );
    assert_eq!(dir_stamp(1_709_251_199_999), "20240229T235959Z");
    assert_eq!(fmt_utc(1_709_208_000_000), "2024-02-29 12:00");
}

#[test]
fn century_year_2100_is_not_a_leap_year() {
    assert_eq!(
        utc_iso_ms(4_107_542_399_999),
        "2100-02-28T23:59:59.999+00:00"
    );
    assert_eq!(
        utc_iso_ms(4_107_542_400_000),
        "2100-03-01T00:00:00.000+00:00"
    );
    assert_eq!(dir_stamp(4_107_542_400_000), "21000301T000000Z");
}

#[test]
fn year_1900_is_not_a_leap_year_either() {
    assert_eq!(
        utc_iso_ms(-2_203_891_200_000),
        "1900-03-01T00:00:00.000+00:00"
    );
    assert_eq!(
        utc_iso_ms(-2_203_891_200_001),
        "1900-02-28T23:59:59.999+00:00"
    );
}

#[test]
fn negative_milliseconds_are_before_the_epoch() {
    assert_eq!(utc_iso_ms(-1), "1969-12-31T23:59:59.999+00:00");
    assert_eq!(utc_iso_ms(-1_000), "1969-12-31T23:59:59.000+00:00");
    assert_eq!(utc_iso_ms(-1_500), "1969-12-31T23:59:58.500+00:00");
    assert_eq!(utc_iso_ms(-86_400_000), "1969-12-31T00:00:00.000+00:00");
    assert_eq!(utc_iso_ms(-86_400_001), "1969-12-30T23:59:59.999+00:00");
    assert_eq!(dir_stamp(-1), "19691231T235959Z");
    assert_eq!(fmt_utc(-1), "1969-12-31 23:59");
}

#[test]
fn the_last_millisecond_of_year_9999() {
    assert_eq!(
        utc_iso_ms(253_402_300_799_999),
        "9999-12-31T23:59:59.999+00:00"
    );
}

#[test]
fn the_millisecond_field_is_always_three_digits() {
    assert_eq!(utc_iso_ms(1_000), "1970-01-01T00:00:01.000+00:00");
    assert_eq!(utc_iso_ms(1_010), "1970-01-01T00:00:01.010+00:00");
    assert_eq!(utc_iso_ms(1_001), "1970-01-01T00:00:01.001+00:00");
}

/// Days from 1900-01-01 to 2100-12-31, checked against a day-by-day
/// calendar written independently of the formatter.
#[test]
fn every_day_from_1900_to_2100_follows_the_calendar() {
    fn is_leap(y: i64) -> bool {
        (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
    }
    fn days_in_month(y: i64, m: i64) -> i64 {
        match m {
            2 if is_leap(y) => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        }
    }
    let (mut y, mut m, mut d) = (1900i64, 1i64, 1i64);
    // 1900-01-01T00:00:00Z
    let mut ms: i64 = -2_208_988_800_000;
    let day_ms = 86_400_000i64;
    while y <= 2100 {
        let expected = format!("{y:04}-{m:02}-{d:02}T00:00:00.000+00:00");
        assert_eq!(utc_iso_ms(ms), expected);
        // The millisecond before midnight belongs to the previous day.
        assert!(
            utc_iso_ms(ms - 1).ends_with("T23:59:59.999+00:00"),
            "{}",
            utc_iso_ms(ms - 1)
        );
        ms += day_ms;
        d += 1;
        if d > days_in_month(y, m) {
            d = 1;
            m += 1;
            if m > 12 {
                m = 1;
                y += 1;
            }
        }
    }
}

#[test]
fn a_sample_of_instants_matches_python_isoformat() {
    // (epoch ms, expected), from datetime.fromtimestamp(ms / 1000, tz=utc).
    let cases = [
        (1_700_000_000_123_i64, "2023-11-14T22:13:20.123+00:00"),
        (1_234_567_890_987, "2009-02-13T23:31:30.987+00:00"),
        (2_147_483_647_999, "2038-01-19T03:14:07.999+00:00"),
        (-62_135_596_800_000, "0001-01-01T00:00:00.000+00:00"),
    ];
    for (ms, expected) in cases {
        assert_eq!(utc_iso_ms(ms), expected, "ms {ms}");
    }
}
