use super::PositionSample;
use std::collections::{HashMap, VecDeque};

pub const HISTORY_RETENTION_SECONDS: i64 = 35 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackingError {
    InvalidPeriod,
}

pub trait PositionRepository {
    fn insert_sample(&mut self, sample: &PositionSample) -> Result<(), TrackingError>;

    fn samples_for_period(
        &self,
        username: &str,
        start_utc: i64,
        end_utc: i64,
    ) -> Result<Vec<PositionSample>, TrackingError>;

    fn prune_before(&mut self, cutoff_utc: i64) -> Result<usize, TrackingError>;
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
}

impl PositionRepository for InMemoryPositionRepository {
    fn insert_sample(&mut self, sample: &PositionSample) -> Result<(), TrackingError> {
        let cutoff_utc = sample.received_at().saturating_sub(self.retention_seconds);

        self.prune_before(cutoff_utc)?;

        let samples = self
            .samples_by_username
            .entry(sample.username().to_owned())
            .or_default();

        samples.push_back(sample.clone());

        Ok(())
    }

    fn samples_for_period(
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

    fn prune_before(&mut self, cutoff_utc: i64) -> Result<usize, TrackingError> {
        let mut removed = 0;

        for samples in self.samples_by_username.values_mut() {
            while samples
                .front()
                .is_some_and(|oldest| oldest.received_at() < cutoff_utc)
            {
                samples.pop_front();
                removed += 1;
            }
        }

        Ok(removed)
    }
}

impl Default for InMemoryPositionRepository {
    fn default() -> Self {
        Self::new(HISTORY_RETENTION_SECONDS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracking::GeoPoint;

    fn save_and_load(
        repository: &mut impl PositionRepository,
        sample: &PositionSample,
    ) -> Vec<PositionSample> {
        repository.insert_sample(sample).unwrap();

        repository
            .samples_for_period(sample.username(), 0, sample.received_at() + 1)
            .unwrap()
    }

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

    #[test]
    fn prune_before_removes_old_samples_from_all_users() {
        let mut repository = InMemoryPositionRepository::new(1_000);

        let pincopallo_old = PositionSample::new(
            String::from("pincopallo"),
            13,
            GeoPoint::try_new(45.0628, 7.7620).unwrap(),
            100,
            None,
        );

        let francesco_current = PositionSample::new(
            String::from("francesco"),
            14,
            GeoPoint::try_new(45.0630, 7.7630).unwrap(),
            150,
            None,
        );

        repository.insert_sample(&pincopallo_old).unwrap();
        repository.insert_sample(&francesco_current).unwrap();

        let removed = repository.prune_before(120).unwrap();

        assert_eq!(removed, 1);
        assert!(
            repository
                .samples_for_period("pincopallo", 0, 300)
                .unwrap()
                .is_empty()
        );

        assert_eq!(
            repository.samples_for_period("francesco", 0, 300).unwrap(),
            vec![francesco_current]
        );
    }

    #[test]
    fn insertion_prunes_expired_samples_for_all_users() {
        let mut repository = InMemoryPositionRepository::new(100);

        let pincopallo_old = PositionSample::new(
            String::from("pincopallo"),
            13,
            GeoPoint::try_new(45.0628, 7.7620).unwrap(),
            100,
            None,
        );

        let francesco_current = PositionSample::new(
            String::from("francesco"),
            14,
            GeoPoint::try_new(45.0630, 7.7630).unwrap(),
            201,
            None,
        );

        repository.insert_sample(&pincopallo_old).unwrap();
        repository.insert_sample(&francesco_current).unwrap();

        let pincopallo_samples = repository.samples_for_period("pincopallo", 0, 300).unwrap();

        assert!(pincopallo_samples.is_empty());
    }

    #[test]
    fn default_repository_uses_thirty_five_day_retention() {
        let mut repository = InMemoryPositionRepository::default();

        let thirty_five_days = 35 * 24 * 60 * 60;

        let pincopallo_old = PositionSample::new(
            String::from("pincopallo"),
            13,
            GeoPoint::try_new(45.0628, 7.7620).unwrap(),
            100,
            None,
        );

        let current_timestamp = 100 + thirty_five_days + 1;

        let pincopallo_current = PositionSample::new(
            String::from("pincopallo"),
            13,
            GeoPoint::try_new(45.0630, 7.7630).unwrap(),
            current_timestamp,
            None,
        );

        repository.insert_sample(&pincopallo_old).unwrap();
        repository.insert_sample(&pincopallo_current).unwrap();

        let samples = repository
            .samples_for_period("pincopallo", 0, current_timestamp + 1)
            .unwrap();

        assert_eq!(samples, vec![pincopallo_current]);
    }

    #[test]
    fn sample_can_be_saved_and_loaded_throug_repository_trait() {
        let mut repository = InMemoryPositionRepository::new(1_000);

        let sample = PositionSample::new(
            String::from("pincopallo"),
            42,
            GeoPoint::try_new(45.0628, 7.7620).unwrap(),
            150,
            None,
        );

        let samples = save_and_load(&mut repository, &sample);

        assert_eq!(samples, vec![sample]);
    }
}
