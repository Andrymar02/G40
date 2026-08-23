mod analytics;
mod model;

pub use analytics::consecutive_segments;
pub use model::{GeoPoint, GeoPointError, MovementSegment, PositionSample};
