use super::{GeoPoint, MovementSegment, PositionSample};

/// Intervallo massimo osservabile tra due campioni consecutivi
pub const MAX_GAP_SECONDS: i64 = 75;
/// Raggio terrestre medio usato dal modello sferico
const EARTH_RADIUS_METERS: f64 = 6_371_000.0;
/// Secondi minimi per essere considerata pausa
pub const PAUSE_THRESHOLD_SECONDS: i64 = 180;

/// Calcola in metri la distanza sulla superficie terrestre tra due punti.
pub fn haversine_distance(start: GeoPoint, end: GeoPoint) -> f64 {
    let start_lat = start.lat().to_radians();
    let end_lat = end.lat().to_radians();

    let delta_lat = (end.lat() - start.lat()).to_radians();
    let delta_lon = (end.lon() - start.lon()).to_radians();

    let sin_half_lat = (delta_lat / 2.0).sin();
    let sin_half_lon = (delta_lon / 2.0).sin();

    let haversine = sin_half_lat.powi(2) + start_lat.cos() * end_lat.cos() * sin_half_lon.powi(2);

    // Contiene eventuali errori di arrotondamento nell'intervallo valido.
    let haversine = haversine.clamp(0.0, 1.0);

    let central_angle = 2.0 * haversine.sqrt().atan2((1.0 - haversine).sqrt());

    EARTH_RADIUS_METERS * central_angle
}

/// Costruisce segmenti tra campioni adiacenti nell'ordine ricevuto.
///
/// Scarta le coppie con sessione diversa, timestamp non crescente
/// o gap superiore a [`MAX_GAP_SECONDS`]. ATTENZIONE: Non riordina i campioni.
pub fn consecutive_segments(samples: &[PositionSample]) -> Vec<MovementSegment> {
    let mut segments = Vec::new();

    for coppia in samples.windows(2) {
        let start = &coppia[0];
        let end = &coppia[1];

        if start.session_id() != end.session_id() || end.received_at() <= start.received_at() {
            continue;
        }

        let gap_seconds = end.received_at() - start.received_at();

        if gap_seconds > MAX_GAP_SECONDS {
            continue;
        }

        let start_point = start.point();
        let end_point = end.point();

        let distance_meters = haversine_distance(start_point, end_point);

        segments.push(MovementSegment::new(
            start.session_id(),
            start_point,
            end_point,
            start.received_at(),
            end.received_at(),
            distance_meters,
        ));
    }

    segments
}

/// Somma di tutte le distanze (in metri) dei segmenti
pub fn total_distance_meters(segments: &[MovementSegment]) -> f64 {
    segments
        .iter()
        .map(|segment| segment.distance_meters())
        .sum()
}
/// Somma di tutti i tempi (in secondi) osservati
pub fn observed_seconds(segments: &[MovementSegment]) -> i64 {
    segments
        .iter()
        .map(|segment| segment.duration_seconds())
        .sum()
}
/// Calcolo dei tempi totali delle pause che passano il check. Misurati in secondi.
///
/// Una sequenza ferma viene conteggiata per intero quando raggiunge
/// [`PAUSE_THRESHOLD_SECONDS`]. Movimento, gap, discontinuità e cambi
/// di sessione separano le sequenze.
pub fn paused_seconds(segments: &[MovementSegment]) -> i64 {
    let mut total_paused_seconds = 0;
    let mut current_stationary_seconds = 0;
    let mut previous_segment: Option<&MovementSegment> = None;

    for segment in segments {
        let continues_previous = match previous_segment {
            None => true,
            Some(previous) => {
                previous.session_id() == segment.session_id()
                    && previous.ended_at() == segment.started_at()
                    && previous.end() == segment.start()
            }
        };

        let changed_position = segment.changed_position();

        if !continues_previous || changed_position {
            total_paused_seconds += qualified_pause_seconds(current_stationary_seconds);
            current_stationary_seconds = 0;
        }

        if !changed_position {
            current_stationary_seconds += segment.duration_seconds();
        }

        previous_segment = Some(segment);
    }

    total_paused_seconds + qualified_pause_seconds(current_stationary_seconds)
}

fn qualified_pause_seconds(stationary_seconds: i64) -> i64 {
    if stationary_seconds >= PAUSE_THRESHOLD_SECONDS {
        stationary_seconds
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EARTH_RADIUS_METERS, MAX_GAP_SECONDS, consecutive_segments, haversine_distance,
        observed_seconds, paused_seconds, total_distance_meters,
    };
    use crate::tracking::{GeoPoint, PositionSample, SessionId};
    use std::f64::consts::PI;

    fn sample(session_id: SessionId, lat: f64, lon: f64, received_at: i64) -> PositionSample {
        PositionSample::new(
            String::from("picopallo"),
            session_id,
            GeoPoint::try_new(lat, lon).unwrap(),
            received_at,
            None,
        )
    }

    #[test]
    fn zero_or_one_samples_produce_no_segments() {
        let empty: Vec<PositionSample> = Vec::new();
        let single = vec![sample(13, 45.0628, 7.7620, 100)];

        assert!(consecutive_segments(&empty).is_empty());
        assert!(consecutive_segments(&single).is_empty());
    }

    #[test]
    fn two_samples_produce_one_segment() {
        let samples = vec![
            sample(13, 45.0628, 7.7620, 100),
            sample(13, 45.1000, 7.8000, 130),
        ];

        let segments = consecutive_segments(&samples);

        assert_eq!(segments.len(), 1);

        let segment = segments[0];

        assert_eq!(segment.session_id(), 13);
        assert_eq!(segment.start(), GeoPoint::try_new(45.0628, 7.7620).unwrap());
        assert_eq!(segment.end(), GeoPoint::try_new(45.1000, 7.8000).unwrap());
        assert_eq!(segment.started_at(), 100);
        assert_eq!(segment.ended_at(), 130);
        assert_eq!(segment.duration_seconds(), 30);

        let expected_distance = haversine_distance(segment.start(), segment.end());

        assert!((segment.distance_meters() - expected_distance).abs() < 0.000_001);
    }

    // testiamo che due sessioni differenti non vengano collegate
    #[test]
    fn does_not_connect_different_sessions() {
        let samples = vec![
            sample(13, 45.0628, 7.7620, 100),
            sample(21, 45.1000, 7.8000, 130),
        ];

        assert!(consecutive_segments(&samples).is_empty());
    }

    /// timestamp uguali devono produrre durata zero
    /// timestamp negativi producono durata negativa :
    #[test]
    fn does_not_create_non_increasing_segments() {
        for timestamp in [100, 90] {
            let samples = vec![
                sample(13, 45.0628, 7.7620, 100),
                sample(13, 45.1000, 7.8000, timestamp),
            ];

            assert!(consecutive_segments(&samples).is_empty());
        }
    }

    #[test]
    fn three_samples_produce_two_segments() {
        let samples = vec![
            sample(13, 44.0000, 7.0000, 100),
            sample(13, 44.5000, 7.5000, 130),
            sample(13, 45.0000, 8.0000, 170),
        ];

        let segments = consecutive_segments(&samples);

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].started_at(), 100);
        assert_eq!(segments[0].ended_at(), 130);

        assert_eq!(segments[1].started_at(), 130);
        assert_eq!(segments[1].ended_at(), 170);
    }

    #[test]
    fn session_change_breaks_only_the_cross_session_segment() {
        let samples = vec![
            sample(13, 44.0000, 7.0000, 100),
            sample(13, 44.5000, 7.5000, 130),
            sample(21, 45.0000, 8.0000, 140),
            sample(21, 45.5000, 8.5000, 170),
        ];

        let segments = consecutive_segments(&samples);

        assert_eq!(segments.len(), 2);

        assert_eq!(segments[0].session_id(), 13);
        assert_eq!(segments[0].started_at(), 100);
        assert_eq!(segments[0].ended_at(), 130);

        assert_eq!(segments[1].session_id(), 21);
        assert_eq!(segments[1].started_at(), 140);
        assert_eq!(segments[1].ended_at(), 170);
    }

    #[test]
    fn gap_longer_than_max_produces_no_segment() {
        let started_at = 100;

        let samples = vec![
            sample(13, 45.0000, 7.0000, started_at),
            sample(13, 45.1000, 7.1000, started_at + MAX_GAP_SECONDS + 1),
        ];

        assert!(consecutive_segments(&samples).is_empty());
    }

    #[test]
    fn gap_equal_to_max_produces_segment() {
        let started_at = 100;

        let samples = vec![
            sample(13, 45.0000, 7.0000, started_at),
            sample(13, 45.1000, 7.1000, started_at + MAX_GAP_SECONDS),
        ];

        let segments = consecutive_segments(&samples);

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].duration_seconds(), MAX_GAP_SECONDS);
    }

    #[test]
    fn same_point_has_zero_distance() {
        let point = GeoPoint::try_new(45.0628, 7.7620).unwrap();

        let distance = haversine_distance(point, point);

        assert_eq!(distance, 0.0);
    }

    #[test]
    fn quarter_equator_is_quarter_circumference() {
        let start = GeoPoint::try_new(0.0, 0.0).unwrap();
        let end = GeoPoint::try_new(0.0, 90.0).unwrap();

        let distance = haversine_distance(start, end);
        let expected = EARTH_RADIUS_METERS * PI / 2.0;

        assert!((distance - expected).abs() < 0.000_001);
    }

    #[test]
    fn equator_to_north_pole_is_quarter_circumference() {
        let start = GeoPoint::try_new(0.0, 0.0).unwrap();
        let end = GeoPoint::try_new(90.0, 0.0).unwrap();

        let distance = haversine_distance(start, end);
        let expected = EARTH_RADIUS_METERS * PI / 2.0;

        assert!((distance - expected).abs() < 0.000_001);
    }

    #[test]
    fn unchanged_position_is_not_movement() {
        let samples = vec![
            sample(13, 45.0628, 7.7620, 100),
            sample(13, 45.0628, 7.7620, 130),
        ];

        let segments = consecutive_segments(&samples);

        assert_eq!(segments.len(), 1);

        let segment = segments[0];

        assert_eq!(segment.distance_meters(), 0.0);
        assert!(!segment.changed_position());
    }

    #[test]
    fn movement_over_threshold_is_movement() {
        let two_meters_in_degrees = (2.0 / EARTH_RADIUS_METERS).to_degrees();

        let samples = vec![
            sample(13, 0.0, 0.0, 100),
            sample(13, 0.0, two_meters_in_degrees, 130),
        ];

        let segments = consecutive_segments(&samples);

        assert_eq!(segments.len(), 1);

        let segment = segments[0];

        assert!((segment.distance_meters() - 2.0).abs() < 0.000_001);
        assert!(segment.changed_position());
    }

    #[test]
    fn movement_below_threshold_is_not_movement() {
        let half_meter_in_degrees = (0.5 / EARTH_RADIUS_METERS).to_degrees();

        let samples = vec![
            sample(13, 0.0, 0.0, 100),
            sample(13, 0.0, half_meter_in_degrees, 130),
        ];

        let segments = consecutive_segments(&samples);

        assert_eq!(segments.len(), 1);

        let segment = segments[0];

        assert!((segment.distance_meters() - 0.5).abs() < 0.000_001);
        assert!(!segment.changed_position());
    }

    #[test]
    fn empty_segments_have_zero_total_distance() {
        assert_eq!(total_distance_meters(&[]), 0.0);
    }

    #[test]
    fn total_distance_is_the_sum_of_segment_distances() {
        let samples = vec![
            sample(13, 44.0000, 7.0000, 100),
            sample(13, 44.5000, 7.5000, 130),
            sample(13, 45.0000, 8.0000, 160),
        ];

        let segments = consecutive_segments(&samples);
        let expected = segments[0].distance_meters() + segments[1].distance_meters();
        let total = total_distance_meters(&segments);

        assert!((total - expected).abs() < 0.000_001);
    }

    #[test]
    fn observed_seconds_sums_segment_durations() {
        let samples = vec![
            sample(13, 44.0, 7.0, 100),
            sample(13, 44.5, 7.5, 130),
            sample(13, 45.0, 8.0, 170),
        ];

        let segments = consecutive_segments(&samples);

        assert_eq!(observed_seconds(&segments), 70);
    }

    #[test]
    fn stationary_run_at_threshold_counts_as_full_pause() {
        let samples = vec![
            sample(13, 45.0, 7.0, 100),
            sample(13, 45.0, 7.0, 160),
            sample(13, 45.0, 7.0, 220),
            sample(13, 45.0, 7.0, 280),
        ];

        let segments = consecutive_segments(&samples);

        assert_eq!(paused_seconds(&segments), 180);
    }

    #[test]
    fn separate_short_stationary_runs_do_not_form_a_pause() {
        let samples = vec![
            sample(13, 45.0, 7.0, 100),
            sample(13, 45.0, 7.0, 160),
            sample(13, 45.0, 7.0, 220),
            sample(13, 46.0, 8.0, 280),
            sample(13, 46.0, 8.0, 340),
            sample(13, 46.0, 8.0, 400),
        ];

        let segments = consecutive_segments(&samples);

        assert_eq!(paused_seconds(&segments), 0);
    }

    #[test]
    fn gap_separates_short_stationary_runs() {
        let samples = vec![
            sample(13, 45.0, 7.0, 100),
            sample(13, 45.0, 7.0, 160),
            sample(13, 45.0, 7.0, 220),
            sample(13, 45.0, 7.0, 400),
            sample(13, 45.0, 7.0, 460),
            sample(13, 45.0, 7.0, 520),
        ];

        let segments = consecutive_segments(&samples);

        assert_eq!(segments.len(), 4);

        assert_eq!(paused_seconds(&segments), 0);
    }

    #[test]
    fn non_increasing_timestamp_separates_stationary_runs() {
        let samples = vec![
            sample(13, 45.0, 7.0, 100),
            sample(13, 45.0, 7.0, 160),
            sample(13, 45.0, 7.0, 220),
            sample(13, 46.0, 8.0, 220),
            sample(13, 46.0, 8.0, 280),
            sample(13, 46.0, 8.0, 340),
        ];

        let segments = consecutive_segments(&samples);

        assert_eq!(segments.len(), 4);
        assert_eq!(paused_seconds(&segments), 0);
    }

    #[test]
    fn session_change_separates_stationary_runs() {
        let samples = vec![
            sample(13, 45.0, 7.0, 100),
            sample(13, 45.0, 7.0, 160),
            sample(13, 45.0, 7.0, 220),
            sample(21, 45.0, 7.0, 220),
            sample(21, 45.0, 7.0, 280),
            sample(21, 45.0, 7.0, 340),
        ];

        let segments = consecutive_segments(&samples);

        assert_eq!(segments.len(), 4);

        assert_eq!(paused_seconds(&segments), 0);
    }
}
