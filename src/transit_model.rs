use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntityId(String);

impl EntityId {
    pub fn namespaced(operator_id: &str, source_id: &str) -> Self {
        Self(format!("{operator_id}:{source_id}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Seconds since the start of the service day. Trips that run past midnight
/// keep counting, so values above 86400 are valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServiceTime(u32);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("expected H:MM:SS with minutes and seconds below 60")]
pub struct ServiceTimeError;

impl ServiceTime {
    pub fn from_seconds(seconds: u32) -> Self {
        Self(seconds)
    }

    pub fn seconds(self) -> u32 {
        self.0
    }

    pub fn parse(text: &str) -> Result<Self, ServiceTimeError> {
        let mut parts = text.split(':');
        let (Some(hours), Some(minutes), Some(seconds), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(ServiceTimeError);
        };
        let hours = parse_digits(hours, 1..=3)?;
        let minutes = parse_digits(minutes, 2..=2)?;
        let seconds = parse_digits(seconds, 2..=2)?;
        if minutes >= 60 || seconds >= 60 {
            return Err(ServiceTimeError);
        }
        Ok(Self(hours * 3600 + minutes * 60 + seconds))
    }
}

fn parse_digits(
    text: &str,
    length: std::ops::RangeInclusive<usize>,
) -> Result<u32, ServiceTimeError> {
    if !length.contains(&text.len()) || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ServiceTimeError);
    }
    text.parse().map_err(|_| ServiceTimeError)
}

/// A service date held as its `YYYYMMDD` integer, which is the form the
/// bundle stores and also sorts chronologically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServiceDate(u32);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("expected a real calendar date written as YYYYMMDD")]
pub struct ServiceDateError;

impl ServiceDate {
    pub fn parse(text: &str) -> Result<Self, ServiceDateError> {
        let value = parse_digits(text, 8..=8).map_err(|_| ServiceDateError)?;
        let (year, month, day) = (value / 10_000, value / 100 % 100, value % 100);
        if (1..=12).contains(&month) && (1..=days_in_month(year, month)).contains(&day) {
            Ok(Self(value))
        } else {
            Err(ServiceDateError)
        }
    }

    pub fn yyyymmdd(self) -> u32 {
        self.0
    }
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn is_leap_year(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

impl fmt::Display for ServiceDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Operator {
    pub id: String,
    pub name: String,
    pub url: Option<String>,
    pub timezone: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stop {
    pub id: EntityId,
    pub code: Option<String>,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub location_type: u8,
    pub parent_station: Option<EntityId>,
    pub wheelchair_boarding: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub id: EntityId,
    pub operator_id: String,
    pub short_name: Option<String>,
    pub long_name: Option<String>,
    pub route_type: u16,
    pub color: Option<String>,
    pub text_color: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Weekdays {
    pub monday: bool,
    pub tuesday: bool,
    pub wednesday: bool,
    pub thursday: bool,
    pub friday: bool,
    pub saturday: bool,
    pub sunday: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateRange {
    pub start: ServiceDate,
    pub end: ServiceDate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub id: EntityId,
    pub weekdays: Weekdays,
    /// `None` for a service that runs only on dates its exceptions add.
    pub date_range: Option<DateRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExceptionType {
    Added,
    Removed,
}

impl ExceptionType {
    pub fn code(self) -> u8 {
        match self {
            Self::Added => 1,
            Self::Removed => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceException {
    pub service_id: EntityId,
    pub date: ServiceDate,
    pub exception_type: ExceptionType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Trip {
    pub id: EntityId,
    pub route_id: EntityId,
    pub service_id: EntityId,
    pub headsign: Option<String>,
    pub direction_id: Option<u8>,
    pub shape_id: Option<EntityId>,
    pub wheelchair_accessible: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StopTime {
    pub trip_id: EntityId,
    pub stop_sequence: u32,
    pub stop_id: EntityId,
    pub arrival: ServiceTime,
    pub departure: ServiceTime,
    pub pickup_type: u8,
    pub drop_off_type: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShapePoint {
    pub shape_id: EntityId,
    pub sequence: u32,
    pub lat: f64,
    pub lon: f64,
    pub dist_traveled: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Frequency {
    pub trip_id: EntityId,
    pub start: ServiceTime,
    pub end: ServiceTime,
    pub headway_secs: u32,
    pub exact_times: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fare {
    pub id: EntityId,
    pub price: f64,
    pub currency: String,
    pub payment_method: u8,
    /// `None` means unlimited transfers.
    pub transfers: Option<u8>,
    pub transfer_duration: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Transfer {
    pub from_stop_id: EntityId,
    pub to_stop_id: EntityId,
    pub transfer_type: u8,
    pub min_transfer_time: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dataset {
    pub operator: Operator,
    pub stops: Vec<Stop>,
    pub routes: Vec<Route>,
    pub services: Vec<Service>,
    pub service_exceptions: Vec<ServiceException>,
    pub trips: Vec<Trip>,
    pub stop_times: Vec<StopTime>,
    pub shapes: Vec<ShapePoint>,
    pub frequencies: Vec<Frequency>,
    pub fares: Vec<Fare>,
    pub transfers: Vec<Transfer>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_dates_keep_their_yyyymmdd_integer_and_order_chronologically() {
        let earlier = ServiceDate::parse("20261231").unwrap();
        let later = ServiceDate::parse("20270101").unwrap();

        assert_eq!(earlier.yyyymmdd(), 20_261_231);
        assert!(earlier < later);
        assert_eq!(earlier.to_string(), "20261231");
    }

    #[test]
    fn service_dates_accept_the_leap_day_only_in_leap_years() {
        assert!(ServiceDate::parse("20240229").is_ok());
        assert!(ServiceDate::parse("20000229").is_ok());
        assert_eq!(ServiceDate::parse("20260229"), Err(ServiceDateError));
        assert_eq!(ServiceDate::parse("19000229"), Err(ServiceDateError));
    }

    #[test]
    fn service_dates_reject_anything_but_a_real_eight_digit_date() {
        for text in [
            "",
            "20261301",
            "20260230",
            "20260431",
            "20260001",
            "20260100",
            "2026011",
            "202601011",
            "2026-01-01",
            "abcd1234",
            "+2026010",
            "٢٠٢٦٠١٠١",
        ] {
            assert_eq!(ServiceDate::parse(text), Err(ServiceDateError), "{text:?}");
        }
    }

    #[test]
    fn exception_types_map_to_the_codes_the_bundle_stores() {
        assert_eq!(ExceptionType::Added.code(), 1);
        assert_eq!(ExceptionType::Removed.code(), 2);
    }

    #[test]
    fn entity_ids_are_prefixed_with_the_operator_id() {
        assert_eq!(EntityId::namespaced("demo", "S1").as_str(), "demo:S1");
    }

    #[test]
    fn service_time_counts_seconds_from_the_start_of_the_service_day() {
        assert_eq!(
            ServiceTime::parse("08:05:09"),
            Ok(ServiceTime::from_seconds(29_109))
        );
    }

    #[test]
    fn service_time_accepts_a_single_digit_hour() {
        assert_eq!(
            ServiceTime::parse("5:00:00"),
            Ok(ServiceTime::from_seconds(18_000))
        );
    }

    #[test]
    fn service_time_keeps_counting_past_midnight() {
        assert_eq!(
            ServiceTime::parse("25:30:00"),
            Ok(ServiceTime::from_seconds(91_800))
        );
    }

    #[test]
    fn service_time_rejects_malformed_text() {
        for text in [
            "",
            "8:5:9",
            "08:60:00",
            "08:00:60",
            "08:00",
            "08:00:00:00",
            "aa:00:00",
            "-1:00:00",
        ] {
            assert_eq!(ServiceTime::parse(text), Err(ServiceTimeError), "{text:?}");
        }
    }
}
