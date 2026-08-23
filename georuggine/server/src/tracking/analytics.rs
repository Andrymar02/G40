use super::{MovementSegment, PositionSample};

/// Costruisce segmenti tra campioni adiacenti nell'ordine ricevuto.
///
/// Scarta le coppie con session_id diverso o con received_at
/// non strettamente crescente. 
/// ATTENZIONE: La funzione non riordina i campioni.
pub fn consecutive_segments(samples: &[PositionSample]) -> Vec<MovementSegment> {
    let mut segments = Vec::new();

    for coppia in samples.windows(2) {
        let start = &coppia[0];
        let end = &coppia[1];

        if start.session_id() != end.session_id() || end.received_at() <= start.received_at() {
            continue;
        }

        segments.push(MovementSegment::new(
            start.session_id(),
            start.point(),
            end.point(),
            start.received_at(),
            end.received_at(),
        ));
    }

    segments

    // if samples.len() < 2 {
    //     return Vec::new();
    // }
}

#[cfg(test)]
mod tests {
    use super::consecutive_segments;
    use crate::tracking::{GeoPoint, PositionSample};

    fn sample(session_id: i64, lat: f64, lon: f64, received_at: i64) -> PositionSample {
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
    }

    /// testiamo che due sessioni differenti non vengano collegate
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
    fn does_not_create_not_increasing_segments() {
        for timestamp in [100, 90] {
            let samples = vec![
                sample(13, 45.0628, 7.7620, 100),
                sample(13, 45.1000, 7.8000, timestamp),
            ];

            assert!(consecutive_segments(&samples).is_empty());
        }
    }

    #[test]
    fn three_samples_produces_two_segments() {
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
}
