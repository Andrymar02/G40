use chrono::{Datelike, Days, Months, NaiveDate, TimeZone, Utc};
use chrono_tz::Europe::Rome;

use super::{ReportPeriod, TrackingError};

///  Risolve un periodo richiesto nei relativi estremi UTC [start, end)
pub fn resolve_report_period(
    period: ReportPeriod,
    now_utc: i64,
) -> Result<(i64, i64), TrackingError> {
    match period {
        ReportPeriod::Custom { start_utc, end_utc } if start_utc < end_utc => {
            Ok((start_utc, end_utc))
        }

        ReportPeriod::CurrentDay => current_day_bounds(now_utc),

        ReportPeriod::CurrentWeek => current_week_bounds(now_utc),

        ReportPeriod::CurrentMonth => current_month_bounds(now_utc),

        ReportPeriod::Custom { .. } => Err(TrackingError::InvalidPeriod),
    }
}

fn current_day_bounds(now_utc: i64) -> Result<(i64, i64), TrackingError> {
    let start_date = rome_date(now_utc)?;

    let end_date = start_date.succ_opt().ok_or(TrackingError::InvalidPeriod)?;

    Ok((rome_midnight_utc(start_date)?, rome_midnight_utc(end_date)?))
}

fn rome_midnight_utc(date: NaiveDate) -> Result<i64, TrackingError> {
    let midnight = date
        .and_hms_opt(0, 0, 0)
        .ok_or(TrackingError::InvalidPeriod)?;

    Rome.from_local_datetime(&midnight)
        .single()
        .map(|date_time| date_time.timestamp())
        .ok_or(TrackingError::InvalidPeriod)
}

fn current_week_bounds(now_utc: i64) -> Result<(i64, i64), TrackingError> {
    let current_date = rome_date(now_utc)?;

    let days_since_monday = current_date.weekday().num_days_from_monday() as u64;

    let start_date = current_date
        .checked_sub_days(Days::new(days_since_monday))
        .ok_or(TrackingError::InvalidPeriod)?;

    let end_date = start_date
        .checked_add_days(Days::new(7))
        .ok_or(TrackingError::InvalidPeriod)?;

    Ok((rome_midnight_utc(start_date)?, rome_midnight_utc(end_date)?))
}

fn rome_date(now_utc: i64) -> Result<NaiveDate, TrackingError> {
    Utc.timestamp_opt(now_utc, 0)
        .single()
        .map(|date_time| date_time.with_timezone(&Rome).date_naive())
        .ok_or(TrackingError::InvalidPeriod)
}

fn current_month_bounds(now_utc: i64) -> Result<(i64, i64), TrackingError> {
    let current_date = rome_date(now_utc)?;

    let start_date = NaiveDate::from_ymd_opt(current_date.year(), current_date.month(), 1)
        .ok_or(TrackingError::InvalidPeriod)?;

    let end_date = start_date
        .checked_add_months(Months::new(1))
        .ok_or(TrackingError::InvalidPeriod)?;

    Ok((rome_midnight_utc(start_date)?, rome_midnight_utc(end_date)?))
}

#[cfg(test)]
mod tests {
    use crate::tracking::{ReportPeriod, TrackingError, resolve_report_period};

    #[test]
    fn custom_period_preserves_valid_utc_bounds() {
        let period = ReportPeriod::Custom {
            start_utc: 100,
            end_utc: 200,
        };

        let bounds = resolve_report_period(period, 999).unwrap();

        assert_eq!(bounds, (100, 200));
    }

    #[test]
    fn custom_period_rejects_non_increasing_bounds() {
        for (start_utc, end_utc) in [(100, 100), (200, 100)] {
            let period = ReportPeriod::Custom { start_utc, end_utc };

            assert_eq!(
                resolve_report_period(period, 999),
                Err(TrackingError::InvalidPeriod)
            );
        }
    }

    #[test]
    fn current_day_uses_rome_local_midnights() {
        // 2026-01-15 12:00:00 UTC, cioè le 13:00 a Roma
        let now_utc = 1_768_478_400;

        let bounds = resolve_report_period(ReportPeriod::CurrentDay, now_utc).unwrap();

        assert_eq!(
            bounds,
            (
                1_768_431_600, // 2026-01-14 23:00:00 UTC
                1_768_518_000, // 2026-01-15 23:00:00 UTC
            )
        );
    }

    #[test]
    fn current_day_respects_spring_dst_transition() {
        // 2026-03-29 12:00:00 UTC.
        let now_utc = 1_774_785_600;

        let (start_utc, end_utc) =
            resolve_report_period(ReportPeriod::CurrentDay, now_utc).unwrap();

        assert_eq!(start_utc, 1_774_738_800);
        assert_eq!(end_utc, 1_774_821_600);
        assert_eq!(end_utc - start_utc, 23 * 60 * 60);
    }

    #[test]
    fn current_week_runs_from_monday_to_next_monday() {
        // Giovedì 2026-01-15 12:00:00 UTC
        let now_utc = 1_768_478_400;

        let bounds = resolve_report_period(ReportPeriod::CurrentWeek, now_utc).unwrap();

        assert_eq!(
            bounds,
            (
                1_768_172_400, // lunedì 12 gennaio, 00:00 a Roma
                1_768_777_200, // lunedì 19 gennaio, 00:00 a Roma
            )
        );
    }

    #[test]
    fn current_week_respects_spring_dst_transition() {
        // Mercoledì 2026-03-25 12:00:00 UTC.
        let now_utc = 1_774_440_000;

        let (start_utc, end_utc) =
            resolve_report_period(ReportPeriod::CurrentWeek, now_utc).unwrap();

        assert_eq!(start_utc, 1_774_220_400);
        assert_eq!(end_utc, 1_774_821_600);
        assert_eq!(end_utc - start_utc, 167 * 60 * 60);
    }

    #[test]
    fn current_month_handles_year_change() {
        // 2026-12-15 12:00:00 UTC.
        let now_utc = 1_797_336_000;

        let bounds = resolve_report_period(ReportPeriod::CurrentMonth, now_utc).unwrap();

        assert_eq!(
            bounds,
            (
                1_796_079_600, // 1 dicembre 2026, 00:00 a Roma
                1_798_758_000, // 1 gennaio 2027, 00:00 a Roma
            )
        );
    }

    #[test]
    fn current_month_respects_spring_dst_transition() {
        // 2026-03-15 12:00:00 UTC.
        let now_utc = 1_773_576_000;

        let (start_utc, end_utc) =
            resolve_report_period(ReportPeriod::CurrentMonth, now_utc).unwrap();

        assert_eq!(start_utc, 1_772_319_600);
        assert_eq!(end_utc, 1_774_994_400);
        assert_eq!(end_utc - start_utc, 743 * 60 * 60);
    }

    #[test]
    fn current_day_respects_autumn_dst_transition() {
        // 2026-10-25 12:00:00 UTC.
        let now_utc = 1_792_929_600;

        let (start_utc, end_utc) =
            resolve_report_period(ReportPeriod::CurrentDay, now_utc).unwrap();

        assert_eq!(start_utc, 1_792_879_200);
        assert_eq!(end_utc, 1_792_969_200);
        assert_eq!(end_utc - start_utc, 25 * 60 * 60);
    }
}
