use std::sync::Arc;
use tokio::sync::RwLock;

use super::{
    FilePositionRepository, HISTORY_RETENTION_SECONDS, MovementReport, PositionRepository,
    PositionSample, ReportPeriod, SessionId, TrackingError, build_movement_report,
    resolve_report_period,
};

pub struct TrackingService<R> {
    repository: RwLock<R>,
}

impl<R: PositionRepository> TrackingService<R> {
    pub fn new(repository: R) -> Self {
        Self {
            repository: RwLock::new(repository),
        }
    }

    pub async fn movement_report(
        &self,
        username: &str,
        period: ReportPeriod,
        now_utc: i64,
    ) -> Result<MovementReport, TrackingError> {
        let (start_utc, end_utc) = resolve_report_period(period, now_utc)?;

        let report_samples = {
            let repository = self.repository.read().await;

            let mut period_samples = repository.samples_for_period(username, start_utc, end_utc)?;

            if !period_samples.is_empty()
                && let Some(previous_sample) =
                    repository.latest_sample_before(username, start_utc)?
            {
                period_samples.insert(0, previous_sample);
            }

            period_samples
        };

        Ok(build_movement_report(
            username,
            start_utc,
            end_utc,
            &report_samples,
        ))
    }

    pub async fn open_session(
        &self,
        session_id: SessionId,
        username: &str,
        connected_at: i64,
    ) -> Result<(), TrackingError> {
        let mut repository = self.repository.write().await;

        repository.open_session(session_id, username, connected_at)
    }

    pub async fn record_position(&self, sample: PositionSample) -> Result<(), TrackingError> {
        let mut repository = self.repository.write().await;

        repository.insert_sample(&sample)
    }

    pub async fn close_session(
        &self,
        session_id: SessionId,
        disconnected_at: i64,
    ) -> Result<(), TrackingError> {
        let mut repository = self.repository.write().await;

        repository.close_session(session_id, disconnected_at)
    }
}

/// Il tipo usato dal server vero: lo storico posizioni deve sopravvivere a
/// un riavvio (richiesto dal professore in sede di revisione del progetto,
/// oltre che utile di per sé), quindi qui il repository è quello persistito
/// su file, non `InMemoryPositionRepository` — usato invece direttamente nei
/// test che non hanno bisogno di sopravvivere a nulla (vedi `repository.rs`
/// e il test qui sotto).
pub type SharedTracking = Arc<TrackingService<FilePositionRepository>>;

/// Carica (o crea, se `path` non esiste ancora) lo storico posizioni dal
/// file indicato. Fallisce se il file esiste ma è illeggibile o corrotto —
/// vedi `FilePositionRepository::load` — cosi' un server che parte non
/// rischia di ripartire silenziosamente da uno storico vuoto.
pub fn new_tracking(path: &str) -> Result<SharedTracking, String> {
    let repository = FilePositionRepository::load(path, HISTORY_RETENTION_SECONDS)?;
    Ok(Arc::new(TrackingService::new(repository)))
}

#[cfg(test)]
mod tests {
    use crate::tracking::{
        GeoPoint, InMemoryPositionRepository, PositionSample, ReportPeriod, TrackingError,
        TrackingService,
    };
    use std::sync::Arc;

    #[tokio::test]
    async fn empty_repository_produces_empty_report_for_custom_period() {
        let service = TrackingService::new(InMemoryPositionRepository::default());

        let report = service
            .movement_report(
                "pincopallo",
                ReportPeriod::Custom {
                    start_utc: 100,
                    end_utc: 400,
                },
                999,
            )
            .await
            .unwrap();

        assert_eq!(report.username, "pincopallo");
        assert_eq!(report.period_start, 100);
        assert_eq!(report.period_end, 400);
        assert_eq!(report.sample_count, 0);
        assert_eq!(report.unobserved_seconds, 300);
    }

    #[tokio::test]
    async fn service_records_position_for_open_session() {
        let service = TrackingService::new(InMemoryPositionRepository::default());

        service.open_session(42, "pincopallo", 100).await.unwrap();

        let sample = PositionSample::new(
            String::from("pincopallo"),
            42,
            GeoPoint::try_new(45.0628, 7.6620).unwrap(),
            150,
            None,
        );

        service.record_position(sample).await.unwrap();

        let report = service
            .movement_report(
                "pincopallo",
                ReportPeriod::Custom {
                    start_utc: 100,
                    end_utc: 200,
                },
                999,
            )
            .await
            .unwrap();
        assert_eq!(report.sample_count, 1);
        assert_eq!(report.coverage_start, Some(150));
        assert_eq!(report.coverage_end, Some(150));
    }

    #[tokio::test]
    async fn service_rejects_positions_after_session_is_closed() {
        let service = TrackingService::new(InMemoryPositionRepository::default());

        service.open_session(42, "pincopallo", 100).await.unwrap();

        service.close_session(42, 200).await.unwrap();

        let sample = PositionSample::new(
            String::from("pincopallo"),
            42,
            GeoPoint::try_new(45.0628, 7.6620).unwrap(),
            250,
            None,
        );

        let result = service.record_position(sample).await;

        assert_eq!(result, Err(TrackingError::SessionNotActive));
    }

    #[tokio::test]
    async fn report_includes_segment_ending_inside_period() {
        let service = TrackingService::new(InMemoryPositionRepository::default());

        service.open_session(42, "pincopallo", 50).await.unwrap();

        let sample_before_period = PositionSample::new(
            String::from("pincopallo"),
            42,
            GeoPoint::try_new(45.0628, 7.6620).unwrap(),
            90,
            None,
        );

        let sample_inside_period = PositionSample::new(
            String::from("pincopallo"),
            42,
            GeoPoint::try_new(45.0630, 7.6620).unwrap(),
            150,
            None,
        );

        service.record_position(sample_before_period).await.unwrap();

        service.record_position(sample_inside_period).await.unwrap();

        let report = service
            .movement_report(
                "pincopallo",
                ReportPeriod::Custom {
                    start_utc: 100,
                    end_utc: 200,
                },
                999,
            )
            .await
            .unwrap();

        assert_eq!(report.observed_seconds, 60);
    }

    #[tokio::test]
    async fn cloned_tracking_handles_share_the_same_history() {
        let tracking = Arc::new(TrackingService::new(InMemoryPositionRepository::default()));
        let connection_tracking = tracking.clone();

        connection_tracking
            .open_session(42, "alice", 100)
            .await
            .unwrap();

        let sample = PositionSample::new(
            String::from("alice"),
            42,
            GeoPoint::try_new(45.0628, 7.6620).unwrap(),
            150,
            None,
        );

        connection_tracking.record_position(sample).await.unwrap();

        let report = tracking
            .movement_report(
                "alice",
                ReportPeriod::Custom {
                    start_utc: 100,
                    end_utc: 200,
                },
                999,
            )
            .await
            .unwrap();

        assert_eq!(report.sample_count, 1);
    }
}
