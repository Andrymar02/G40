use super::PositionSample;
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackingError {
    InvalidPeriod,
}

pub struct InMemoryPositionRepository {
    samples_by_username: HashMap<String, VecDeque<PositionSample>>,
    retention_seconds: i64,
}

impl InMemoryPositionRepository {
    pub fn new(retention_seconds: i64) -> Self {
        Self {
            samples_by_username: HashMap::new(),
            retention_seconds,
        }
    }

    pub fn samples_for_period(
        &self,
        username: &str,
        start_utc: i64,
        end_utc: i64,
    ) -> Result<Vec<PositionSample>, TrackingError> {
        if start_utc >= end_utc {
            return Err(TrackingError::InvalidPeriod);
        }

        let samples = self
            .samples_by_username
            .get(username)
            .map(|samples| {
                samples
                    .iter()
                    .filter(|sample| {
                        sample.received_at() >= start_utc && sample.received_at() < end_utc
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        Ok(samples)
    }

    pub fn insert_sample(&mut self, sample: &PositionSample) -> Result<(), TrackingError> {
        let cutoff_utc = sample.received_at().saturating_sub(self.retention_seconds);

        let samples = self
            .samples_by_username
            .entry(sample.username().to_owned())
            .or_default();

        samples.push_back(sample.clone());

        while samples
            .front()
            .is_some_and(|oldest| oldest.received_at() < cutoff_utc)
        {
            samples.pop_front();
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracking::GeoPoint;

    #[test]
    fn empty_repository_returns_no_samples() {
        let repository = InMemoryPositionRepository::new(1_000);

        let samples = repository
            .samples_for_period("pincopallo", 100, 200)
            .unwrap();

        assert!(samples.is_empty());
    }

    #[test]
    fn inserted_sample_can_be_retrieved() {
        let mut repository = InMemoryPositionRepository::new(1_000);

        let sample = PositionSample::new(
            String::from("pincopallo"),
            42,
            GeoPoint::try_new(45.0628, 7.7620).unwrap(),
            150,
            None,
        );

        repository.insert_sample(&sample).unwrap();

        let samples = repository
            .samples_for_period("pincopallo", 100, 200)
            .unwrap();

        assert_eq!(samples, vec![sample]);
    }

    #[test]
    fn period_includes_start_and_excludes_end() {
        let mut repository = InMemoryPositionRepository::new(1_000);

        let at_start = PositionSample::new(
            String::from("pincopallo"),
            42,
            GeoPoint::try_new(45.0628, 7.7620).unwrap(),
            100,
            None,
        );

        let at_end = PositionSample::new(
            String::from("pincopallo"),
            42,
            GeoPoint::try_new(45.0630, 7.7630).unwrap(),
            200,
            None,
        );

        repository.insert_sample(&at_start).unwrap();
        repository.insert_sample(&at_end).unwrap();

        let samples = repository
            .samples_for_period("pincopallo", 100, 200)
            .unwrap();

        assert_eq!(samples, vec![at_start]);
    }

    #[test]
    fn rejects_non_increasing_period() {
        let repository = InMemoryPositionRepository::new(1_000);

        assert_eq!(
            repository.samples_for_period("pincopallo", 100, 100),
            Err(TrackingError::InvalidPeriod)
        );

        assert_eq!(
            repository.samples_for_period("pincopallo", 200, 100),
            Err(TrackingError::InvalidPeriod)
        );
    }

    #[test]
    fn insertion_prunes_samples_older_than_retention() {
        let mut repository = InMemoryPositionRepository::new(100);

        let old_sample = PositionSample::new(
            String::from("pincopallo"),
            42,
            GeoPoint::try_new(45.0628, 7.7620).unwrap(),
            100,
            None,
        );

        let current_sample = PositionSample::new(
            String::from("pincopallo"),
            42,
            GeoPoint::try_new(45.0630, 7.7630).unwrap(),
            201,
            None,
        );

        repository.insert_sample(&old_sample).unwrap();
        repository.insert_sample(&current_sample).unwrap();

        let samples = repository.samples_for_period("pincopallo", 0, 300).unwrap();

        assert_eq!(samples, vec![current_sample]);
    }
}
