mod table;

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use zip::ZipArchive;
use zip::result::ZipError;

use self::table::{Row, parse_rows};
use super::{AdapterError, OperatorAdapter};
use crate::transit_model::{
    Dataset, EntityId, Fare, Frequency, Operator, Route, ServiceTime, ShapePoint, Stop, StopTime,
    Transfer, Trip,
};

const AGENCY: &str = "agency.txt";
const STOPS: &str = "stops.txt";
const ROUTES: &str = "routes.txt";
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
            parse_file(&mut archive, ROUTES, |row| self.route(row))?,
        )?;
        let route_ids = ids(&routes, |route| &route.id);
        let trips = required(
            TRIPS,
            parse_file(&mut archive, TRIPS, |row| self.trip(row, &route_ids))?,
        )?;
        let trip_ids = ids(&trips, |trip| &trip.id);
        let stop_times = required(
            STOP_TIMES,
            parse_file(&mut archive, STOP_TIMES, |row| {
                self.stop_time(row, &trip_ids, &stop_ids)
            })?,
        )?;
        let shapes =
            parse_file(&mut archive, SHAPES, |row| self.shape_point(row))?.unwrap_or_default();
        let frequencies = parse_file(&mut archive, FREQUENCIES, |row| {
            self.frequency(row, &trip_ids)
        })?
        .unwrap_or_default();
        let fares =
            parse_file(&mut archive, FARE_ATTRIBUTES, |row| self.fare(row))?.unwrap_or_default();
        let transfers = parse_file(&mut archive, TRANSFERS, |row| self.transfer(row, &stop_ids))?
            .unwrap_or_default();

        Ok(Dataset {
            operator,
            stops,
            routes,
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
        let mut parents = Vec::new();
        let stops = required(
            STOPS,
            parse_file(archive, STOPS, |row| {
                let stop = self.stop(row)?;
                if let Some(parent) = row.optional("parent_station") {
                    parents.push((row.line(), parent.to_owned()));
                }
                Ok(stop)
            })?,
        )?;
        let stop_ids = ids(&stops, |stop| &stop.id);
        match parents
            .into_iter()
            .find(|(_, parent)| !stop_ids.contains(&self.id(parent)))
        {
            Some((line, parent)) => Err(AdapterError::UnknownReference {
                file: STOPS,
                line,
                field: "parent_station",
                value: parent,
                target: STOPS,
            }),
            None => Ok(stops),
        }
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

    fn trip(&self, row: &Row, route_ids: &HashSet<EntityId>) -> Result<Trip, AdapterError> {
        Ok(Trip {
            id: self.id(row.required("trip_id")?),
            route_id: self.reference(row, "route_id", route_ids, ROUTES)?,
            service_id: self.id(row.required("service_id")?),
            headsign: row.optional("trip_headsign").map(str::to_owned),
            direction_id: row.parse_optional("direction_id")?,
            shape_id: row.optional("shape_id").map(|shape| self.id(shape)),
            wheelchair_accessible: row.parse_or("wheelchair_accessible", 0)?,
        })
    }

    /// GTFS lets either time be blank when the other is given; a row with
    /// neither has no time to store.
    fn stop_time(
        &self,
        row: &Row,
        trip_ids: &HashSet<EntityId>,
        stop_ids: &HashSet<EntityId>,
    ) -> Result<StopTime, AdapterError> {
        let arrival = optional_time(row, "arrival_time")?;
        let departure = optional_time(row, "departure_time")?;
        let (arrival, departure) = match (arrival, departure) {
            (Some(arrival), Some(departure)) => (arrival, departure),
            (Some(time), None) | (None, Some(time)) => (time, time),
            (None, None) => {
                return Err(AdapterError::MissingField {
                    file: STOP_TIMES,
                    line: row.line(),
                    field: "arrival_time",
                });
            }
        };
        Ok(StopTime {
            trip_id: self.reference(row, "trip_id", trip_ids, TRIPS)?,
            stop_sequence: row.parse_required("stop_sequence")?,
            stop_id: self.reference(row, "stop_id", stop_ids, STOPS)?,
            arrival,
            departure,
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
            dist_traveled: row.parse_optional("shape_dist_traveled")?,
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
            price: row.parse_required("price")?,
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

fn required<T>(file: &'static str, rows: Option<Vec<T>>) -> Result<Vec<T>, AdapterError> {
    rows.ok_or(AdapterError::MissingFile { file })
}

fn ids<T>(items: &[T], id: impl Fn(&T) -> &EntityId) -> HashSet<EntityId> {
    items.iter().map(|item| id(item).clone()).collect()
}

fn coordinate(row: &Row, field: &'static str, limit: f64) -> Result<f64, AdapterError> {
    let value: f64 = row.parse_required(field)?;
    if value.is_finite() && value.abs() <= limit {
        Ok(value)
    } else {
        Err(row.invalid(field, row.required(field)?))
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{Cursor, Write};

    use zip::write::SimpleFileOptions;

    use super::*;
    use crate::transit_model::{EntityId, ServiceTime};

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

    struct Feed(BTreeMap<&'static str, String>);

    impl Feed {
        fn minimal() -> Self {
            Self(BTreeMap::from([
                ("agency.txt", AGENCY.to_owned()),
                ("stops.txt", STOPS.to_owned()),
                ("routes.txt", ROUTES.to_owned()),
                ("trips.txt", TRIPS.to_owned()),
                ("stop_times.txt", STOP_TIMES.to_owned()),
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
    fn a_stop_time_without_any_time_is_rejected() {
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
                AdapterError::MissingField {
                    file: "stop_times.txt",
                    line: 3,
                    field: "arrival_time"
                }
            ),
            "{error}"
        );
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
