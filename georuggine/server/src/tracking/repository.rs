use super::{PositionSample, SessionId, TrackingSession};
use std::collections::{HashMap, VecDeque};

pub const HISTORY_RETENTION_SECONDS: i64 = 35 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackingError {
    InvalidPeriod,
    SessionAlreadyExists,
    SessionAlreadyActive,
    UnknownSession,
    SessionNotActive,
    InvalidSessionTimestamp,
    SessionUserMismatch,
}

pub trait PositionRepository {
    fn open_session(
        &mut self,
        session_id: SessionId,
        username: &str,
        connected_at: i64,
    ) -> Result<(), TrackingError>;

    fn close_session(
        &mut self,
        session_id: SessionId,
        disconnected_at: i64,
    ) -> Result<(), TrackingError>;

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
    sessions_by_id: HashMap<SessionId, TrackingSession>,
    retention_seconds: i64,
}

impl InMemoryPositionRepository {
    pub fn new(retention_seconds: i64) -> Self {
        Self {
            samples_by_username: HashMap::new(),
            sessions_by_id: HashMap::new(),
            retention_seconds,
        }
    }
}

impl PositionRepository for InMemoryPositionRepository {
    fn open_session(
        &mut self,
        session_id: SessionId,
        username: &str,
        connected_at: i64,
    ) -> Result<(), TrackingError> {
        if self.sessions_by_id.contains_key(&session_id) {
            return Err(TrackingError::SessionAlreadyExists);
        }

        let username_already_active = self
            .sessions_by_id
            .values()
            .any(|session| session.username() == username && session.disconnected_at().is_none());

        if username_already_active {
            return Err(TrackingError::SessionAlreadyActive);
        }

        let session = TrackingSession::new(session_id, username.to_owned(), connected_at);

        self.sessions_by_id.insert(session_id, session);

        Ok(())
    }

    fn close_session(
        &mut self,
        session_id: SessionId,
        disconnected_at: i64,
    ) -> Result<(), TrackingError> {
        let session = self
            .sessions_by_id
            .get_mut(&session_id)
            .ok_or(TrackingError::UnknownSession)?;

        if session.disconnected_at().is_some() {
            return Err(TrackingError::SessionNotActive);
        }

        if disconnected_at < session.connected_at() {
            return Err(TrackingError::InvalidSessionTimestamp);
        }

        session.close(disconnected_at);

        Ok(())
    }

    fn insert_sample(&mut self, sample: &PositionSample) -> Result<(), TrackingError> {
        let session = self
            .sessions_by_id
            .get(&sample.session_id())
            .ok_or(TrackingError::UnknownSession)?;

        if session.username() != sample.username() {
            return Err(TrackingError::SessionUserMismatch);
        }

        if session.disconnected_at().is_some() {
            return Err(TrackingError::SessionNotActive);
        }

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

    fn open_session_for_sample(repository: &mut impl PositionRepository, sample: &PositionSample) {
        repository
            .open_session(sample.session_id(), sample.username(), sample.received_at())
            .unwrap();
    }

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

        open_session_for_sample(&mut repository, &sample);

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

        open_session_for_sample(&mut repository, &at_start);
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

        open_session_for_sample(&mut repository, &old_sample);
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

        open_session_for_sample(&mut repository, &pincopallo_old);
        repository.insert_sample(&pincopallo_old).unwrap();

        open_session_for_sample(&mut repository, &francesco_current);
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

        open_session_for_sample(&mut repository, &pincopallo_old);
        repository.insert_sample(&pincopallo_old).unwrap();

        open_session_for_sample(&mut repository, &francesco_current);
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

        open_session_for_sample(&mut repository, &pincopallo_old);

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

        open_session_for_sample(&mut repository, &sample);

        let samples = save_and_load(&mut repository, &sample);

        assert_eq!(samples, vec![sample]);
    }

    #[test]
    fn opening_same_session_id_twice_is_rejected() {
        let mut repository = InMemoryPositionRepository::new(1_000);

        assert_eq!(repository.open_session(13, "pincopallo", 100), Ok(()));

        assert_eq!(
            repository.open_session(13, "francesco", 150),
            Err(TrackingError::SessionAlreadyExists)
        );
    }

    #[test]
    fn second_active_session_for_same_user_is_rejected() {
        let mut repository = InMemoryPositionRepository::new(1_000);

        assert_eq!(repository.open_session(13, "pincopallo", 100), Ok(()));

        assert_eq!(
            repository.open_session(14, "pincopallo", 150),
            Err(TrackingError::SessionAlreadyActive)
        );
    }

    #[test]
    fn closing_unknown_session_is_rejected() {
        let mut repository = InMemoryPositionRepository::new(1_000);

        assert_eq!(
            repository.close_session(999, 200),
            Err(TrackingError::UnknownSession)
        );
    }

    #[test]
    fn closed_session_allows_new_session_for_same_user() {
        let mut repository = InMemoryPositionRepository::new(1_000);

        repository.open_session(13, "pincopallo", 100).unwrap();
        repository.close_session(13, 150).unwrap();

        assert_eq!(repository.open_session(14, "pincopallo", 200), Ok(()));
    }

    #[test]
    fn closing_already_closed_session_is_rejected() {
        let mut repository = InMemoryPositionRepository::new(1_000);

        repository.open_session(13, "pincopallo", 100).unwrap();
        repository.close_session(13, 150).unwrap();

        assert_eq!(
            repository.close_session(13, 200),
            Err(TrackingError::SessionNotActive)
        );
    }

    #[test]
    fn closing_before_connection_is_rejected() {
        let mut repository = InMemoryPositionRepository::new(1_000);

        repository.open_session(13, "pincopallo", 100).unwrap();

        assert_eq!(
            repository.close_session(13, 99),
            Err(TrackingError::InvalidSessionTimestamp)
        );
    }

    #[test]
    fn sample_for_unknown_session_is_rejected() {
        let mut repository = InMemoryPositionRepository::new(1_000);

        let sample = PositionSample::new(
            String::from("pincopallo"),
            42,
            GeoPoint::try_new(45.0628, 7.7620).unwrap(),
            150,
            None,
        );

        assert_eq!(
            repository.insert_sample(&sample),
            Err(TrackingError::UnknownSession)
        );

        assert!(
            repository
                .samples_for_period("pincopallo", 100, 200)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn sample_for_closed_session_is_rejected() {
        let mut repository = InMemoryPositionRepository::new(1_000);

        repository.open_session(42, "alice", 100).unwrap();
        repository.close_session(42, 140).unwrap();

        let sample = PositionSample::new(
            String::from("alice"),
            42,
            GeoPoint::try_new(45.0628, 7.7620).unwrap(),
            150,
            None,
        );

        assert_eq!(
            repository.insert_sample(&sample),
            Err(TrackingError::SessionNotActive)
        );

        assert!(
            repository
                .samples_for_period("alice", 100, 200)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn sample_username_must_match_session_username() {
        let mut repository = InMemoryPositionRepository::new(1_000);

        repository.open_session(42, "alice", 100).unwrap();

        let sample = PositionSample::new(
            String::from("francesco"),
            42,
            GeoPoint::try_new(45.0628, 7.7620).unwrap(),
            150,
            None,
        );

        assert_eq!(
            repository.insert_sample(&sample),
            Err(TrackingError::SessionUserMismatch)
        );

        assert!(
            repository
                .samples_for_period("francesco", 100, 200)
                .unwrap()
                .is_empty()
        );
    }
}
