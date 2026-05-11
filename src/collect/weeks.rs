//! ISO-week iteration over a date range.
//!
//! Used by the collection pipeline to split a `[from, to]` window into the
//! list of ISO weeks it overlaps, so that each week can be checked against
//! the `collection_runs` table independently and either skipped or
//! collected.

use chrono::{Datelike, Duration, NaiveDate};

/// Iterator item yielded by [`weeks_in_range`].
///
/// `(iso_year, iso_week, week_start, week_end)` — `week_start` is the Monday
/// of the ISO week and `week_end` is the following Sunday. Both endpoints
/// are inclusive.
pub type WeekTuple = (i32, u32, NaiveDate, NaiveDate);

/// Return the Monday on or before `d` (i.e. the start of the ISO week
/// containing `d`).
fn iso_week_monday(d: NaiveDate) -> NaiveDate {
    let dow = d.weekday().num_days_from_monday() as i64;
    d - Duration::days(dow)
}

/// Iterate over all ISO weeks that overlap the closed interval `[from, to]`.
///
/// Each tuple is `(iso_year, iso_week_number, monday_of_week,
/// sunday_of_week)`. The first tuple's `week_start` may be before `from`
/// (when `from` falls mid-week), and the last tuple's `week_end` may be
/// after `to` (when `to` falls mid-week). Returns an empty iterator if
/// `to < from`.
///
/// # Examples
///
/// ```
/// use chrono::NaiveDate;
/// use tga::collect::weeks::weeks_in_range;
///
/// let from = NaiveDate::from_ymd_opt(2026, 3, 9).expect("valid");
/// let to = NaiveDate::from_ymd_opt(2026, 3, 9).expect("valid");
/// let v: Vec<_> = weeks_in_range(from, to).collect();
/// assert_eq!(v.len(), 1);
/// ```
pub fn weeks_in_range(from: NaiveDate, to: NaiveDate) -> impl Iterator<Item = WeekTuple> {
    let mut out = Vec::new();
    if to < from {
        return out.into_iter();
    }
    let mut cursor = iso_week_monday(from);
    let final_monday = iso_week_monday(to);
    loop {
        let iso = cursor.iso_week();
        let week_start = cursor;
        let week_end = cursor + Duration::days(6);
        out.push((iso.year(), iso.week(), week_start, week_end));
        if cursor >= final_monday {
            break;
        }
        cursor += Duration::days(7);
    }
    out.into_iter()
}

/// Convenience: clamp a week to `[from, to]` for use as a collection window.
///
/// Given an ISO week tuple, returns `(window_start, window_end)` truncated
/// so that we don't accidentally collect commits outside the user-requested
/// range when the week partially overlaps the range boundary.
pub fn clamp_week_to_range(
    week: WeekTuple,
    from: NaiveDate,
    to: NaiveDate,
) -> (NaiveDate, NaiveDate) {
    let (_, _, ws, we) = week;
    let start = if ws < from { from } else { ws };
    let end = if we > to { to } else { we };
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Weekday;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).expect("valid date")
    }

    #[test]
    fn weeks_in_range_single_week() {
        // Monday → Wednesday of the same ISO week.
        let from = d(2026, 3, 9); // Monday
        let to = d(2026, 3, 11); // Wednesday
        let v: Vec<_> = weeks_in_range(from, to).collect();
        assert_eq!(
            v.len(),
            1,
            "single-week range should yield exactly one tuple"
        );
        let (year, week, ws, we) = v[0];
        assert_eq!(year, 2026);
        assert_eq!(week, 11);
        assert_eq!(ws.weekday(), Weekday::Mon);
        assert_eq!(we.weekday(), Weekday::Sun);
    }

    #[test]
    fn weeks_in_range_spans_multiple() {
        // W15 through W19 (5 weeks).
        let from = d(2026, 4, 6); // Monday of W15
        let to = d(2026, 5, 10); // Sunday of W19
        let v: Vec<_> = weeks_in_range(from, to).collect();
        assert_eq!(v.len(), 5);
        let weeks: Vec<u32> = v.iter().map(|t| t.1).collect();
        assert_eq!(weeks, vec![15, 16, 17, 18, 19]);
    }

    #[test]
    fn weeks_in_range_partial_week() {
        // Range starts on Wednesday — should still include that whole week.
        let from = d(2026, 3, 11); // Wednesday of W11
        let to = d(2026, 3, 20); // Friday of W12
        let v: Vec<_> = weeks_in_range(from, to).collect();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].1, 11);
        assert_eq!(v[1].1, 12);
        // First tuple's week_start is the Monday on/before `from`.
        assert_eq!(v[0].2, d(2026, 3, 9));
        // Last tuple's week_end is the Sunday on/after `to`.
        assert_eq!(v[1].3, d(2026, 3, 22));
    }

    #[test]
    fn weeks_in_range_inverted_returns_empty() {
        let v: Vec<_> = weeks_in_range(d(2026, 3, 11), d(2026, 3, 1)).collect();
        assert!(v.is_empty());
    }
}
