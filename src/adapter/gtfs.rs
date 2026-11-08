mod interpolation;
mod table;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::Hash;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use zip::ZipArchive;
use zip::result::ZipError;

use self::interpolation::{GeoPoint, StopTimeRow, Timing, fill_blank_times};
use self::table::{Row, parse_rows};
use super::{AdapterError, OperatorAdapter};
use crate::transit_model::{
    Dataset, DateRange, EntityId, ExceptionType, Fare, Frequency, Operator, Route, Service,
    ServiceDate, ServiceException, ServiceTime, ShapePoint, Stop, Transfer, Trip, Weekdays,
};

const AGENCY: &str = "agency.txt";
const STOPS: &str = "stops.txt";
const ROUTES: &str = "routes.txt";
const CALENDAR: &str = "calendar.txt";
const CALENDAR_DATES: &str = "calendar_dates.txt";
const SERVICE_DEFINITIONS: &str = "calendar.txt or calendar_dates.txt";
const TRIPS: &str = "trips.txt";
const STOP_TIMES: &str = "stop_times.txt";
const SHAPES: &str = "shapes.txt";
const FREQUENCIES: &str = "frequencies.txt";
const FARE_ATTRIBUTES: &str = "fare_attributes.txt";
const TRANSFERS: &str = "transfers.txt";

const LOCATION_TYPES: std::ops::RangeInclusive<u8> = 0..=4;

/// Reads a standard GTFS Schedule zip. Files outside the standard set this
/// adapter knows are ignored.
pub struct GtfsScheduleAdapter {
    operator_id: String,
}

impl GtfsScheduleAdapter {
    pub fn new(operator_id: impl Into<String>) -> Self {
        Self {
            operator_id: operator_id.into(),
        }
    }

    pub fn load_archive<R: Read + Seek>(&self, archive: R) -> Result<Dataset, AdapterError> {
        let mut archive = ZipArchive::new(archive)?;

        let operator = required(
            AGENCY,
            parse_file(&mut archive, AGENCY, |row| self.operator(row))?,
        )?
        .into_iter()
        .next()
        .ok_or(AdapterError::EmptyFile { file: AGENCY })?;
        let stops = self.stops(&mut archive)?;
        let stop_ids = ids(&stops, |stop| &stop.id);
        let routes = required(
            ROUTES,
            parse_unique(
                &mut archive,
                ROUTES,
                &["route_id"],
                |route: &Route| route.id.clone(),
                |row| self.route(row),
            )?,
        )?;
        let route_ids = ids(&routes, |route| &route.id);
        let (services, service_exceptions) = self.services(&mut archive)?;
        let service_ids = ids(&services, |service| &service.id);
        let trips = required(
            TRIPS,
            parse_unique(
                &mut archive,
                TRIPS,
                &["trip_id"],
                |trip: &Trip| trip.id.clone(),
                |row| self.trip(row, &route_ids, &service_ids),
            )?,
        )?;
        let trip_ids = ids(&trips, |trip| &trip.id);
        let stop_positions = positions(&stops);
        let stop_times = fill_blank_times(required(
            STOP_TIMES,
            parse_unique(
                &mut archive,
                STOP_TIMES,
                &["trip_id", "stop_sequence"],
                |stop_time: &StopTimeRow| (stop_time.trip_id.clone(), stop_time.stop_sequence),
                |row| self.stop_time(row, &trip_ids, &stop_positions),
            )?,
        )?)?;
        let shapes = parse_unique(
            &mut archive,
            SHAPES,
            &["shape_id", "shape_pt_sequence"],
            |point: &ShapePoint| (point.shape_id.clone(), point.sequence),
            |row| self.shape_point(row),
        )?
        .unwrap_or_default();
        let frequencies = parse_unique(
            &mut archive,
            FREQUENCIES,
            &["trip_id", "start_time"],
            |frequency: &Frequency| (frequency.trip_id.clone(), frequency.start),
            |row| self.frequency(row, &trip_ids),
        )?
        .unwrap_or_default();
        let fares = parse_unique(
            &mut archive,
            FARE_ATTRIBUTES,
            &["fare_id"],
            |fare: &Fare| fare.id.clone(),
            |row| self.fare(row),
        )?
        .unwrap_or_default();
        let transfers = parse_unique(
            &mut archive,
            TRANSFERS,
            &["from_stop_id", "to_stop_id"],
            |transfer: &Transfer| (transfer.from_stop_id.clone(), transfer.to_stop_id.clone()),
            |row| self.transfer(row, &stop_ids),
        )?
        .unwrap_or_default();

        Ok(Dataset {
            operator,
            stops,
            routes,
            services,
            service_exceptions,
            trips,
            stop_times,
            shapes,
            frequencies,
            fares,
            transfers,
        })
    }

    fn id(&self, source_id: &str) -> EntityId {
        EntityId::namespaced(&self.operator_id, source_id)
    }

    fn reference(
        &self,
        row: &Row,
        field: &'static str,
        known: &HashSet<EntityId>,
        target: &'static str,
    ) -> Result<EntityId, AdapterError> {
        let source_id = row.required(field)?;
        let id = self.id(source_id);
        row.reference(field, &id, source_id, known, target)?;
        Ok(id)
    }

    fn operator(&self, row: &Row) -> Result<Operator, AdapterError> {
        Ok(Operator {
            id: self.operator_id.clone(),
            name: row.required("agency_name")?.to_owned(),
            url: row.optional("agency_url").map(str::to_owned),
            timezone: row.required("agency_timezone")?.to_owned(),
        })
    }

    /// Parent stations may appear after their children, so references to
    /// them are checked once the whole file is read.
    fn stops<R: Read + Seek>(
        &self,
        archive: &mut ZipArchive<R>,
    ) -> Result<Vec<Stop>, AdapterError> {
        let mut links = Vec::new();
        let stops = required(
            STOPS,
            parse_unique(
                archive,
                STOPS,
                &["stop_id"],
                |stop: &Stop| stop.id.clone(),
                |row| {
                    let stop = self.stop(row)?;
                    if let Some(parent) = row.optional("parent_station") {
                        links.push(ParentLink {
                            line: row.line(),
                            stop: row.required("stop_id")?.to_owned(),
                            parent: parent.to_owned(),
                        });
                    }
                    Ok(stop)
                },
            )?,
        )?;
        let stop_ids = ids(&stops, |stop| &stop.id);
        if let Some(link) = links
            .iter()
            .find(|link| !stop_ids.contains(&self.id(&link.parent)))
        {
            return Err(AdapterError::UnknownReference {
                file: STOPS,
                line: link.line,
                field: "parent_station",
                value: link.parent.clone(),
                target: STOPS,
            });
        }
        if let Some(link) = find_parent_cycle(&links) {
            return Err(AdapterError::ParentCycle {
                file: STOPS,
                line: link.line,
                stop_id: link.stop.clone(),
            });
        }
        Ok(stops)
    }

    fn stop(&self, row: &Row) -> Result<Stop, AdapterError> {
        let location_type = row.parse_or("location_type", 0)?;
        if !LOCATION_TYPES.contains(&location_type) {
            return Err(row.invalid("location_type", &location_type.to_string()));
        }
        Ok(Stop {
            id: self.id(row.required("stop_id")?),
            code: row.optional("stop_code").map(str::to_owned),
            name: row.required("stop_name")?.to_owned(),
            lat: coordinate(row, "stop_lat", 90.0)?,
            lon: coordinate(row, "stop_lon", 180.0)?,
            location_type,
            parent_station: row.optional("parent_station").map(|parent| self.id(parent)),
            wheelchair_boarding: row.parse_or("wheelchair_boarding", 0)?,
        })
    }

    fn route(&self, row: &Row) -> Result<Route, AdapterError> {
        Ok(Route {
            id: self.id(row.required("route_id")?),
            operator_id: self.operator_id.clone(),
            short_name: row.optional("route_short_name").map(str::to_owned),
            long_name: row.optional("route_long_name").map(str::to_owned),
            route_type: row.parse_required("route_type")?,
            color: row.optional("route_color").map(str::to_owned),
            text_color: row.optional("route_text_color").map(str::to_owned),
        })
    }

    /// A service named only by exceptions still needs a row of its own, since
    /// trips reference one table of services.
    fn services<R: Read + Seek>(
        &self,
        archive: &mut ZipArchive<R>,
    ) -> Result<(Vec<Service>, Vec<ServiceException>), AdapterError> {
        let weekly = parse_unique(
            archive,
            CALENDAR,
            &["service_id"],
            |service: &Service| service.id.clone(),
            |row| self.weekly_service(row),
        )?;
        let exceptions = parse_unique(
            archive,
            CALENDAR_DATES,
            &["service_id", "date"],
            |exception: &ServiceException| (exception.service_id.clone(), exception.date),
            |row| self.service_exception(row),
        )?;
        if weekly.is_none() && exceptions.is_none() {
            return Err(AdapterError::MissingEveryFile {
                files: [CALENDAR, CALENDAR_DATES],
            });
        }

        let mut services = weekly.unwrap_or_default();
        let exceptions = exceptions.unwrap_or_default();
        let mut known = ids(&services, |service| &service.id);
        for exception in &exceptions {
            if known.insert(exception.service_id.clone()) {
                services.push(Service {
                    id: exception.service_id.clone(),
                    weekdays: Weekdays::default(),
                    date_range: None,
                });
            }
        }
        Ok((services, exceptions))
    }

    fn weekly_service(&self, row: &Row) -> Result<Service, AdapterError> {
        let start = required_date(row, "start_date")?;
        let end = required_date(row, "end_date")?;
        if start > end {
            return Err(row.reversed_date_range(start, end));
        }
        Ok(Service {
            id: self.id(row.required("service_id")?),
            weekdays: Weekdays {
                monday: weekday(row, "monday")?,
                tuesday: weekday(row, "tuesday")?,
                wednesday: weekday(row, "wednesday")?,
                thursday: weekday(row, "thursday")?,
                friday: weekday(row, "friday")?,
                saturday: weekday(row, "saturday")?,
                sunday: weekday(row, "sunday")?,
            },
            date_range: Some(DateRange { start, end }),
        })
    }

    fn service_exception(&self, row: &Row) -> Result<ServiceException, AdapterError> {
        let exception_type = match row.required("exception_type")? {
            "1" => ExceptionType::Added,
            "2" => ExceptionType::Removed,
            other => return Err(row.invalid("exception_type", other)),
        };
        Ok(ServiceException {
            service_id: self.id(row.required("service_id")?),
            date: required_date(row, "date")?,
            exception_type,
        })
    }

    fn trip(
        &self,
        row: &Row,
        route_ids: &HashSet<EntityId>,
        service_ids: &HashSet<EntityId>,
    ) -> Result<Trip, AdapterError> {
        Ok(Trip {
            id: self.id(row.required("trip_id")?),
            route_id: self.reference(row, "route_id", route_ids, ROUTES)?,
            service_id: self.reference(row, "service_id", service_ids, SERVICE_DEFINITIONS)?,
            headsign: row.optional("trip_headsign").map(str::to_owned),
            direction_id: row.parse_optional("direction_id")?,
            shape_id: row.optional("shape_id").map(|shape| self.id(shape)),
            wheelchair_accessible: row.parse_or("wheelchair_accessible", 0)?,
        })
    }

    /// GTFS lets either time be blank when the other is given. A row with
    /// neither keeps no timing here and is filled in once its trip is known.
    fn stop_time(
        &self,
        row: &Row,
        trip_ids: &HashSet<EntityId>,
        stop_positions: &HashMap<EntityId, GeoPoint>,
    ) -> Result<StopTimeRow, AdapterError> {
        let arrival = optional_time(row, "arrival_time")?;
        let departure = optional_time(row, "departure_time")?;
        let timing = match (arrival, departure) {
            (Some(arrival), Some(departure)) if arrival > departure => {
                return Err(AdapterError::ArrivalAfterDeparture {
                    file: STOP_TIMES,
                    line: row.line(),
                });
            }
            (Some(arrival), Some(departure)) => Some(Timing { arrival, departure }),
            (Some(time), None) | (None, Some(time)) => Some(Timing {
                arrival: time,
                departure: time,
            }),
            (None, None) => None,
        };
        let trip_id = self.reference(row, "trip_id", trip_ids, TRIPS)?;
        let stop_sequence = row.parse_required("stop_sequence")?;
        let source_stop_id = row.required("stop_id")?;
        let stop_id = self.id(source_stop_id);
        let position = *stop_positions
            .get(&stop_id)
            .ok_or_else(|| row.unknown_reference("stop_id", source_stop_id, STOPS))?;
        Ok(StopTimeRow {
            line: row.line(),
            trip_id,
            stop_sequence,
            stop_id,
            position,
            timing,
            pickup_type: row.parse_or("pickup_type", 0)?,
            drop_off_type: row.parse_or("drop_off_type", 0)?,
        })
    }

    fn shape_point(&self, row: &Row) -> Result<ShapePoint, AdapterError> {
        Ok(ShapePoint {
            shape_id: self.id(row.required("shape_id")?),
            sequence: row.parse_required("shape_pt_sequence")?,
            lat: coordinate(row, "shape_pt_lat", 90.0)?,
            lon: coordinate(row, "shape_pt_lon", 180.0)?,
            dist_traveled: optional_finite(row, "shape_dist_traveled")?,
        })
    }

    fn frequency(
        &self,
        row: &Row,
        trip_ids: &HashSet<EntityId>,
    ) -> Result<Frequency, AdapterError> {
        Ok(Frequency {
            trip_id: self.reference(row, "trip_id", trip_ids, TRIPS)?,
            start: required_time(row, "start_time")?,
            end: required_time(row, "end_time")?,
            headway_secs: row.parse_required("headway_secs")?,
            exact_times: row.parse_or("exact_times", 0)?,
        })
    }

    fn fare(&self, row: &Row) -> Result<Fare, AdapterError> {
        Ok(Fare {
            id: self.id(row.required("fare_id")?),
            price: required_finite(row, "price")?,
            currency: row.required("currency_type")?.to_owned(),
            payment_method: row.parse_required("payment_method")?,
            transfers: row.parse_optional("transfers")?,
            transfer_duration: row.parse_optional("transfer_duration")?,
        })
    }

    fn transfer(&self, row: &Row, stop_ids: &HashSet<EntityId>) -> Result<Transfer, AdapterError> {
        Ok(Transfer {
            from_stop_id: self.reference(row, "from_stop_id", stop_ids, STOPS)?,
            to_stop_id: self.reference(row, "to_stop_id", stop_ids, STOPS)?,
            transfer_type: row.parse_or("transfer_type", 0)?,
            min_transfer_time: row.parse_optional("min_transfer_time")?,
        })
    }
}

impl OperatorAdapter for GtfsScheduleAdapter {
    fn operator_id(&self) -> &str {
        &self.operator_id
    }

    fn load_static(&self, source: &Path) -> Result<Dataset, AdapterError> {
        let file = File::open(source).map_err(|source_error| AdapterError::Io {
            path: source.to_owned(),
            source: source_error,
        })?;
        self.load_archive(BufReader::new(file))
    }
}

fn parse_file<R: Read + Seek, T>(
    archive: &mut ZipArchive<R>,
    file: &'static str,
    parse: impl FnMut(&Row) -> Result<T, AdapterError>,
) -> Result<Option<Vec<T>>, AdapterError> {
    match archive.by_name(file) {
        Ok(entry) => parse_rows(file, entry, parse).map(Some),
        Err(ZipError::FileNotFound) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Keys compare by parsed value, as the database does, so `1` and `01` in a
/// sequence column are the same key.
fn parse_unique<R: Read + Seek, T, K: Eq + Hash>(
    archive: &mut ZipArchive<R>,
    file: &'static str,
    key_fields: &'static [&'static str],
    key: impl Fn(&T) -> K,
    mut parse: impl FnMut(&Row) -> Result<T, AdapterError>,
) -> Result<Option<Vec<T>>, AdapterError> {
    let mut seen = HashSet::new();
    parse_file(archive, file, |row| {
        let item = parse(row)?;
        if seen.insert(key(&item)) {
            Ok(item)
        } else {
            Err(row.duplicate(key_fields))
        }
    })
}

struct ParentLink {
    line: u64,
    stop: String,
    parent: String,
}

/// Returns the link at which a walk up the parents first revisits a stop, so
/// the reported stop is inside the cycle rather than merely leading into it.
fn find_parent_cycle(links: &[ParentLink]) -> Option<&ParentLink> {
    let by_stop: HashMap<&str, &ParentLink> = links
        .iter()
        .map(|link| (link.stop.as_str(), link))
        .collect();
    let mut cleared = HashSet::new();
    for link in links {
        let mut path = HashSet::new();
        let mut current = Some(link);
        while let Some(step) = current {
            let stop = step.stop.as_str();
            if cleared.contains(stop) {
                break;
            }
            if !path.insert(stop) {
                return Some(step);
            }
            current = by_stop.get(step.parent.as_str()).copied();
        }
        cleared.extend(path);
    }
    None
}

fn required<T>(file: &'static str, rows: Option<Vec<T>>) -> Result<Vec<T>, AdapterError> {
    rows.ok_or(AdapterError::MissingFile { file })
}

fn ids<T>(items: &[T], id: impl Fn(&T) -> &EntityId) -> HashSet<EntityId> {
    items.iter().map(|item| id(item).clone()).collect()
}

fn positions(stops: &[Stop]) -> HashMap<EntityId, GeoPoint> {
    stops
        .iter()
        .map(|stop| {
            let position = GeoPoint {
                lat: stop.lat,
                lon: stop.lon,
            };
            (stop.id.clone(), position)
        })
        .collect()
}

fn coordinate(row: &Row, field: &'static str, limit: f64) -> Result<f64, AdapterError> {
    let value = required_finite(row, field)?;
    if value.abs() <= limit {
        Ok(value)
    } else {
        Err(row.invalid(field, row.required(field)?))
    }
}

fn required_finite(row: &Row, field: &'static str) -> Result<f64, AdapterError> {
    let value: f64 = row.parse_required(field)?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(row.invalid(field, row.required(field)?))
    }
}

fn optional_finite(row: &Row, field: &'static str) -> Result<Option<f64>, AdapterError> {
    match row.optional(field) {
        Some(_) => required_finite(row, field).map(Some),
        None => Ok(None),
    }
}

fn optional_time(row: &Row, field: &'static str) -> Result<Option<ServiceTime>, AdapterError> {
    row.optional(field)
        .map(|text| ServiceTime::parse(text).map_err(|_| row.invalid(field, text)))
        .transpose()
}

fn required_time(row: &Row, field: &'static str) -> Result<ServiceTime, AdapterError> {
    let text = row.required(field)?;
    ServiceTime::parse(text).map_err(|_| row.invalid(field, text))
}

fn required_date(row: &Row, field: &'static str) -> Result<ServiceDate, AdapterError> {
    let text = row.required(field)?;
    ServiceDate::parse(text).map_err(|_| row.invalid(field, text))
}

fn weekday(row: &Row, field: &'static str) -> Result<bool, AdapterError> {
    match row.required(field)? {
        "0" => Ok(false),
        "1" => Ok(true),
        other => Err(row.invalid(field, other)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{Cursor, Write};

    use zip::write::SimpleFileOptions;

    use super::*;
    use crate::transit_model::{
        DateRange, EntityId, ExceptionType, Service, ServiceDate, ServiceException, ServiceTime,
        Weekdays,
    };

    const AGENCY: &str = "agency_id,agency_name,agency_url,agency_timezone\n\
        A1,Demo Transit,https://transit.example,Etc/UTC\n";
    const STOPS: &str = "stop_id,stop_code,stop_name,stop_lat,stop_lon,location_type,parent_station,wheelchair_boarding\n\
        STN,,Central,1.5,103.5,1,,1\n\
        S1,C1,Central Platform 1,1.5001,103.5001,0,STN,\n\
        S2,,Riverside,1.6,103.6,,,\n";
    const ROUTES: &str = "route_id,agency_id,route_short_name,route_long_name,route_type,route_color,route_text_color\n\
        R1,A1,1,Central to Riverside,3,FF0000,FFFFFF\n";
    const TRIPS: &str = "route_id,service_id,trip_id,trip_headsign,direction_id,shape_id,wheelchair_accessible\n\
        R1,WK,T1,Riverside,0,SH1,1\n";
    const STOP_TIMES: &str = "trip_id,arrival_time,departure_time,stop_id,stop_sequence,pickup_type,drop_off_type\n\
        T1,23:50:00,23:51:00,S1,1,,\n\
        T1,24:10:30,24:10:30,S2,2,1,0\n";
    const CALENDAR_HEADER: &str =
        "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n";
    const CALENDAR: &str = "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n\
        WK,1,1,1,1,1,0,0,20260101,20261231\n";
    const CALENDAR_DATES_HEADER: &str = "service_id,date,exception_type\n";

    struct Feed(BTreeMap<&'static str, String>);

    impl Feed {
        fn minimal() -> Self {
            Self(BTreeMap::from([
                ("agency.txt", AGENCY.to_owned()),
                ("stops.txt", STOPS.to_owned()),
                ("routes.txt", ROUTES.to_owned()),
                ("trips.txt", TRIPS.to_owned()),
                ("stop_times.txt", STOP_TIMES.to_owned()),
                ("calendar.txt", CALENDAR.to_owned()),
            ]))
        }

        fn with(mut self, file: &'static str, contents: &str) -> Self {
            self.0.insert(file, contents.to_owned());
            self
        }

        fn without(mut self, file: &'static str) -> Self {
            self.0.remove(file);
            self
        }

        fn zip(&self) -> Cursor<Vec<u8>> {
            let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
            for (name, contents) in &self.0 {
                writer
                    .start_file(*name, SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(contents.as_bytes()).unwrap();
            }
            let mut archive = writer.finish().unwrap();
            archive.set_position(0);
            archive
        }

        fn load(&self) -> Result<Dataset, AdapterError> {
            GtfsScheduleAdapter::new("demo").load_archive(self.zip())
        }
    }

    fn id(source_id: &str) -> EntityId {
        EntityId::namespaced("demo", source_id)
    }

    fn date(text: &str) -> ServiceDate {
        ServiceDate::parse(text).unwrap()
    }

    fn assert_invalid_value(
        error: &AdapterError,
        expected_file: &str,
        expected_line: u64,
        expected_field: &str,
        expected_value: &str,
    ) {
        assert!(
            matches!(
                error,
                AdapterError::InvalidValue { file, line, field, value }
                    if *file == expected_file
                        && *line == expected_line
                        && *field == expected_field
                        && value == expected_value
            ),
            "{error}"
        );
    }

    #[test]
    fn weekly_services_are_namespaced_with_their_weekdays_and_date_range() {
        let calendar = format!("{CALENDAR_HEADER}WK,1,0,1,0,1,0,1,20260101,20261231\n");
        let dataset = Feed::minimal()
            .with("calendar.txt", &calendar)
            .load()
            .unwrap();

        assert_eq!(
            dataset.services,
            [Service {
                id: id("WK"),
                weekdays: Weekdays {
                    monday: true,
                    tuesday: false,
                    wednesday: true,
                    thursday: false,
                    friday: true,
                    saturday: false,
                    sunday: true,
                },
                date_range: Some(DateRange {
                    start: date("20260101"),
                    end: date("20261231"),
                }),
            }]
        );
    }

    #[test]
    fn a_service_may_run_on_a_single_day() {
        let calendar = format!("{CALENDAR_HEADER}WK,1,0,0,0,0,0,0,20260105,20260105\n");
        let dataset = Feed::minimal()
            .with("calendar.txt", &calendar)
            .load()
            .unwrap();

        assert_eq!(
            dataset.services[0].date_range,
            Some(DateRange {
                start: date("20260105"),
                end: date("20260105"),
            })
        );
    }

    #[test]
    fn calendar_dates_become_exceptions_of_a_weekly_service() {
        let exceptions = format!("{CALENDAR_DATES_HEADER}WK,20260704,1\nWK,20260703,2\n");
        let dataset = Feed::minimal()
            .with("calendar_dates.txt", &exceptions)
            .load()
            .unwrap();

        assert_eq!(dataset.services.len(), 1);
        assert_eq!(
            dataset.service_exceptions,
            [
                ServiceException {
                    service_id: id("WK"),
                    date: date("20260704"),
                    exception_type: ExceptionType::Added,
                },
                ServiceException {
                    service_id: id("WK"),
                    date: date("20260703"),
                    exception_type: ExceptionType::Removed,
                },
            ]
        );
    }

    #[test]
    fn a_service_only_in_calendar_dates_has_no_weekly_pattern() {
        let exceptions =
            format!("{CALENDAR_DATES_HEADER}EVENT,20260815,1\nEVENT,20260816,1\nWK,20260703,2\n");
        let trips = "route_id,service_id,trip_id\nR1,EVENT,T1\n";
        let dataset = Feed::minimal()
            .with("calendar_dates.txt", &exceptions)
            .with("trips.txt", trips)
            .load()
            .unwrap();

        assert_eq!(dataset.services.len(), 2);
        assert_eq!(dataset.services[0].id, id("WK"));
        assert_eq!(
            dataset.services[1],
            Service {
                id: id("EVENT"),
                weekdays: Weekdays::default(),
                date_range: None,
            }
        );
    }

    #[test]
    fn calendar_dates_alone_are_enough() {
        let exceptions = format!("{CALENDAR_DATES_HEADER}WK,20260815,1\n");
        let dataset = Feed::minimal()
            .without("calendar.txt")
            .with("calendar_dates.txt", &exceptions)
            .load()
            .unwrap();

        assert_eq!(dataset.services[0].id, id("WK"));
        assert_eq!(dataset.service_exceptions.len(), 1);
    }

    #[test]
    fn a_feed_with_neither_calendar_file_is_rejected() {
        let error = Feed::minimal().without("calendar.txt").load().unwrap_err();

        assert!(
            matches!(
                error,
                AdapterError::MissingEveryFile {
                    files: ["calendar.txt", "calendar_dates.txt"]
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn an_empty_calendar_file_counts_as_having_that_file() {
        let trips = "route_id,service_id,trip_id\nR1,EVENT,T1\n";
        let exceptions = format!("{CALENDAR_DATES_HEADER}EVENT,20260815,1\n");
        for empty in ["", CALENDAR_HEADER] {
            let dataset = Feed::minimal()
                .with("calendar.txt", empty)
                .with("calendar_dates.txt", &exceptions)
                .with("trips.txt", trips)
                .load()
                .unwrap();

            assert_eq!(dataset.services.len(), 1, "{empty:?}");
        }
    }

    #[test]
    fn a_feed_whose_only_calendar_is_empty_fails_on_the_trip_not_on_the_file() {
        for (file, empty) in [
            ("calendar.txt", ""),
            ("calendar.txt", CALENDAR_HEADER),
            ("calendar_dates.txt", ""),
            ("calendar_dates.txt", CALENDAR_DATES_HEADER),
        ] {
            let error = Feed::minimal()
                .without("calendar.txt")
                .with(file, empty)
                .load()
                .unwrap_err();

            assert!(
                matches!(
                    &error,
                    AdapterError::UnknownReference { file: "trips.txt", line: 2, field: "service_id", value, .. }
                        if value == "WK"
                ),
                "{file}: {error}"
            );
        }
    }

    #[test]
    fn a_trip_whose_service_is_in_neither_calendar_file_is_rejected() {
        let trips = "route_id,service_id,trip_id\nR1,WK,T1\nR1,NOPE,T2\n";
        let exceptions = format!("{CALENDAR_DATES_HEADER}WK,20260815,1\n");
        let error = Feed::minimal()
            .with("trips.txt", trips)
            .with("calendar_dates.txt", &exceptions)
            .load()
            .unwrap_err();

        assert!(
            matches!(
                &error,
                AdapterError::UnknownReference {
                    file: "trips.txt",
                    line: 3,
                    field: "service_id",
                    value,
                    target: "calendar.txt or calendar_dates.txt",
                } if value == "NOPE"
            ),
            "{error}"
        );
    }

    #[test]
    fn a_weekday_other_than_zero_or_one_is_invalid() {
        for value in ["2", "yes", "01", "-1"] {
            let calendar = format!("{CALENDAR_HEADER}WK,1,1,{value},1,1,0,0,20260101,20261231\n");
            let error = Feed::minimal()
                .with("calendar.txt", &calendar)
                .load()
                .unwrap_err();

            assert_invalid_value(&error, "calendar.txt", 2, "wednesday", value);
        }
    }

    #[test]
    fn an_empty_weekday_is_a_missing_field() {
        let calendar = format!("{CALENDAR_HEADER}WK,1,1,1,1,1,0,,20260101,20261231\n");
        let error = Feed::minimal()
            .with("calendar.txt", &calendar)
            .load()
            .unwrap_err();

        assert!(
            matches!(
                error,
                AdapterError::MissingField {
                    file: "calendar.txt",
                    line: 2,
                    field: "sunday"
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn an_exception_type_other_than_one_or_two_is_invalid() {
        for value in ["0", "3", "01", "added"] {
            let exceptions = format!("{CALENDAR_DATES_HEADER}WK,20260815,{value}\n");
            let error = Feed::minimal()
                .with("calendar_dates.txt", &exceptions)
                .load()
                .unwrap_err();

            assert_invalid_value(&error, "calendar_dates.txt", 2, "exception_type", value);
        }
    }

    #[test]
    fn a_date_that_is_not_a_real_calendar_date_is_invalid() {
        for value in ["20261301", "20260230", "2026-01-01", "2026011"] {
            let exceptions = format!("{CALENDAR_DATES_HEADER}WK,{value},1\n");
            let error = Feed::minimal()
                .with("calendar_dates.txt", &exceptions)
                .load()
                .unwrap_err();
            assert_invalid_value(&error, "calendar_dates.txt", 2, "date", value);

            let calendar = format!("{CALENDAR_HEADER}WK,1,1,1,1,1,0,0,{value},20261231\n");
            let error = Feed::minimal()
                .with("calendar.txt", &calendar)
                .load()
                .unwrap_err();
            assert_invalid_value(&error, "calendar.txt", 2, "start_date", value);

            let calendar = format!("{CALENDAR_HEADER}WK,1,1,1,1,1,0,0,20260101,{value}\n");
            let error = Feed::minimal()
                .with("calendar.txt", &calendar)
                .load()
                .unwrap_err();
            assert_invalid_value(&error, "calendar.txt", 2, "end_date", value);
        }
    }

    #[test]
    fn a_start_date_after_the_end_date_is_rejected() {
        let calendar = format!(
            "{CALENDAR_HEADER}OK,1,1,1,1,1,0,0,20260101,20261231\n\
             WK,1,1,1,1,1,0,0,20261231,20260101\n"
        );
        let error = Feed::minimal()
            .with("calendar.txt", &calendar)
            .load()
            .unwrap_err();

        assert!(
            matches!(
                &error,
                AdapterError::ReversedDateRange { file: "calendar.txt", line: 3, start, end }
                    if *start == date("20261231") && *end == date("20260101")
            ),
            "{error}"
        );
    }

    #[test]
    fn the_operator_comes_from_the_configured_id_and_the_agency_file() {
        let dataset = Feed::minimal().load().unwrap();

        assert_eq!(dataset.operator.id, "demo");
        assert_eq!(dataset.operator.name, "Demo Transit");
        assert_eq!(
            dataset.operator.url.as_deref(),
            Some("https://transit.example")
        );
        assert_eq!(dataset.operator.timezone, "Etc/UTC");
    }

    #[test]
    fn every_entity_id_is_namespaced_with_the_operator_id() {
        let dataset = Feed::minimal().load().unwrap();

        let stop_ids: Vec<_> = dataset.stops.iter().map(|stop| stop.id.as_str()).collect();
        assert_eq!(stop_ids, ["demo:STN", "demo:S1", "demo:S2"]);
        assert_eq!(dataset.routes[0].id, id("R1"));
        assert_eq!(dataset.routes[0].operator_id, "demo");
        assert_eq!(dataset.trips[0].id, id("T1"));
        assert_eq!(dataset.trips[0].route_id, id("R1"));
        assert_eq!(dataset.trips[0].service_id, id("WK"));
        assert_eq!(dataset.trips[0].shape_id, Some(id("SH1")));
        assert_eq!(dataset.stop_times[0].trip_id, id("T1"));
        assert_eq!(dataset.stop_times[0].stop_id, id("S1"));
    }

    #[test]
    fn stops_keep_their_parent_station_and_defaults() {
        let dataset = Feed::minimal().load().unwrap();
        let [station, platform, plain] = dataset.stops.as_slice() else {
            panic!("expected three stops");
        };

        assert_eq!(station.location_type, 1);
        assert_eq!(station.parent_station, None);
        assert_eq!(station.wheelchair_boarding, 1);
        assert_eq!(platform.parent_station, Some(id("STN")));
        assert_eq!(platform.code.as_deref(), Some("C1"));
        assert_eq!(platform.lat, 1.5001);
        assert_eq!(plain.location_type, 0);
        assert_eq!(plain.wheelchair_boarding, 0);
        assert_eq!(plain.code, None);
    }

    #[test]
    fn stop_times_past_midnight_count_beyond_24_hours() {
        let dataset = Feed::minimal().load().unwrap();
        let second = &dataset.stop_times[1];

        assert_eq!(
            dataset.stop_times[0].arrival,
            ServiceTime::from_seconds(85_800)
        );
        assert_eq!(second.arrival, ServiceTime::from_seconds(87_030));
        assert_eq!(second.stop_sequence, 2);
        assert_eq!(second.pickup_type, 1);
    }

    #[test]
    fn a_stop_time_with_one_time_uses_it_for_both() {
        let stop_times = "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
            T1,08:00:00,,S1,1\n\
            T1,,08:10:00,S2,2\n";
        let dataset = Feed::minimal()
            .with("stop_times.txt", stop_times)
            .load()
            .unwrap();

        assert_eq!(
            dataset.stop_times[0].departure,
            ServiceTime::from_seconds(28_800)
        );
        assert_eq!(
            dataset.stop_times[1].arrival,
            ServiceTime::from_seconds(29_400)
        );
    }

    #[test]
    fn fares_treat_an_empty_transfers_field_as_unlimited() {
        let fares = "fare_id,price,currency_type,payment_method,transfers,agency_id,transfer_duration\n\
            FLAT,2.50,USD,0,,A1,5400\n\
            SINGLE,4,USD,1,0,A1,\n";
        let dataset = Feed::minimal()
            .with("fare_attributes.txt", fares)
            .load()
            .unwrap();
        let [flat, single] = dataset.fares.as_slice() else {
            panic!("expected two fares");
        };

        assert_eq!(flat.id, id("FLAT"));
        assert_eq!(flat.price, 2.5);
        assert_eq!(flat.currency, "USD");
        assert_eq!(flat.payment_method, 0);
        assert_eq!(flat.transfers, None);
        assert_eq!(flat.transfer_duration, Some(5400));
        assert_eq!(single.transfers, Some(0));
        assert_eq!(single.transfer_duration, None);
    }

    #[test]
    fn shapes_frequencies_and_transfers_are_loaded_when_present() {
        let shapes = "shape_id,shape_pt_lat,shape_pt_lon,shape_pt_sequence,shape_dist_traveled\n\
            SH1,1.5,103.5,0,\n\
            SH1,1.6,103.6,1,12.5\n";
        let frequencies = "trip_id,start_time,end_time,headway_secs,exact_times\n\
            T1,06:00:00,25:00:00,600,\n";
        let transfers = "from_stop_id,to_stop_id,transfer_type,min_transfer_time\n\
            S1,S2,2,180\n";
        let dataset = Feed::minimal()
            .with("shapes.txt", shapes)
            .with("frequencies.txt", frequencies)
            .with("transfers.txt", transfers)
            .load()
            .unwrap();

        assert_eq!(dataset.shapes.len(), 2);
        assert_eq!(dataset.shapes[0].shape_id, id("SH1"));
        assert_eq!(dataset.shapes[0].dist_traveled, None);
        assert_eq!(dataset.shapes[1].dist_traveled, Some(12.5));
        assert_eq!(dataset.frequencies[0].trip_id, id("T1"));
        assert_eq!(
            dataset.frequencies[0].end,
            ServiceTime::from_seconds(90_000)
        );
        assert_eq!(dataset.frequencies[0].headway_secs, 600);
        assert_eq!(dataset.transfers[0].from_stop_id, id("S1"));
        assert_eq!(dataset.transfers[0].transfer_type, 2);
        assert_eq!(dataset.transfers[0].min_transfer_time, Some(180));
    }

    #[test]
    fn optional_files_may_be_absent_and_unknown_files_are_ignored() {
        let dataset = Feed::minimal()
            .with("vendor_extension.txt", "anything,at,all\n1,2,3\n")
            .load()
            .unwrap();

        assert!(dataset.shapes.is_empty());
        assert!(dataset.frequencies.is_empty());
        assert!(dataset.fares.is_empty());
        assert!(dataset.transfers.is_empty());
    }

    #[test]
    fn a_leading_byte_order_mark_does_not_hide_the_first_column() {
        let dataset = Feed::minimal()
            .with("agency.txt", &format!("\u{feff}{AGENCY}"))
            .load()
            .unwrap();

        assert_eq!(dataset.operator.name, "Demo Transit");
    }

    #[test]
    fn a_missing_required_file_is_reported_by_name() {
        let error = Feed::minimal()
            .without("stop_times.txt")
            .load()
            .unwrap_err();

        assert!(
            matches!(
                error,
                AdapterError::MissingFile {
                    file: "stop_times.txt"
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn an_agency_file_without_rows_is_rejected() {
        let error = Feed::minimal()
            .with("agency.txt", "agency_name,agency_url,agency_timezone\n")
            .load()
            .unwrap_err();

        assert!(
            matches!(error, AdapterError::EmptyFile { file: "agency.txt" }),
            "{error}"
        );
    }

    #[test]
    fn a_malformed_value_reports_file_line_and_field() {
        let stops = "stop_id,stop_name,stop_lat,stop_lon\n\
            STN,Central,1.5,103.5\n\
            S1,Platform,north,103.5\n\
            S2,Riverside,1.6,103.6\n";
        let error = Feed::minimal().with("stops.txt", stops).load().unwrap_err();

        assert!(
            matches!(
                &error,
                AdapterError::InvalidValue { file: "stops.txt", line: 3, field: "stop_lat", value } if value == "north"
            ),
            "{error}"
        );
    }

    #[test]
    fn an_out_of_range_coordinate_is_invalid() {
        let stops =
            "stop_id,stop_name,stop_lat,stop_lon\nS1,Platform,91,103.5\nS2,Riverside,1.6,103.6\n";
        let error = Feed::minimal().with("stops.txt", stops).load().unwrap_err();

        assert!(
            matches!(
                error,
                AdapterError::InvalidValue {
                    file: "stops.txt",
                    line: 2,
                    field: "stop_lat",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_malformed_time_reports_file_line_and_field() {
        let stop_times = "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
            T1,08:00:00,08:00:00,S1,1\n\
            T1,8.10,08:10:00,S2,2\n";
        let error = Feed::minimal()
            .with("stop_times.txt", stop_times)
            .load()
            .unwrap_err();

        assert!(
            matches!(
                error,
                AdapterError::InvalidValue {
                    file: "stop_times.txt",
                    line: 3,
                    field: "arrival_time",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn an_empty_required_field_reports_file_line_and_field() {
        let routes = "route_id,route_short_name,route_type\nR1,1,\n";
        let error = Feed::minimal()
            .with("routes.txt", routes)
            .load()
            .unwrap_err();

        assert!(
            matches!(
                error,
                AdapterError::MissingField {
                    file: "routes.txt",
                    line: 2,
                    field: "route_type"
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_blank_intermediate_stop_time_is_placed_by_distance_along_the_trip() {
        let stops = "stop_id,stop_name,stop_lat,stop_lon\n\
            A,Stop A,0,0\n\
            B,Stop B,0,1\n\
            C,Stop C,0,4\n";
        let stop_times = "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
            T1,08:00:00,08:00:00,A,1\n\
            T1,,,B,2\n\
            T1,08:10:00,08:10:00,C,3\n";
        let dataset = Feed::minimal()
            .with("stops.txt", stops)
            .with("stop_times.txt", stop_times)
            .load()
            .unwrap();

        assert_eq!(
            dataset.stop_times[1].arrival,
            ServiceTime::from_seconds(28_950)
        );
        assert_eq!(
            dataset.stop_times[1].departure,
            ServiceTime::from_seconds(28_950)
        );
    }

    #[test]
    fn a_trip_ending_on_a_stop_without_any_time_is_rejected() {
        let stop_times = "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
            T1,08:00:00,08:00:00,S1,1\n\
            T1,,,S2,2\n";
        let error = Feed::minimal()
            .with("stop_times.txt", stop_times)
            .load()
            .unwrap_err();

        assert!(
            matches!(
                error,
                AdapterError::UntimedTripEnd {
                    file: "stop_times.txt",
                    line: 3
                }
            ),
            "{error}"
        );
        assert!(error.to_string().contains("arrival_time"), "{error}");
        assert!(error.to_string().contains("departure_time"), "{error}");
    }

    #[test]
    fn backwards_anchors_around_a_blank_stop_are_rejected_with_both_lines() {
        let stops = "stop_id,stop_name,stop_lat,stop_lon\n\
            A,Stop A,0,0\n\
            B,Stop B,0,1\n\
            C,Stop C,0,4\n";
        let stop_times = "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
            T1,08:10:00,08:10:00,A,1\n\
            T1,,,B,2\n\
            T1,08:00:00,08:00:00,C,3\n";
        let error = Feed::minimal()
            .with("stops.txt", stops)
            .with("stop_times.txt", stop_times)
            .load()
            .unwrap_err();

        assert!(
            matches!(
                error,
                AdapterError::BackwardsTimes {
                    file: "stop_times.txt",
                    line: 4,
                    earlier_line: 2
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_stop_time_arriving_after_it_departs_is_rejected_even_beside_a_blank_stop() {
        let stops = "stop_id,stop_name,stop_lat,stop_lon\n\
            A,Stop A,0,0\n\
            B,Stop B,0,0\n\
            C,Stop C,0,4\n";
        let stop_times = "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
            T1,08:10:00,08:00:00,A,1\n\
            T1,,,B,2\n\
            T1,08:30:00,08:30:00,C,3\n";
        let error = Feed::minimal()
            .with("stops.txt", stops)
            .with("stop_times.txt", stop_times)
            .load()
            .unwrap_err();

        assert!(
            matches!(
                error,
                AdapterError::ArrivalAfterDeparture {
                    file: "stop_times.txt",
                    line: 2
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_stop_time_arriving_after_it_departs_is_rejected_past_twenty_four_hours() {
        let stop_times = |arrival: &str, departure: &str| {
            format!(
                "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
                 T1,{arrival},{departure},S1,1\n\
                 T1,26:00:00,26:00:00,S2,2\n"
            )
        };

        let error = Feed::minimal()
            .with("stop_times.txt", &stop_times("25:10:00", "25:00:00"))
            .load()
            .unwrap_err();
        assert!(
            matches!(
                error,
                AdapterError::ArrivalAfterDeparture {
                    file: "stop_times.txt",
                    line: 2
                }
            ),
            "{error}"
        );

        Feed::minimal()
            .with("stop_times.txt", &stop_times("25:00:00", "25:00:00"))
            .load()
            .unwrap();
    }

    #[test]
    fn backwards_anchors_report_their_true_lines_when_rows_are_out_of_order() {
        let stops = "stop_id,stop_name,stop_lat,stop_lon\n\
            A,Stop A,0,0\n\
            B,Stop B,0,1\n\
            C,Stop C,0,4\n";
        let stop_times = "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
            T1,08:00:00,08:00:00,C,3\n\
            T1,,,B,2\n\
            T1,08:10:00,08:10:00,A,1\n";
        let error = Feed::minimal()
            .with("stops.txt", stops)
            .with("stop_times.txt", stop_times)
            .load()
            .unwrap_err();

        assert!(
            matches!(
                error,
                AdapterError::BackwardsTimes {
                    file: "stop_times.txt",
                    line: 2,
                    earlier_line: 4
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_non_finite_price_is_invalid() {
        for price in ["NaN", "inf", "-inf"] {
            let fares = format!(
                "fare_id,price,currency_type,payment_method,transfers\nF1,{price},USD,0,\n"
            );
            let error = Feed::minimal()
                .with("fare_attributes.txt", &fares)
                .load()
                .unwrap_err();

            assert!(
                matches!(
                    &error,
                    AdapterError::InvalidValue {
                        file: "fare_attributes.txt",
                        line: 2,
                        field: "price",
                        value
                    } if value == price
                ),
                "{error}"
            );
        }
    }

    #[test]
    fn a_non_finite_shape_distance_is_invalid() {
        for distance in ["NaN", "inf"] {
            let shapes = format!(
                "shape_id,shape_pt_lat,shape_pt_lon,shape_pt_sequence,shape_dist_traveled\n\
                 SH1,1.5,103.5,0,{distance}\n"
            );
            let error = Feed::minimal()
                .with("shapes.txt", &shapes)
                .load()
                .unwrap_err();

            assert!(
                matches!(
                    &error,
                    AdapterError::InvalidValue {
                        file: "shapes.txt",
                        line: 2,
                        field: "shape_dist_traveled",
                        value
                    } if value == distance
                ),
                "{error}"
            );
        }
    }

    #[test]
    fn a_repeated_primary_key_reports_file_line_and_key() {
        let cases: [(&'static str, &str, u64, &[&str], &str); 10] = [
            (
                "calendar.txt",
                "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n\
                 WK,1,1,1,1,1,0,0,20260101,20261231\n\
                 WK,0,0,0,0,0,1,1,20260101,20261231\n",
                3,
                &["service_id"],
                "WK",
            ),
            (
                "calendar_dates.txt",
                "service_id,date,exception_type\nWK,20260815,1\nWK,20260816,1\nWK,20260815,2\n",
                4,
                &["service_id", "date"],
                "WK, 20260815",
            ),
            (
                "stops.txt",
                "stop_id,stop_name,stop_lat,stop_lon\nS1,A,1,2\nS2,B,1,2\nS1,C,1,2\n",
                4,
                &["stop_id"],
                "S1",
            ),
            (
                "routes.txt",
                "route_id,route_type\nR1,3\nR1,3\n",
                3,
                &["route_id"],
                "R1",
            ),
            (
                "trips.txt",
                "route_id,service_id,trip_id\nR1,WK,T1\nR1,WK,T1\n",
                3,
                &["trip_id"],
                "T1",
            ),
            (
                "stop_times.txt",
                "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
                 T1,08:00:00,08:00:00,S1,1\n\
                 T1,08:10:00,08:10:00,S2,01\n",
                3,
                &["trip_id", "stop_sequence"],
                "T1, 01",
            ),
            (
                "shapes.txt",
                "shape_id,shape_pt_lat,shape_pt_lon,shape_pt_sequence\nSH1,1,2,0\nSH1,1,2,0\n",
                3,
                &["shape_id", "shape_pt_sequence"],
                "SH1, 0",
            ),
            (
                "frequencies.txt",
                "trip_id,start_time,end_time,headway_secs\n\
                 T1,06:00:00,07:00:00,600\n\
                 T1,6:00:00,08:00:00,300\n",
                3,
                &["trip_id", "start_time"],
                "T1, 6:00:00",
            ),
            (
                "fare_attributes.txt",
                "fare_id,price,currency_type,payment_method,transfers\nF1,1,USD,0,\nF1,2,USD,0,\n",
                3,
                &["fare_id"],
                "F1",
            ),
            (
                "transfers.txt",
                "from_stop_id,to_stop_id\nS1,S2\nS2,S1\nS1,S2\n",
                4,
                &["from_stop_id", "to_stop_id"],
                "S1, S2",
            ),
        ];

        for (file, contents, expected_line, expected_fields, expected_value) in cases {
            let error = Feed::minimal().with(file, contents).load().unwrap_err();

            match &error {
                AdapterError::DuplicateKey {
                    file: reported,
                    line,
                    key_fields,
                    value,
                } => {
                    assert_eq!(*reported, file);
                    assert_eq!(*line, expected_line, "{file}");
                    assert_eq!(*key_fields, expected_fields, "{file}");
                    assert_eq!(value, expected_value, "{file}");
                }
                other => panic!("{file}: expected a duplicate key, got {other}"),
            }
        }
    }

    #[test]
    fn a_composite_key_only_repeats_when_every_part_matches() {
        let trips = "route_id,service_id,trip_id\nR1,WK,T1\nR1,WK,T2\n";
        let stop_times = "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
            T1,08:00:00,08:00:00,S1,1\n\
            T2,08:00:00,08:00:00,S1,1\n";
        let dataset = Feed::minimal()
            .with("trips.txt", trips)
            .with("stop_times.txt", stop_times)
            .load()
            .unwrap();

        assert_eq!(dataset.stop_times.len(), 2);
    }

    fn assert_parent_cycle(stops: &str, expected_line: u64, expected_stop: &str) {
        let error = Feed::minimal().with("stops.txt", stops).load().unwrap_err();

        assert!(
            matches!(
                &error,
                AdapterError::ParentCycle { file: "stops.txt", line, stop_id }
                    if *line == expected_line && stop_id == expected_stop
            ),
            "{error}"
        );
    }

    #[test]
    fn a_stop_that_is_its_own_parent_is_rejected() {
        assert_parent_cycle(
            "stop_id,stop_name,stop_lat,stop_lon,parent_station\n\
             S1,Platform,1.5,103.5,\n\
             S2,Riverside,1.6,103.6,S2\n",
            3,
            "S2",
        );
    }

    #[test]
    fn a_parent_cycle_is_rejected_at_a_stop_inside_the_cycle() {
        assert_parent_cycle(
            "stop_id,stop_name,stop_lat,stop_lon,parent_station\n\
             LEAF,Leaf,1.5,103.5,A\n\
             A,Stop A,1.5,103.5,B\n\
             B,Stop B,1.5,103.5,C\n\
             C,Stop C,1.5,103.5,A\n",
            3,
            "A",
        );
    }

    #[test]
    fn a_boarding_area_may_have_a_platform_parent_listed_after_it() {
        let stops = "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
            BOARD,Boarding Area,1.5,103.5,4,S1\n\
            S1,Platform,1.5,103.5,0,STN\n\
            STN,Central,1.5,103.5,1,\n\
            S2,Riverside,1.6,103.6,0,\n";
        let dataset = Feed::minimal().with("stops.txt", stops).load().unwrap();

        assert_eq!(dataset.stops[0].parent_station, Some(id("S1")));
    }

    #[test]
    fn a_row_with_the_wrong_number_of_fields_is_a_csv_error_with_its_line() {
        let trips = "route_id,service_id,trip_id\nR1,WK,T1,extra\n";
        let error = Feed::minimal().with("trips.txt", trips).load().unwrap_err();

        assert!(
            matches!(
                error,
                AdapterError::Csv {
                    file: "trips.txt",
                    line: 2,
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn an_unknown_parent_station_is_rejected() {
        let stops = "stop_id,stop_name,stop_lat,stop_lon,parent_station\n\
            S1,Platform,1.5,103.5,NOPE\n\
            S2,Riverside,1.6,103.6,\n";
        let error = Feed::minimal().with("stops.txt", stops).load().unwrap_err();

        assert!(
            matches!(
                &error,
                AdapterError::UnknownReference { file: "stops.txt", line: 2, field: "parent_station", value, target: "stops.txt" }
                    if value == "NOPE"
            ),
            "{error}"
        );
    }

    #[test]
    fn a_stop_time_for_an_unknown_stop_is_rejected() {
        let stop_times = "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
            T1,08:00:00,08:00:00,S1,1\n\
            T1,08:10:00,08:10:00,S9,2\n";
        let error = Feed::minimal()
            .with("stop_times.txt", stop_times)
            .load()
            .unwrap_err();

        assert!(
            matches!(
                error,
                AdapterError::UnknownReference {
                    file: "stop_times.txt",
                    line: 3,
                    field: "stop_id",
                    target: "stops.txt",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_trip_for_an_unknown_route_is_rejected() {
        let trips = "route_id,service_id,trip_id\nR9,WK,T1\n";
        let error = Feed::minimal().with("trips.txt", trips).load().unwrap_err();

        assert!(
            matches!(
                error,
                AdapterError::UnknownReference {
                    file: "trips.txt",
                    line: 2,
                    field: "route_id",
                    target: "routes.txt",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_frequency_or_transfer_with_unknown_references_is_rejected() {
        let frequencies = "trip_id,start_time,end_time,headway_secs\nT9,06:00:00,07:00:00,600\n";
        let error = Feed::minimal()
            .with("frequencies.txt", frequencies)
            .load()
            .unwrap_err();
        assert!(
            matches!(
                error,
                AdapterError::UnknownReference {
                    file: "frequencies.txt",
                    line: 2,
                    field: "trip_id",
                    ..
                }
            ),
            "{error}"
        );

        let transfers = "from_stop_id,to_stop_id,transfer_type\nS1,S9,0\n";
        let error = Feed::minimal()
            .with("transfers.txt", transfers)
            .load()
            .unwrap_err();
        assert!(
            matches!(
                error,
                AdapterError::UnknownReference {
                    file: "transfers.txt",
                    line: 2,
                    field: "to_stop_id",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn bytes_that_are_not_a_zip_archive_are_an_archive_error() {
        let error = GtfsScheduleAdapter::new("demo")
            .load_archive(Cursor::new(b"not a zip".to_vec()))
            .unwrap_err();

        assert!(matches!(error, AdapterError::Archive(_)), "{error}");
    }

    #[test]
    fn load_static_reads_the_archive_from_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feed.zip");
        std::fs::write(&path, Feed::minimal().zip().into_inner()).unwrap();

        let dataset = GtfsScheduleAdapter::new("demo").load_static(&path).unwrap();

        assert_eq!(dataset.stops.len(), 3);
    }

    #[test]
    fn load_static_reports_an_unreadable_path() {
        let error = GtfsScheduleAdapter::new("demo")
            .load_static(Path::new("/nonexistent/feed.zip"))
            .unwrap_err();

        assert!(matches!(error, AdapterError::Io { .. }), "{error}");
    }
}
