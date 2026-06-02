//! Time-window scheduler — defer downloads outside allowed hours.

use chrono::{DateTime, Local, NaiveTime, Utc};

use crate::core::{AppSettings, DownloadEntry};

pub fn is_within_schedule(settings: &AppSettings) -> bool {
    if settings.schedule_windows.is_empty() {
        return true;
    }

    let now = Local::now().time();
    settings.schedule_windows.iter().any(|w| {
        let start = NaiveTime::from_hms_opt(w.start_hour.into(), w.start_minute.into(), 0)
            .unwrap_or(NaiveTime::MIN);
        let end = NaiveTime::from_hms_opt(w.end_hour.into(), w.end_minute.into(), 59)
            .unwrap_or_else(|| NaiveTime::from_hms_opt(23, 59, 59).unwrap());

        if start <= end {
            now >= start && now < end
        } else {
            // overnight window e.g. 22:00 → 06:00
            now >= start || now < end
        }
    })
}

pub fn is_entry_ready(entry: &DownloadEntry) -> bool {
    match &entry.scheduled_at {
        None => true,
        Some(ts) => DateTime::parse_from_rfc3339(ts)
            .map(|dt| dt.with_timezone(&Utc) <= Utc::now())
            .unwrap_or(true),
    }
}

pub fn next_schedule_wait(settings: &AppSettings) -> Option<std::time::Duration> {
    if is_within_schedule(settings) {
        return None;
    }
    // coarse poll — check again in 60s
    Some(std::time::Duration::from_secs(60))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ScheduleWindow;
    use chrono::{Duration as ChronoDuration, Utc};
    use std::path::PathBuf;

    #[test]
    fn test_empty_schedule_always_allowed() {
        assert!(is_within_schedule(&AppSettings::default()));
    }

    #[test]
    fn test_full_day_window_is_allowed() {
        let settings = AppSettings {
            schedule_windows: vec![ScheduleWindow {
                start_hour: 0,
                start_minute: 0,
                end_hour: 23,
                end_minute: 59,
            }],
            ..Default::default()
        };
        assert!(is_within_schedule(&settings));
    }

    #[test]
    fn test_entry_without_schedule_is_ready() {
        let entry = DownloadEntry::new_http(
            "1".into(),
            "http://example.com".into(),
            PathBuf::from("f.bin"),
        );
        assert!(is_entry_ready(&entry));
    }

    #[test]
    fn test_entry_with_past_schedule_is_ready() {
        let past = (Utc::now() - ChronoDuration::hours(1)).to_rfc3339();
        let mut entry = DownloadEntry::new_http("1".into(), "http://x.com".into(), PathBuf::from("a"));
        entry.scheduled_at = Some(past);
        assert!(is_entry_ready(&entry));
    }

    #[test]
    fn test_entry_with_future_schedule_not_ready() {
        let future = (Utc::now() + ChronoDuration::hours(2)).to_rfc3339();
        let mut entry = DownloadEntry::new_http("1".into(), "http://x.com".into(), PathBuf::from("a"));
        entry.scheduled_at = Some(future);
        assert!(!is_entry_ready(&entry));
    }

    #[test]
    fn test_next_wait_none_when_allowed() {
        assert!(next_schedule_wait(&AppSettings::default()).is_none());
    }
}
