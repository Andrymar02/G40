mod analytics;
mod model;

pub use analytics::{MAX_GAP_SECONDS, consecutive_segments};
pub use model::{GeoPoint, GeoPointError, MovementSegment, PositionSample};
