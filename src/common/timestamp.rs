//! Timestamp normalization and interval handling utilities.
//!
//! `DuckDB` stores timestamps internally as `i64` microseconds since Unix epoch.
//! Intervals are stored as a struct with months, days, and microseconds components.
//! We normalize everything to microseconds for consistent comparison.

/// Microseconds per second.
pub const MICROS_PER_SECOND: i64 = 1_000_000;

/// Microseconds per day (`24 * 60 * 60 * 1_000_000`).
pub const MICROS_PER_DAY: i64 = 86_400_000_000;

/// Extracts the microseconds component from a `DuckDB` interval.
///
/// `DuckDB` intervals have three components: months, days, microseconds.
/// For behavioral analytics, we only support intervals expressible in exact
/// microseconds (days + micros). Month-based intervals are ambiguous (28-31 days)
/// and will cause this function to return `None`.
///
/// # Layout
///
/// `DuckDB`'s `duckdb_interval` C struct is:
/// ```c
/// typedef struct {
///     int32_t months;
///     int32_t days;
///     int64_t micros;
/// } duckdb_interval;
/// ```
///
/// In memory as bytes (16 bytes total):
/// - bytes 0..4: months (i32)
/// - bytes 4..8: days (i32)
/// - bytes 8..16: micros (i64)
#[must_use]
#[inline]
pub fn interval_to_micros(months: i32, days: i32, micros: i64) -> Option<i64> {
    if months != 0 {
        return None;
    }
    let day_micros = i64::from(days).checked_mul(MICROS_PER_DAY)?;
    day_micros.checked_add(micros)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interval_to_micros_basic() {
        // 30 minutes = 30 * 60 * 1_000_000 = 1_800_000_000 microseconds
        assert_eq!(interval_to_micros(0, 0, 1_800_000_000), Some(1_800_000_000));
    }

    #[test]
    fn test_interval_to_micros_days() {
        // 1 day = 86_400_000_000 microseconds
        assert_eq!(interval_to_micros(0, 1, 0), Some(MICROS_PER_DAY));
    }

    #[test]
    fn test_interval_to_micros_days_and_micros() {
        // 1 day + 1 hour
        let one_hour = 3_600_000_000_i64;
        assert_eq!(
            interval_to_micros(0, 1, one_hour),
            Some(MICROS_PER_DAY + one_hour)
        );
    }

    #[test]
    fn test_interval_to_micros_rejects_months() {
        assert_eq!(interval_to_micros(1, 0, 0), None);
        assert_eq!(interval_to_micros(-1, 0, 0), None);
        assert_eq!(interval_to_micros(12, 0, 0), None);
    }

    #[test]
    fn test_interval_to_micros_zero() {
        assert_eq!(interval_to_micros(0, 0, 0), Some(0));
    }

    #[test]
    fn test_interval_to_micros_negative() {
        assert_eq!(interval_to_micros(0, 0, -1_000_000), Some(-1_000_000));
    }

    #[test]
    fn test_interval_to_micros_overflow_days_max() {
        // i32::MAX days * MICROS_PER_DAY overflows i64
        assert_eq!(interval_to_micros(0, i32::MAX, 0), None);
    }

    #[test]
    fn test_interval_to_micros_overflow_days_min() {
        // i32::MIN days * MICROS_PER_DAY overflows i64
        assert_eq!(interval_to_micros(0, i32::MIN, 0), None);
    }

    #[test]
    fn test_interval_to_micros_overflow_addition() {
        // Large days + large micros overflows the addition
        let large_days = 100_000; // 100k days fits in checked_mul
        let day_micros = i64::from(large_days) * MICROS_PER_DAY;
        // Adding i64::MAX to day_micros must overflow
        assert_eq!(
            interval_to_micros(0, large_days, i64::MAX - day_micros + 1),
            None
        );
    }

    #[test]
    fn test_interval_to_micros_negative_days() {
        assert_eq!(interval_to_micros(0, -1, 0), Some(-MICROS_PER_DAY));
    }

    #[test]
    fn test_interval_to_micros_large_valid() {
        // 365 days should be fine
        let expected = 365 * MICROS_PER_DAY;
        assert_eq!(interval_to_micros(0, 365, 0), Some(expected));
    }
}
