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
}
