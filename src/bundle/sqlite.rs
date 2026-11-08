use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, Transaction, params};

use crate::transit_model::{Dataset, EntityId, Stop};

use super::{BundleError, SCHEMA_VERSION};

pub const SCHEMA_SQL: &str = include_str!("../../contracts/bundle/schema.sql");

/// Writes `dataset` into a new SQLite database at `path`, which must not
/// hold a database yet. Rows go in foreign key order inside one transaction,
/// so a dangling reference fails the whole write.
pub fn write_sqlite(
    dataset: &Dataset,
    source_sha256: &str,
    path: &Path,
) -> Result<(), BundleError> {
    let mut connection = Connection::open(path)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.execute_batch(SCHEMA_SQL)?;

    let transaction = connection.transaction()?;
    insert_operator(&transaction, dataset)?;
    insert_stops(&transaction, &dataset.stops)?;
    insert_routes(&transaction, dataset)?;
    insert_trips(&transaction, dataset)?;
    insert_stop_times(&transaction, dataset)?;
    insert_shapes(&transaction, dataset)?;
    insert_frequencies(&transaction, dataset)?;
    insert_fares(&transaction, dataset)?;
    insert_transfers(&transaction, dataset)?;
    transaction.execute(
        "INSERT INTO bundle_meta (id, schema_version, source_sha256, built_at) \
         VALUES (1, ?1, ?2, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
        params![SCHEMA_VERSION, source_sha256],
    )?;
    transaction.commit()?;
    connection.close().map_err(|(_, error)| error)?;
    Ok(())
}

fn insert_operator(transaction: &Transaction, dataset: &Dataset) -> rusqlite::Result<()> {
    let operator = &dataset.operator;
    transaction.execute(
        "INSERT INTO operators (id, name, url, timezone) VALUES (?1, ?2, ?3, ?4)",
        params![operator.id, operator.name, operator.url, operator.timezone],
    )?;
    Ok(())
}

fn insert_stops(transaction: &Transaction, stops: &[Stop]) -> rusqlite::Result<()> {
    let mut statement = transaction.prepare(
        "INSERT INTO stops (id, code, name, lat, lon, location_type, parent_station, wheelchair_boarding) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for stop in parents_first(stops) {
        statement.execute(params![
            stop.id.as_str(),
            stop.code,
            stop.name,
            stop.lat,
            stop.lon,
            stop.location_type,
            stop.parent_station.as_ref().map(EntityId::as_str),
            stop.wheelchair_boarding,
        ])?;
    }
    Ok(())
}

/// Orders stops by how many parents they have, so each parent station row
/// exists before the rows that reference it.
fn parents_first(stops: &[Stop]) -> Vec<&Stop> {
    let parents: HashMap<&EntityId, &EntityId> = stops
        .iter()
        .filter_map(|stop| {
            stop.parent_station
                .as_ref()
                .map(|parent| (&stop.id, parent))
        })
        .collect();
    let depth = |stop: &Stop| {
        std::iter::successors(Some(&stop.id), |id| parents.get(id).copied())
            .take(stops.len() + 1)
            .count()
    };
    let mut ordered: Vec<&Stop> = stops.iter().collect();
    ordered.sort_by_cached_key(|stop| depth(stop));
    ordered
}

fn insert_routes(transaction: &Transaction, dataset: &Dataset) -> rusqlite::Result<()> {
    let mut statement = transaction.prepare(
        "INSERT INTO routes (id, operator_id, short_name, long_name, route_type, color, text_color) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for route in &dataset.routes {
        statement.execute(params![
            route.id.as_str(),
            route.operator_id,
            route.short_name,
            route.long_name,
            route.route_type,
            route.color,
            route.text_color,
        ])?;
    }
    Ok(())
}

fn insert_trips(transaction: &Transaction, dataset: &Dataset) -> rusqlite::Result<()> {
    let mut statement = transaction.prepare(
        "INSERT INTO trips (id, route_id, service_id, headsign, direction_id, shape_id, wheelchair_accessible) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for trip in &dataset.trips {
        statement.execute(params![
            trip.id.as_str(),
            trip.route_id.as_str(),
            trip.service_id.as_str(),
            trip.headsign,
            trip.direction_id,
            trip.shape_id.as_ref().map(EntityId::as_str),
            trip.wheelchair_accessible,
        ])?;
    }
    Ok(())
}

fn insert_stop_times(transaction: &Transaction, dataset: &Dataset) -> rusqlite::Result<()> {
    let mut statement = transaction.prepare(
        "INSERT INTO stop_times (trip_id, stop_sequence, stop_id, arrival_time, departure_time, pickup_type, drop_off_type) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for stop_time in &dataset.stop_times {
        statement.execute(params![
            stop_time.trip_id.as_str(),
            stop_time.stop_sequence,
            stop_time.stop_id.as_str(),
            stop_time.arrival.seconds(),
            stop_time.departure.seconds(),
            stop_time.pickup_type,
            stop_time.drop_off_type,
        ])?;
    }
    Ok(())
}

fn insert_shapes(transaction: &Transaction, dataset: &Dataset) -> rusqlite::Result<()> {
    let mut statement = transaction.prepare(
        "INSERT INTO shapes (shape_id, pt_sequence, lat, lon, dist_traveled) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for point in &dataset.shapes {
        statement.execute(params![
            point.shape_id.as_str(),
            point.sequence,
            point.lat,
            point.lon,
            point.dist_traveled,
        ])?;
    }
    Ok(())
}

fn insert_frequencies(transaction: &Transaction, dataset: &Dataset) -> rusqlite::Result<()> {
    let mut statement = transaction.prepare(
        "INSERT INTO frequencies (trip_id, start_time, end_time, headway_secs, exact_times) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for frequency in &dataset.frequencies {
        statement.execute(params![
            frequency.trip_id.as_str(),
            frequency.start.seconds(),
            frequency.end.seconds(),
            frequency.headway_secs,
            frequency.exact_times,
        ])?;
    }
    Ok(())
}

fn insert_fares(transaction: &Transaction, dataset: &Dataset) -> rusqlite::Result<()> {
    let mut statement = transaction.prepare(
        "INSERT INTO fares (id, operator_id, price, currency, payment_method, transfers, transfer_duration) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for fare in &dataset.fares {
        statement.execute(params![
            fare.id.as_str(),
            dataset.operator.id,
            fare.price,
            fare.currency,
            fare.payment_method,
            fare.transfers,
            fare.transfer_duration,
        ])?;
    }
    Ok(())
}

fn insert_transfers(transaction: &Transaction, dataset: &Dataset) -> rusqlite::Result<()> {
    let mut statement = transaction.prepare(
        "INSERT INTO transfers (from_stop_id, to_stop_id, transfer_type, min_transfer_time) \
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for transfer in &dataset.transfers {
        statement.execute(params![
            transfer.from_stop_id.as_str(),
            transfer.to_stop_id.as_str(),
            transfer.transfer_type,
            transfer.min_transfer_time,
        ])?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::transit_model::{
        EntityId, Fare, Frequency, Operator, Route, ServiceTime, ShapePoint, Stop, StopTime,
        Transfer, Trip,
    };

    const SOURCE_SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn id(source_id: &str) -> EntityId {
        EntityId::namespaced("demo", source_id)
    }

    fn stop(source_id: &str, parent: Option<&str>) -> Stop {
        Stop {
            id: id(source_id),
            code: None,
            name: format!("Stop {source_id}"),
            lat: 1.5,
            lon: 103.5,
            location_type: u8::from(parent.is_none() && source_id.starts_with("STN")),
            parent_station: parent.map(id),
            wheelchair_boarding: 0,
        }
    }

    fn stop_time(sequence: u32, stop_id: &str, seconds: u32) -> StopTime {
        StopTime {
            trip_id: id("T1"),
            stop_sequence: sequence,
            stop_id: id(stop_id),
            arrival: ServiceTime::from_seconds(seconds),
            departure: ServiceTime::from_seconds(seconds),
            pickup_type: 0,
            drop_off_type: 0,
        }
    }

    /// The platform is listed before its station on purpose, to prove the
    /// writer orders parents first.
    pub(crate) fn sample_dataset() -> Dataset {
        Dataset {
            operator: Operator {
                id: "demo".into(),
                name: "Demo Transit".into(),
                url: Some("https://transit.example".into()),
                timezone: "Etc/UTC".into(),
            },
            stops: vec![stop("P1", Some("STN")), stop("STN", None), stop("S2", None)],
            routes: vec![Route {
                id: id("R1"),
                operator_id: "demo".into(),
                short_name: Some("1".into()),
                long_name: None,
                route_type: 3,
                color: None,
                text_color: None,
            }],
            trips: vec![Trip {
                id: id("T1"),
                route_id: id("R1"),
                service_id: id("WK"),
                headsign: Some("Riverside".into()),
                direction_id: Some(0),
                shape_id: Some(id("SH1")),
                wheelchair_accessible: 0,
            }],
            stop_times: vec![stop_time(1, "P1", 85_800), stop_time(2, "S2", 87_030)],
            shapes: vec![ShapePoint {
                shape_id: id("SH1"),
                sequence: 0,
                lat: 1.5,
                lon: 103.5,
                dist_traveled: None,
            }],
            frequencies: vec![Frequency {
                trip_id: id("T1"),
                start: ServiceTime::from_seconds(21_600),
                end: ServiceTime::from_seconds(90_000),
                headway_secs: 600,
                exact_times: 0,
            }],
            fares: vec![Fare {
                id: id("FLAT"),
                price: 2.5,
                currency: "USD".into(),
                payment_method: 0,
                transfers: None,
                transfer_duration: Some(5400),
            }],
            transfers: vec![Transfer {
                from_stop_id: id("P1"),
                to_stop_id: id("S2"),
                transfer_type: 2,
                min_transfer_time: Some(180),
            }],
        }
    }

    fn written(dataset: &Dataset) -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bundle.sqlite");
        write_sqlite(dataset, SOURCE_SHA256, &path).unwrap();
        let connection = Connection::open(&path).unwrap();
        (dir, connection)
    }

    fn count(connection: &Connection, table: &str) -> usize {
        let rows: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        usize::try_from(rows).unwrap()
    }

    #[test]
    fn every_table_holds_one_row_per_dataset_entity() {
        let dataset = sample_dataset();
        let (_dir, connection) = written(&dataset);

        assert_eq!(count(&connection, "operators"), 1);
        assert_eq!(count(&connection, "stops"), dataset.stops.len());
        assert_eq!(count(&connection, "routes"), dataset.routes.len());
        assert_eq!(count(&connection, "trips"), dataset.trips.len());
        assert_eq!(count(&connection, "stop_times"), dataset.stop_times.len());
        assert_eq!(count(&connection, "shapes"), dataset.shapes.len());
        assert_eq!(count(&connection, "frequencies"), dataset.frequencies.len());
        assert_eq!(count(&connection, "fares"), dataset.fares.len());
        assert_eq!(count(&connection, "transfers"), dataset.transfers.len());
    }

    #[test]
    fn the_bundle_meta_row_records_the_schema_and_source() {
        let (_dir, connection) = written(&sample_dataset());

        let (id, schema_version, source, built_at): (i64, u32, String, String) = connection
            .query_row(
                "SELECT id, schema_version, source_sha256, built_at FROM bundle_meta",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        assert_eq!(count(&connection, "bundle_meta"), 1);
        assert_eq!(id, 1);
        assert_eq!(schema_version, super::super::SCHEMA_VERSION);
        assert_eq!(source, SOURCE_SHA256);
        assert_eq!(built_at.len(), "2026-01-01T00:00:00Z".len());
        assert!(built_at.ends_with('Z'), "{built_at}");
    }

    #[test]
    fn rows_keep_namespaced_ids_parent_stations_and_late_night_times() {
        let (_dir, connection) = written(&sample_dataset());

        let parent: String = connection
            .query_row(
                "SELECT parent_station FROM stops WHERE id = 'demo:P1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let late_arrival: u32 = connection
            .query_row(
                "SELECT arrival_time FROM stop_times WHERE trip_id = 'demo:T1' AND stop_sequence = 2",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let unlimited_transfers: Option<u8> = connection
            .query_row("SELECT transfers FROM fares", [], |row| row.get(0))
            .unwrap();
        let route_operator: String = connection
            .query_row("SELECT operator_id FROM routes", [], |row| row.get(0))
            .unwrap();

        assert_eq!(parent, "demo:STN");
        assert_eq!(late_arrival, 87_030);
        assert_eq!(unlimited_transfers, None);
        assert_eq!(route_operator, "demo");
    }

    #[test]
    fn the_written_database_passes_a_foreign_key_check() {
        let (_dir, connection) = written(&sample_dataset());

        let violations: usize = connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query_map([], |_| Ok(()))
            .unwrap()
            .count();

        assert_eq!(violations, 0);
    }

    #[test]
    fn a_dangling_reference_is_rejected_by_the_foreign_keys() {
        let mut dataset = sample_dataset();
        dataset.stop_times.push(stop_time(3, "NOWHERE", 88_000));
        let dir = tempfile::tempdir().unwrap();

        let error =
            write_sqlite(&dataset, SOURCE_SHA256, &dir.path().join("bundle.sqlite")).unwrap_err();

        assert!(matches!(error, BundleError::Sqlite(_)), "{error}");
    }

    #[test]
    fn departures_by_stop_use_the_stop_departure_index() {
        let (_dir, connection) = written(&sample_dataset());

        let plan: Vec<String> = connection
            .prepare(
                "EXPLAIN QUERY PLAN SELECT trip_id FROM stop_times \
                 WHERE stop_id = 'demo:S2' AND departure_time >= 80000 ORDER BY departure_time",
            )
            .unwrap()
            .query_map([], |row| row.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        assert!(
            plan.iter()
                .any(|step| step.contains("idx_stop_times_stop_departure")),
            "{plan:?}"
        );
    }
}
