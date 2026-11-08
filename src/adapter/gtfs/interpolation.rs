use std::collections::HashMap;

use super::STOP_TIMES;
use crate::adapter::AdapterError;
use crate::transit_model::{EntityId, ServiceTime, StopTime};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct GeoPoint {
    pub(super) lat: f64,
    pub(super) lon: f64,
}

impl GeoPoint {
    /// Haversine central angle. Only ratios of distances are ever used, so
    /// the radius of the Earth cancels out.
    fn angle_to(self, other: Self) -> f64 {
        let (lat_a, lat_b) = (self.lat.to_radians(), other.lat.to_radians());
        let half_lat = (lat_b - lat_a) / 2.0;
        let half_lon = (other.lon - self.lon).to_radians() / 2.0;
        let haversine = half_lat.sin().powi(2) + lat_a.cos() * lat_b.cos() * half_lon.sin().powi(2);
        // Rounding can push opposite points just above 1, where asin is NaN.
        2.0 * haversine.clamp(0.0, 1.0).sqrt().asin()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Timing {
    pub(super) arrival: ServiceTime,
    pub(super) departure: ServiceTime,
}

/// A parsed `stop_times.txt` row, before blank times are filled in.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct StopTimeRow {
    pub(super) line: u64,
    pub(super) trip_id: EntityId,
    pub(super) stop_sequence: u32,
    pub(super) stop_id: EntityId,
    pub(super) position: GeoPoint,
    pub(super) timing: Option<Timing>,
    pub(super) pickup_type: u8,
    pub(super) drop_off_type: u8,
}

impl StopTimeRow {
    fn into_stop_time(self, filled: Option<ServiceTime>) -> Result<StopTime, AdapterError> {
        let timing = self
            .timing
            .or(filled.map(|time| Timing {
                arrival: time,
                departure: time,
            }))
            .ok_or(AdapterError::UntimedTripEnd {
                file: STOP_TIMES,
                line: self.line,
            })?;
        Ok(StopTime {
            trip_id: self.trip_id,
            stop_sequence: self.stop_sequence,
            stop_id: self.stop_id,
            arrival: timing.arrival,
            departure: timing.departure,
            pickup_type: self.pickup_type,
            drop_off_type: self.drop_off_type,
        })
    }
}

#[derive(Clone, Copy)]
struct Anchor {
    row: usize,
    time: ServiceTime,
}

/// Times each blank row from the timed rows around it, by distance along the
/// trip. A blank row with no timed row on one side is the first or last stop
/// of its trip, stays unfilled and is reported as such. Rows keep file order.
pub(super) fn fill_blank_times(rows: Vec<StopTimeRow>) -> Result<Vec<StopTime>, AdapterError> {
    let mut filled = vec![None; rows.len()];
    for trip in trips_in_stop_order(&rows) {
        fill_trip(&rows, &trip, &mut filled)?;
    }
    rows.into_iter()
        .zip(filled)
        .map(|(row, time)| row.into_stop_time(time))
        .collect()
}

/// Row indices grouped by trip, trips in order of first appearance so the
/// same feed always reports the same error first.
fn trips_in_stop_order(rows: &[StopTimeRow]) -> Vec<Vec<usize>> {
    let mut slots: HashMap<&EntityId, usize> = HashMap::new();
    let mut trips: Vec<Vec<usize>> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let slot = *slots.entry(&row.trip_id).or_insert_with(|| {
            trips.push(Vec::new());
            trips.len() - 1
        });
        trips[slot].push(index);
    }
    for trip in &mut trips {
        trip.sort_unstable_by_key(|&index| rows[index].stop_sequence);
    }
    trips
}

fn fill_trip(
    rows: &[StopTimeRow],
    trip: &[usize],
    filled: &mut [Option<ServiceTime>],
) -> Result<(), AdapterError> {
    let mut previous: Option<(usize, Timing)> = None;
    let mut blanks = Vec::new();
    for &index in trip {
        let Some(timing) = rows[index].timing else {
            blanks.push(index);
            continue;
        };
        if let Some((earlier, earlier_timing)) = previous
            && !blanks.is_empty()
        {
            let earlier = Anchor {
                row: earlier,
                time: earlier_timing.departure,
            };
            let later = Anchor {
                row: index,
                time: timing.arrival,
            };
            fill_run(rows, earlier, &blanks, later, filled)?;
        }
        blanks.clear();
        previous = Some((index, timing));
    }
    Ok(())
}

fn fill_run(
    rows: &[StopTimeRow],
    earlier: Anchor,
    blanks: &[usize],
    later: Anchor,
    filled: &mut [Option<ServiceTime>],
) -> Result<(), AdapterError> {
    let start = earlier.time.seconds();
    let duration = later
        .time
        .seconds()
        .checked_sub(start)
        .ok_or(AdapterError::BackwardsTimes {
            file: STOP_TIMES,
            line: rows[later.row].line,
            earlier_line: rows[earlier.row].line,
        })?;
    let stops: Vec<GeoPoint> = std::iter::once(earlier.row)
        .chain(blanks.iter().copied())
        .chain(std::iter::once(later.row))
        .map(|index| rows[index].position)
        .collect();

    let mut offset = 0;
    for (&blank, progress) in blanks.iter().zip(progress_of_inner_stops(&stops)) {
        offset = offset.max(seconds_into(duration, progress));
        filled[blank] = Some(ServiceTime::from_seconds(start + offset));
    }
    Ok(())
}

/// How far along `stops`, from 0 to 1, each stop between the two ends lies.
/// Stops at one spot have no distance to compare, so they split evenly.
fn progress_of_inner_stops(stops: &[GeoPoint]) -> Vec<f64> {
    let mut travelled = 0.0;
    let cumulative: Vec<f64> = stops
        .windows(2)
        .map(|leg| {
            travelled += leg[0].angle_to(leg[1]);
            travelled
        })
        .collect();
    match cumulative.split_last() {
        Some((&total, inner)) if total > 0.0 => {
            inner.iter().map(|distance| distance / total).collect()
        }
        _ => {
            let legs = cumulative.len() as f64;
            (1..cumulative.len()).map(|leg| leg as f64 / legs).collect()
        }
    }
}

/// Floating point rounding must never carry a stop past the later anchor.
fn seconds_into(duration: u32, progress: f64) -> u32 {
    ((f64::from(duration) * progress).round() as u32).min(duration)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIP: &str = "T1";

    fn row_at(
        trip: &str,
        line: u64,
        sequence: u32,
        point: GeoPoint,
        timing: Option<Timing>,
    ) -> StopTimeRow {
        StopTimeRow {
            line,
            trip_id: EntityId::namespaced("demo", trip),
            stop_sequence: sequence,
            stop_id: EntityId::namespaced("demo", &format!("S{sequence}")),
            position: point,
            timing,
            pickup_type: 0,
            drop_off_type: 0,
        }
    }

    fn on_equator(lon: f64) -> GeoPoint {
        GeoPoint { lat: 0.0, lon }
    }

    fn timing(arrival: u32, departure: u32) -> Timing {
        Timing {
            arrival: ServiceTime::from_seconds(arrival),
            departure: ServiceTime::from_seconds(departure),
        }
    }

    fn timed(sequence: u32, lon: f64, seconds: u32) -> StopTimeRow {
        row_at(
            TRIP,
            u64::from(sequence) + 1,
            sequence,
            on_equator(lon),
            Some(timing(seconds, seconds)),
        )
    }

    fn blank(sequence: u32, lon: f64) -> StopTimeRow {
        row_at(
            TRIP,
            u64::from(sequence) + 1,
            sequence,
            on_equator(lon),
            None,
        )
    }

    fn arrivals(rows: Vec<StopTimeRow>) -> Vec<u32> {
        fill_blank_times(rows)
            .unwrap()
            .iter()
            .map(|stop_time| stop_time.arrival.seconds())
            .collect()
    }

    #[test]
    fn timed_rows_keep_their_arrival_and_departure() {
        let filled = fill_blank_times(vec![
            row_at(TRIP, 2, 1, on_equator(0.0), Some(timing(100, 130))),
            row_at(TRIP, 3, 2, on_equator(1.0), Some(timing(200, 260))),
        ])
        .unwrap();

        assert_eq!(filled[0].arrival, ServiceTime::from_seconds(100));
        assert_eq!(filled[0].departure, ServiceTime::from_seconds(130));
        assert_eq!(filled[1].arrival, ServiceTime::from_seconds(200));
        assert_eq!(filled[1].departure, ServiceTime::from_seconds(260));
    }

    #[test]
    fn blank_stops_are_spread_by_distance_when_spacing_is_unequal() {
        let times = arrivals(vec![
            timed(1, 0.0, 0),
            blank(2, 1.0),
            blank(3, 3.0),
            timed(4, 4.0, 600),
        ]);

        assert_eq!(times, [0, 150, 450, 600]);
    }

    #[test]
    fn a_blank_stop_arrives_and_departs_at_the_same_second() {
        let filled =
            fill_blank_times(vec![timed(1, 0.0, 0), blank(2, 1.0), timed(3, 4.0, 600)]).unwrap();

        assert_eq!(filled[1].arrival, filled[1].departure);
    }

    #[test]
    fn the_run_starts_at_the_earlier_departure_and_ends_at_the_later_arrival() {
        let filled = fill_blank_times(vec![
            row_at(TRIP, 2, 1, on_equator(0.0), Some(timing(100, 160))),
            blank(2, 1.0),
            row_at(TRIP, 4, 3, on_equator(2.0), Some(timing(260, 300))),
        ])
        .unwrap();

        assert_eq!(filled[1].arrival, ServiceTime::from_seconds(210));
    }

    #[test]
    fn co_located_stops_split_the_interval_evenly_by_stop_count() {
        let times = arrivals(vec![
            timed(1, 5.0, 0),
            blank(2, 5.0),
            blank(3, 5.0),
            timed(4, 5.0, 300),
        ]);

        assert_eq!(times, [0, 100, 200, 300]);
    }

    #[test]
    fn a_blank_stop_co_located_with_its_anchor_shares_the_anchor_time() {
        let times = arrivals(vec![timed(1, 0.0, 100), blank(2, 0.0), timed(3, 2.0, 700)]);

        assert_eq!(times, [100, 100, 700]);
    }

    #[test]
    fn each_run_of_blanks_uses_its_own_anchors() {
        let times = arrivals(vec![
            timed(1, 0.0, 0),
            blank(2, 1.0),
            timed(3, 2.0, 100),
            blank(4, 3.0),
            blank(5, 4.0),
            timed(6, 5.0, 400),
        ]);

        assert_eq!(times, [0, 50, 100, 200, 300, 400]);
    }

    #[test]
    fn rows_are_placed_by_stop_sequence_and_returned_in_file_order() {
        let filled =
            fill_blank_times(vec![timed(3, 4.0, 600), blank(2, 1.0), timed(1, 0.0, 0)]).unwrap();

        let sequences: Vec<u32> = filled.iter().map(|row| row.stop_sequence).collect();
        let times: Vec<u32> = filled.iter().map(|row| row.arrival.seconds()).collect();
        assert_eq!(sequences, [3, 2, 1]);
        assert_eq!(times, [600, 150, 0]);
    }

    #[test]
    fn trips_are_filled_independently_even_when_their_rows_interleave() {
        let times = arrivals(vec![
            row_at("A", 2, 1, on_equator(0.0), Some(timing(0, 0))),
            row_at("B", 3, 1, on_equator(0.0), Some(timing(1000, 1000))),
            row_at("A", 4, 2, on_equator(1.0), None),
            row_at("B", 5, 2, on_equator(1.0), None),
            row_at("A", 6, 3, on_equator(2.0), Some(timing(100, 100))),
            row_at("B", 7, 3, on_equator(2.0), Some(timing(1200, 1200))),
        ]);

        assert_eq!(times, [0, 1000, 50, 1100, 100, 1200]);
    }

    #[test]
    fn anchors_past_twenty_four_hours_keep_counting() {
        let times = arrivals(vec![
            timed(1, 0.0, 90_000),
            blank(2, 1.0),
            timed(3, 2.0, 93_600),
        ]);

        assert_eq!(times, [90_000, 91_800, 93_600]);
    }

    fn assert_untimed_trip_end(rows: Vec<StopTimeRow>, expected_line: u64) {
        let error = fill_blank_times(rows).unwrap_err();

        assert!(
            matches!(
                error,
                AdapterError::UntimedTripEnd { file: "stop_times.txt", line } if line == expected_line
            ),
            "{error}"
        );
    }

    #[test]
    fn a_blank_first_stop_is_rejected_at_its_line() {
        assert_untimed_trip_end(vec![blank(1, 0.0), timed(2, 1.0, 100)], 2);
    }

    #[test]
    fn a_blank_last_stop_is_rejected_at_its_line() {
        assert_untimed_trip_end(vec![timed(1, 0.0, 0), blank(2, 1.0)], 3);
    }

    #[test]
    fn the_first_stop_is_found_by_sequence_rather_than_file_position() {
        assert_untimed_trip_end(
            vec![timed(2, 1.0, 100), timed(3, 2.0, 200), blank(1, 0.0)],
            2,
        );
    }

    #[test]
    fn a_trip_of_one_blank_stop_is_rejected() {
        assert_untimed_trip_end(vec![blank(1, 0.0)], 2);
    }

    #[test]
    fn a_later_anchor_before_the_earlier_one_is_rejected_at_its_line() {
        let error = fill_blank_times(vec![timed(1, 0.0, 500), blank(2, 1.0), timed(3, 2.0, 400)])
            .unwrap_err();

        assert!(
            matches!(
                error,
                AdapterError::BackwardsTimes {
                    file: "stop_times.txt",
                    line: 4,
                    earlier_line: 2,
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn equal_anchors_give_every_blank_stop_that_time() {
        let times = arrivals(vec![timed(1, 0.0, 300), blank(2, 1.0), timed(3, 2.0, 300)]);

        assert_eq!(times, [300, 300, 300]);
    }

    #[test]
    fn backwards_times_are_only_rejected_around_a_run_of_blanks() {
        let times = arrivals(vec![
            timed(1, 0.0, 900),
            timed(2, 1.0, 100),
            blank(3, 2.0),
            timed(4, 3.0, 300),
        ]);

        assert_eq!(times, [900, 100, 200, 300]);
    }

    fn assert_stays_ordered_between_the_anchors(times: &[u32], start: u32, end: u32) {
        assert_eq!(times.first(), Some(&start));
        assert_eq!(times.last(), Some(&end));
        assert!(times.windows(2).all(|pair| pair[0] <= pair[1]), "{times:?}");
    }

    #[test]
    fn many_stops_in_a_very_short_interval_stay_in_order_between_the_anchors() {
        let last = 2_000;
        let mut rows = vec![timed(1, 0.0, 86_399)];
        let mut lon = 0.0;
        for sequence in 2..last {
            lon += 0.000_1 + f64::from(sequence % 7) * 0.003_3;
            rows.push(blank(sequence, lon));
        }
        rows.push(timed(last, lon + 0.1, 86_402));

        assert_stays_ordered_between_the_anchors(&arrivals(rows), 86_399, 86_402);
    }

    #[test]
    fn wildly_uneven_legs_never_pass_the_later_anchor() {
        let times = arrivals(vec![
            timed(1, 0.0, 0),
            blank(2, 1e-12),
            blank(3, 179.0),
            blank(4, 179.999_999_999),
            timed(5, 180.0, u32::MAX),
        ]);

        assert_stays_ordered_between_the_anchors(&times, 0, u32::MAX);
    }

    #[test]
    fn opposite_points_on_the_globe_do_not_break_the_distance() {
        let times = arrivals(vec![
            row_at(
                TRIP,
                2,
                1,
                GeoPoint {
                    lat: 90.0,
                    lon: 0.0,
                },
                Some(timing(0, 0)),
            ),
            row_at(
                TRIP,
                3,
                2,
                GeoPoint {
                    lat: -90.0,
                    lon: 0.0,
                },
                None,
            ),
            row_at(
                TRIP,
                4,
                3,
                GeoPoint {
                    lat: 90.0,
                    lon: 0.0,
                },
                Some(timing(600, 600)),
            ),
        ]);

        assert_eq!(times, [0, 300, 600]);
    }
}
