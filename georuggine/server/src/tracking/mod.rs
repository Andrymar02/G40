mod analytics;
mod model;
mod period;
mod repository;
mod service;

pub use analytics::{
    MAX_GAP_SECONDS, PAUSE_THRESHOLD_SECONDS, average_moving_speed, average_overall_speed,
    build_movement_report, consecutive_segments, haversine_distance, moving_seconds,
    observed_seconds, paused_seconds, total_distance_meters, unobserved_seconds,
};
pub use model::{
    GeoPoint, GeoPointError, MOVEMENT_THRESHOLD_METERS, MovementReport, MovementSegment,
    PositionSample, ReportPeriod, SessionId, TrackingSession,
};
pub use repository::{
    HISTORY_RETENTION_SECONDS, InMemoryPositionRepository, PositionRepository, TrackingError,
};

pub use period::resolve_report_period;

pub use service::{SharedTracking, TrackingService, new_tracking};
