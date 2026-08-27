/// Distanza oltre la quale una posizione è considerata cambiata
pub const MOVEMENT_THRESHOLD_METERS: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeoPointError {
    NonFinite,
    LatitudeOutOfRange,
    LongitudeOutOfRange,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoPoint {
    lat: f64,
    lon: f64,
}

impl GeoPoint {
    pub fn try_new(lat: f64, lon: f64) -> Result<Self, GeoPointError> {
        if !lat.is_finite() || !lon.is_finite() {
            return Err(GeoPointError::NonFinite);
        }

        if lat.abs() > 90.0 {
            return Err(GeoPointError::LatitudeOutOfRange);
        }

        if lon.abs() > 180.0 {
            return Err(GeoPointError::LongitudeOutOfRange);
        }

        Ok(Self { lat, lon })
    }

    pub fn lat(&self) -> f64 {
        self.lat
    }

    pub fn lon(&self) -> f64 {
        self.lon
    }
}

pub type SessionId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackingSession {
    id: SessionId,
    username: String,
    connected_at: i64,
    disconnected_at: Option<i64>,
}

impl TrackingSession {
    pub fn new(id: SessionId, username: String, connected_at: i64) -> Self {
        Self {
            id,
            username,
            connected_at,
            disconnected_at: None,
        }
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn connected_at(&self) -> i64 {
        self.connected_at
    }

    pub fn disconnected_at(&self) -> Option<i64> {
        self.disconnected_at
    }

    pub fn close(&mut self, disconnected_at: i64) {
        self.disconnected_at = Some(disconnected_at);
    }
}

/// Posizione ricevuta dal server.
///
/// received_at è il timestamp del server usato per l'ordinamento.
/// client_timestamp è opzionale e proviene dal client.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionSample {
    username: String,
    session_id: SessionId,
    point: GeoPoint,
    received_at: i64,
    client_timestamp: Option<i64>,
}

impl PositionSample {
    pub fn new(
        username: String,
        session_id: SessionId,
        point: GeoPoint,
        received_at: i64,
        client_timestamp: Option<i64>,
    ) -> Self {
        Self {
            username,
            session_id,
            point,
            received_at,
            client_timestamp,
        }
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn point(&self) -> GeoPoint {
        self.point
    }

    pub fn received_at(&self) -> i64 {
        self.received_at
    }

    pub fn client_timestamp(&self) -> Option<i64> {
        self.client_timestamp
    }
}

/// Movimento tra due posizioni consecutive della stessa sessione.
///
/// I segmenti prodotti dall'analisi hanno timestamp strettamente crescenti.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MovementSegment {
    session_id: SessionId,
    start: GeoPoint,
    end: GeoPoint,
    started_at: i64,
    ended_at: i64,
    distance_meters: f64,
}

impl MovementSegment {
    /// pub(crate) === new può essere chiamato soltanto all'interno di questo crate (server)
    /// i moduli esterni possono leggere il segmento ma non possono crearne uno arbitrariamente
    pub(crate) fn new(
        session_id: SessionId,
        start: GeoPoint,
        end: GeoPoint,
        started_at: i64,
        ended_at: i64,
        distance_meters: f64,
    ) -> Self {
        Self {
            session_id,
            start,
            end,
            started_at,
            ended_at,
            distance_meters,
        }
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn start(&self) -> GeoPoint {
        self.start
    }

    pub fn end(&self) -> GeoPoint {
        self.end
    }

    pub fn started_at(&self) -> i64 {
        self.started_at
    }

    pub fn ended_at(&self) -> i64 {
        self.ended_at
    }

    pub fn duration_seconds(&self) -> i64 {
        // durata calcolata ogni volta (non vedo il senso nel salvarla)
        self.ended_at - self.started_at
    }

    pub fn distance_meters(&self) -> f64 {
        self.distance_meters
    }

    pub fn changed_position(&self) -> bool {
        self.distance_meters > MOVEMENT_THRESHOLD_METERS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_a_valid_point() {
        let point = GeoPoint::try_new(45.0628, 7.6620).unwrap();

        assert_eq!(point.lat(), 45.0628);
        assert_eq!(point.lon(), 7.6620);
    }

    #[test]
    fn accept_boundary_coordinates() {
        assert!(GeoPoint::try_new(-90.0, -180.0).is_ok());
        assert!(GeoPoint::try_new(90.0, 180.0).is_ok());
    }

    #[test]
    fn reject_non_finite_coordinates() {
        assert_eq!(
            GeoPoint::try_new(f64::NAN, 7.0),
            Err(GeoPointError::NonFinite)
        );
        assert_eq!(
            GeoPoint::try_new(45.0, f64::INFINITY),
            Err(GeoPointError::NonFinite)
        );
    }

    #[test]
    fn rejects_latitude_out_of_range() {
        assert_eq!(
            GeoPoint::try_new(90.1, 7.0),
            Err(GeoPointError::LatitudeOutOfRange)
        );
    }

    #[test]
    fn rejects_longitude_out_of_range() {
        assert_eq!(
            GeoPoint::try_new(45.0, -180.1),
            Err(GeoPointError::LongitudeOutOfRange)
        );
    }

    #[test]
    fn creates_position_sample() {
        let point = GeoPoint::try_new(45.0628, 7.7620).unwrap();

        let sample = PositionSample::new(
            String::from("pincopallo"),
            42,
            point,
            1_755_000_010,
            Some(1_755_000_000),
        );

        assert_eq!(sample.username(), "pincopallo");
        assert_eq!(sample.session_id(), 42);
        assert_eq!(sample.point(), point);
        assert_eq!(sample.received_at(), 1_755_000_010);
        assert_eq!(sample.client_timestamp(), Some(1_755_000_000));
    }

    #[test]
    fn accepts_missing_client_timestamp() {
        let point = GeoPoint::try_new(45.0628, 7.7620).unwrap();

        let sample =
            PositionSample::new(String::from("pincopallo"), 43, point, 1_755_000_010, None);

        assert_eq!(sample.client_timestamp(), None);
        assert_eq!(sample.received_at(), 1_755_000_010);
    }

    #[test]
    fn movement_at_threshold_is_not_movement() {
        let point = GeoPoint::try_new(45.0628, 7.7620).unwrap();

        let segment = MovementSegment::new(13, point, point, 100, 130, MOVEMENT_THRESHOLD_METERS);

        assert!(!segment.changed_position());
    }
}
