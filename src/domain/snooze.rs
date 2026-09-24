use chrono::{DateTime, Days, Duration, TimeZone, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnoozeChoice {
    For { raw: String, duration: Duration },
    Tomorrow,
}

impl SnoozeChoice {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.eq_ignore_ascii_case("tomorrow") {
            return Ok(SnoozeChoice::Tomorrow);
        }
        let std = humantime::parse_duration(raw).map_err(|e| format!("invalid snooze choice {raw:?}: {e}"))?;
        let duration = Duration::from_std(std).map_err(|e| e.to_string())?;
        Ok(SnoozeChoice::For {
            raw: raw.to_string(),
            duration,
        })
    }

    pub fn label(&self, tomorrow_hour: u32) -> String {
        match self {
            SnoozeChoice::For { raw, .. } => raw.clone(),
            SnoozeChoice::Tomorrow => format!("Tomorrow {tomorrow_hour:02}:00"),
        }
    }

    pub fn until<Tz: TimeZone>(&self, now: DateTime<Utc>, tz: &Tz, tomorrow_hour: u32) -> DateTime<Utc> {
        match self {
            SnoozeChoice::For { duration, .. } => now + *duration,
            SnoozeChoice::Tomorrow => {
                let date = now.with_timezone(tz).date_naive() + Days::new(1);
                let naive = date
                    .and_hms_opt(tomorrow_hour, 0, 0)
                    .expect("tomorrow_hour is validated to 0..=23");
                tz.from_local_datetime(&naive)
                    .earliest()
                    .or_else(|| tz.from_local_datetime(&(naive + Duration::hours(1))).earliest())
                    .expect("a local time exists within an hour of any DST gap")
                    .with_timezone(&Utc)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    #[test]
    fn parses_durations_and_tomorrow() {
        assert_eq!(
            SnoozeChoice::parse("15m").unwrap(),
            SnoozeChoice::For {
                raw: "15m".into(),
                duration: Duration::minutes(15)
            }
        );
        assert_eq!(SnoozeChoice::parse("Tomorrow").unwrap(), SnoozeChoice::Tomorrow);
        assert!(SnoozeChoice::parse("soon").is_err());
    }

    #[test]
    fn labels() {
        assert_eq!(SnoozeChoice::parse("1h").unwrap().label(9), "1h");
        assert_eq!(SnoozeChoice::Tomorrow.label(9), "Tomorrow 09:00");
    }

    #[test]
    fn duration_until_adds_to_now() {
        let now = Utc.with_ymd_and_hms(2026, 9, 21, 10, 0, 0).unwrap();
        let tz = FixedOffset::east_opt(0).unwrap();
        let until = SnoozeChoice::parse("1h").unwrap().until(now, &tz, 9);
        assert_eq!(until, Utc.with_ymd_and_hms(2026, 9, 21, 11, 0, 0).unwrap());
    }

    #[test]
    fn tomorrow_is_next_local_calendar_day_at_hour() {
        let now = Utc.with_ymd_and_hms(2026, 9, 21, 23, 30, 0).unwrap();
        let berlin_summer = FixedOffset::east_opt(2 * 3600).unwrap();
        let until = SnoozeChoice::Tomorrow.until(now, &berlin_summer, 9);
        assert_eq!(until, Utc.with_ymd_and_hms(2026, 9, 23, 7, 0, 0).unwrap());
    }

    #[test]
    fn tomorrow_skips_a_spring_forward_gap() {
        let berlin = chrono_tz::Europe::Berlin;
        let now = Utc.with_ymd_and_hms(2027, 3, 27, 9, 0, 0).unwrap();
        let until = SnoozeChoice::Tomorrow.until(now, &berlin, 2);
        assert_eq!(until, Utc.with_ymd_and_hms(2027, 3, 28, 1, 0, 0).unwrap());
    }

    #[test]
    fn tomorrow_takes_the_earlier_of_an_ambiguous_hour() {
        let berlin = chrono_tz::Europe::Berlin;
        let now = Utc.with_ymd_and_hms(2026, 10, 24, 8, 0, 0).unwrap();
        let until = SnoozeChoice::Tomorrow.until(now, &berlin, 2);
        assert_eq!(until, Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap());
    }
}
