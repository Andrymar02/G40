mod analytics;
mod model;
mod repository;

pub use analytics::{MAX_GAP_SECONDS, consecutive_segments, haversine_distance};
pub use model::{
    GeoPoint, GeoPointError, MOVEMENT_THRESHOLD_METERS, MovementSegment, PositionSample, SessionId,
};
pub use repository::{
    HISTORY_RETENTION_SECONDS, InMemoryPositionRepository, PositionRepository, TrackingError,
};
