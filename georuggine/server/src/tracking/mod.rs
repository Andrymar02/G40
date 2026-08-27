mod analytics;
mod model;
mod repository;

pub use analytics::{
    MAX_GAP_SECONDS, PAUSE_THRESHOLD_SECONDS, consecutive_segments, haversine_distance,
    observed_seconds, paused_seconds, total_distance_meters,
};
pub use model::{
    GeoPoint, GeoPointError, MOVEMENT_THRESHOLD_METERS, MovementSegment, PositionSample, SessionId,
    TrackingSession,
};
pub use repository::{
    HISTORY_RETENTION_SECONDS, InMemoryPositionRepository, PositionRepository, TrackingError,
};
