use super::{PositionSample, SessionId, TrackingSession};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::io::Write;

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
    /// La modifica in memoria è andata a buon fine, ma scriverla sul file di
    /// storico è fallito (disco pieno, permessi cambiati a runtime, ecc.).
    /// A differenza di `UserStore::register`, qui la mutazione **non** viene
    /// annullata: farlo per ogni operazione (apertura/chiusura sessione,
    /// inserimento campione) complicherebbe molto la logica per un caso
    /// limite raro. Il dato resta quindi valido per l'esecuzione corrente,
    /// ma un riavvio del server prima di un salvataggio riuscito lo perde —
    /// l'errore è segnalato esplicitamente a chi chiama (vedi `main.rs`),
    /// mai ignorato in silenzio.
    PersistenceFailed,
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

    fn latest_sample_before(
        &self,
        username: &str,
        before_utc: i64,
    ) -> Result<Option<PositionSample>, TrackingError>;

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

        if sample.received_at() < session.connected_at() {
            return Err(TrackingError::InvalidSessionTimestamp);
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

    fn latest_sample_before(
        &self,
        username: &str,
        before_utc: i64,
    ) -> Result<Option<PositionSample>, TrackingError> {
        let sample = self.samples_by_username.get(username).and_then(|samples| {
            samples
                .iter()
                .filter(|sample| sample.received_at() < before_utc)
                .max_by_key(|sample| sample.received_at())
                // max_by_key perché cerchiamo il campione con il timestamp maggiore tra quelli precedenti al confine, indipendentemente dall’ordine d’inserimento.
                .cloned()
        });

        Ok(sample)
    }
}

impl Default for InMemoryPositionRepository {
    fn default() -> Self {
        Self::new(HISTORY_RETENTION_SECONDS)
    }
}

/// Fotografia serializzabile dello stato del repository: solo dati, nessuna
/// logica (quella resta in `InMemoryPositionRepository`, che questo tipo
/// avvolge). Le sessioni sono salvate come `Vec` perché `TrackingSession` non
/// espone un modo per ricostruire la chiave (`SessionId`) se non tramite
/// `id()`: la si rilegge da lì al caricamento invece di duplicarla nel JSON.
#[derive(serde::Serialize, serde::Deserialize)]
struct Snapshot {
    sessions: Vec<TrackingSession>,
    samples: HashMap<String, VecDeque<PositionSample>>,
}

/// Sotto questo intervallo, campioni successivi non forzano una nuova
/// scrittura: si accumulano in memoria e vengono scritti alla prima
/// occasione utile (il campione che fa scadere l'intervallo, oppure
/// l'apertura/chiusura di una sessione, che scrivono sempre). Con un solo
/// veicolo il comportamento non cambia quasi — manda una posizione ogni 30s
/// comunque — ma con una flotta il vantaggio è che il tasso di scrittura
/// resta limitato a una ogni `MIN_PERSIST_INTERVAL_SECONDS` indipendentemente
/// da quanti veicoli mandano posizioni nello stesso istante, invece di una
/// riscrittura per ciascuno.
const MIN_PERSIST_INTERVAL_SECONDS: i64 = 30;

/// Repository dello storico posizioni persistito su file, con lo stesso
/// schema di scrittura atomica di `UserStore::save` (file temporaneo,
/// `fsync`, `rename`). Tutta la logica di consultazione/potatura resta
/// quella di `InMemoryPositionRepository`, che questo tipo avvolge e
/// riporta su disco dopo ogni apertura/chiusura di sessione, e al più ogni
/// `MIN_PERSIST_INTERVAL_SECONDS` per i campioni di posizione (vedi
/// `insert_sample` sotto).
///
/// SCELTA: si riscrive per intero lo stato a ogni scrittura, invece di
/// un log di sole aggiunte. È più semplice, coerente con `users.rs`, e a
/// questa scala (una manciata di veicoli simulati, non una flotta reale)
/// il costo resta trascurabile. Un log append-only scalerebbe meglio se lo
/// storico crescesse molto, ma renderebbe più complesso sia il
/// caricamento sia la potatura per retention.
///
/// Il trait `PositionRepository` non è asincrono (vedi sopra), quindi la
/// scrittura qui è sincrona (`std::fs`, non `tokio::fs`): per un file che
/// cresce con l'attività di pochi veicoli e si scrive al più ogni 30
/// secondi in tutto (non per utente, vedi sopra), il tempo bloccato sul
/// thread resta trascurabile anche con più veicoli.
pub struct FilePositionRepository {
    inner: InMemoryPositionRepository,
    path: String,
    /// `received_at` dell'ultimo campione che ha effettivamente scritto su
    /// disco. A 0 al primo avvio: il primissimo campione ricevuto scrive
    /// sempre, qualunque sia `MIN_PERSIST_INTERVAL_SECONDS`.
    last_persist_utc: i64,
}

impl FilePositionRepository {
    /// Carica lo stato dal file indicato, o parte vuoto se non esiste ancora
    /// — stessa convenzione di `UserStore::load`: un file **assente** è il
    /// primo avvio, un file **presente ma illeggibile o malformato** ferma
    /// l'avvio invece di ripartire silenziosamente da uno storico vuoto (che
    /// alla prima potatura o al primo salvataggio cancellerebbe per sempre i
    /// dati che erano solo temporaneamente illeggibili).
    pub fn load(path: &str, retention_seconds: i64) -> Result<Self, String> {
        let mut inner = InMemoryPositionRepository::new(retention_seconds);

        match std::fs::read_to_string(path) {
            Ok(data) => {
                let snapshot: Snapshot = serde_json::from_str(&data)
                    .map_err(|e| format!("{path} non è uno storico posizioni valido: {e}"))?;

                for mut session in snapshot.sessions {
                    // BUG CORRETTO: una sessione salvata su file ancora
                    // "aperta" (`disconnected_at: None`) veniva ricaricata
                    // così com'era. Il registro delle connessioni vere
                    // (`session::SessionRegistry`, in main.rs) riparte invece
                    // sempre vuoto a ogni avvio: se il processo precedente
                    // è terminato senza un logout pulito (kill, crash, mancanza
                    // di corrente — non c'è alcun signal handler in `main.rs`),
                    // qui restava registrata per sempre una sessione "attiva"
                    // di cui nessuno sa più nulla. Quell'utente non riusciva
                    // più a fare login: `open_session` la vedeva ancora attiva
                    // e rispondeva `SessionAlreadyActive` a ogni tentativo,
                    // fino al prossimo riavvio del server — un blocco
                    // permanente, verificato riproducendolo direttamente.
                    // Si chiude qui alla sua stessa `connected_at` (durata
                    // zero) invece di inventare un istante di disconnessione
                    // che non si conosce davvero: l'unico scopo è smettere di
                    // considerarla attiva, non produrre una durata plausibile.
                    if session.disconnected_at().is_none() {
                        let connected_at = session.connected_at();
                        session.close(connected_at);
                    }

                    inner.sessions_by_id.insert(session.id(), session);
                }
                inner.samples_by_username = snapshot.samples;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("impossibile leggere {path}: {e}")),
        }

        Ok(Self {
            inner,
            path: path.to_string(),
            last_persist_utc: 0,
        })
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            sessions: self.inner.sessions_by_id.values().cloned().collect(),
            samples: self.inner.samples_by_username.clone(),
        }
    }

    /// `persist()` seguito da un aggiornamento di `last_persist_utc`: le
    /// chiamate che devono scrivere sempre (apertura/chiusura sessione)
    /// passano da qui invece che da `persist()` direttamente, cosi' anche
    /// loro fanno ripartire la soglia di `insert_sample` — altrimenti un
    /// campione arrivato subito dopo un login riscriverebbe il file una
    /// seconda volta a distanza di pochi millisecondi.
    fn persist_and_mark(&mut self, at: i64) -> io::Result<()> {
        self.persist()?;
        self.last_persist_utc = at;
        Ok(())
    }

    /// Scrittura atomica: file temporaneo nella stessa directory (`rename`
    /// fra filesystem diversi fallirebbe con `EXDEV`), `sync_all` prima del
    /// `rename` cosi' un crash della macchina non lascia il file rinominato
    /// ma vuoto, poi `rename` — visibile per intero o non ancora, mai a
    /// metà. Stesso schema di `UserStore::write_temp`/`save`, qui sincrono.
    fn persist(&self) -> io::Result<()> {
        let json = serde_json::to_string(&self.snapshot())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let temp_path = format!("{}.tmp", self.path);

        let write_result = (|| -> io::Result<()> {
            let mut file = std::fs::File::create(&temp_path)?;
            file.write_all(json.as_bytes())?;
            file.sync_all()
        })();

        match write_result {
            Ok(()) => std::fs::rename(&temp_path, &self.path),
            Err(e) => {
                let _ = std::fs::remove_file(&temp_path);
                Err(e)
            }
        }
    }
}

impl PositionRepository for FilePositionRepository {
    fn open_session(
        &mut self,
        session_id: SessionId,
        username: &str,
        connected_at: i64,
    ) -> Result<(), TrackingError> {
        self.inner
            .open_session(session_id, username, connected_at)?;

        if self.persist_and_mark(connected_at).is_err() {
            // BUG CORRETTO: qui, a differenza di `close_session`/
            // `insert_sample`, la mutazione in memoria va annullata se il
            // salvataggio fallisce. Altrimenti la sessione appena aperta
            // resta "attiva" solo in memoria (il file su disco non l'ha mai
            // vista): ogni tentativo di login successivo per lo stesso
            // utente troverebbe `open_session` a rifiutare con
            // `SessionAlreadyActive`, un blocco permanente per un problema
            // magari solo transitorio (disco pieno un istante). Riprovare il
            // login dopo che il problema si è risolto deve poter funzionare
            // di nuovo, come per `UserStore::register` (salva-poi-commetti).
            // `close_session`/`insert_sample` non hanno questo trattamento:
            // il loro fallimento perde al più un dato o si autocorregge al
            // prossimo salvataggio riuscito, non blocca nessun login.
            self.inner.sessions_by_id.remove(&session_id);
            return Err(TrackingError::PersistenceFailed);
        }

        Ok(())
    }

    fn close_session(
        &mut self,
        session_id: SessionId,
        disconnected_at: i64,
    ) -> Result<(), TrackingError> {
        self.inner.close_session(session_id, disconnected_at)?;
        self.persist_and_mark(disconnected_at)
            .map_err(|_| TrackingError::PersistenceFailed)
    }

    /// A differenza di `open_session`/`close_session`, non scrive a ogni
    /// chiamata: sotto `MIN_PERSIST_INTERVAL_SECONDS` dall'ultima scrittura
    /// il campione resta solo in memoria (vedi il commento sulla costante).
    /// La mutazione in memoria avviene comunque sempre: un client che legge
    /// subito dopo (`/stats`) vede il campione anche se non è ancora su
    /// disco, esattamente come già succede fra una scrittura e l'altra.
    fn insert_sample(&mut self, sample: &PositionSample) -> Result<(), TrackingError> {
        self.inner.insert_sample(sample)?;

        let da_ultima_scrittura = sample.received_at() - self.last_persist_utc;
        if da_ultima_scrittura < MIN_PERSIST_INTERVAL_SECONDS {
            return Ok(());
        }

        self.persist_and_mark(sample.received_at())
            .map_err(|_| TrackingError::PersistenceFailed)
    }

    fn samples_for_period(
        &self,
        username: &str,
        start_utc: i64,
        end_utc: i64,
    ) -> Result<Vec<PositionSample>, TrackingError> {
        self.inner.samples_for_period(username, start_utc, end_utc)
    }

    fn latest_sample_before(
        &self,
        username: &str,
        before_utc: i64,
    ) -> Result<Option<PositionSample>, TrackingError> {
        self.inner.latest_sample_before(username, before_utc)
    }

    fn prune_before(&mut self, cutoff_utc: i64) -> Result<usize, TrackingError> {
        let removed = self.inner.prune_before(cutoff_utc)?;
        self.persist()
            .map_err(|_| TrackingError::PersistenceFailed)?;
        Ok(removed)
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

    #[test]
    fn sample_before_session_connection_is_rejected() {
        let mut repository = InMemoryPositionRepository::new(1_000);

        repository.open_session(13, "alice", 100).unwrap();

        let sample = PositionSample::new(
            String::from("alice"),
            13,
            GeoPoint::try_new(45.0268, 7.7620).unwrap(),
            99,
            None,
        );

        assert_eq!(
            repository.insert_sample(&sample),
            Err(TrackingError::InvalidSessionTimestamp)
        );

        assert!(
            repository
                .samples_for_period("alice", 0, 200)
                .unwrap()
                .is_empty()
        );
    }
}

#[cfg(test)]
mod file_repository_tests {
    use super::*;
    use crate::tracking::GeoPoint;

    /// Percorso temporaneo univoco, stesso schema di `users::tests::temp_path`:
    /// i test girano in parallelo e non devono condividere un file.
    fn temp_path(name: &str) -> String {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!("georuggine_tracking_test_{name}_{unique}.json"))
            .to_string_lossy()
            .into_owned()
    }

    fn cleanup(path: &str) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{path}.tmp"));
    }

    #[test]
    fn file_assente_parte_da_storico_vuoto() {
        let path = temp_path("assente");

        let repository = FilePositionRepository::load(&path, 1_000).unwrap();

        assert!(
            repository
                .samples_for_period("pincopallo", 0, 1_000)
                .unwrap()
                .is_empty()
        );

        cleanup(&path);
    }

    #[test]
    fn file_corrotto_fa_fallire_il_caricamento() {
        let path = temp_path("corrotto");
        std::fs::write(&path, "{ questo non e' json valido").unwrap();

        assert!(FilePositionRepository::load(&path, 1_000).is_err());

        cleanup(&path);
    }

    #[test]
    fn sessioni_e_campioni_sopravvivono_al_riavvio() {
        let path = temp_path("riavvio");

        {
            let mut repository = FilePositionRepository::load(&path, 1_000).unwrap();

            repository.open_session(42, "pincopallo", 100).unwrap();

            let sample = PositionSample::new(
                String::from("pincopallo"),
                42,
                GeoPoint::try_new(45.0628, 7.7620).unwrap(),
                150,
                None,
            );
            repository.insert_sample(&sample).unwrap();
        }

        // Nuova istanza sullo stesso file = riavvio del server.
        let repository = FilePositionRepository::load(&path, 1_000).unwrap();

        let samples = repository.samples_for_period("pincopallo", 0, 300).unwrap();

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].session_id(), 42);

        cleanup(&path);
    }

    #[test]
    fn una_sessione_chiusa_resta_chiusa_dopo_il_riavvio() {
        let path = temp_path("sessione_chiusa");

        {
            let mut repository = FilePositionRepository::load(&path, 1_000).unwrap();
            repository.open_session(13, "alice", 100).unwrap();
            repository.close_session(13, 150).unwrap();
        }

        let mut repository = FilePositionRepository::load(&path, 1_000).unwrap();

        // Una sessione già chiusa prima del riavvio non deve poter ricevere
        // campioni: se il caricamento la dimenticasse "attiva", questo
        // fallirebbe silenziosamente ad accettare un campione che non dovrebbe.
        let sample = PositionSample::new(
            String::from("alice"),
            13,
            GeoPoint::try_new(45.0628, 7.7620).unwrap(),
            200,
            None,
        );

        assert_eq!(
            repository.insert_sample(&sample),
            Err(TrackingError::SessionNotActive)
        );

        cleanup(&path);
    }

    #[test]
    fn una_sessione_lasciata_aperta_dal_riavvio_non_blocca_il_login() {
        let path = temp_path("sessione_orfana");

        {
            // Simula un processo terminato senza logout pulito (kill,
            // crash): la sessione resta "aperta" nel file, nessuno la chiude.
            let mut repository = FilePositionRepository::load(&path, 1_000).unwrap();
            repository.open_session(13, "alice", 100).unwrap();
        }

        // Riavvio: senza la riconciliazione, questa seconda open_session
        // per lo stesso utente fallirebbe con SessionAlreadyActive per
        // sempre, anche se nessuno ha mai chiuso la prima "a mano".
        let mut repository = FilePositionRepository::load(&path, 1_000).unwrap();

        assert_eq!(repository.open_session(14, "alice", 200), Ok(()));

        cleanup(&path);
    }

    #[test]
    fn un_salvataggio_fallito_in_apertura_non_blocca_i_login_successivi() {
        // Stesso schema di
        // `users::tests::un_salvataggio_fallito_non_danneggia_il_file_esistente`:
        // si fa sparire la cartella (rinominandola altrove, non
        // cancellandola) cosi' la scrittura del temporaneo fallisce allo
        // stesso modo su Unix e su Windows.
        let dir = std::env::temp_dir().join(format!(
            "georuggine_tracking_rollback_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("storico.json").to_string_lossy().into_owned();

        let mut repository = FilePositionRepository::load(&path, 1_000).unwrap();

        let dir_spostata = std::env::temp_dir().join(format!(
            "georuggine_tracking_rollback_via_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::rename(&dir, &dir_spostata).unwrap();

        let esito = repository.open_session(13, "alice", 100);

        std::fs::rename(&dir_spostata, &dir).unwrap();

        assert_eq!(esito, Err(TrackingError::PersistenceFailed));

        // Il punto del test: senza il rollback, questa seconda apertura
        // per lo stesso utente fallirebbe con SessionAlreadyActive, perché
        // la prima (fallita su disco) sarebbe rimasta "attiva" in memoria.
        assert_eq!(repository.open_session(14, "alice", 200), Ok(()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn la_potatura_alla_scrittura_si_riflette_su_disco() {
        let path = temp_path("potatura");

        {
            let mut repository = FilePositionRepository::load(&path, 100).unwrap();

            repository.open_session(13, "pincopallo", 100).unwrap();

            let old_sample = PositionSample::new(
                String::from("pincopallo"),
                13,
                GeoPoint::try_new(45.0628, 7.7620).unwrap(),
                100,
                None,
            );
            repository.insert_sample(&old_sample).unwrap();

            let current_sample = PositionSample::new(
                String::from("pincopallo"),
                13,
                GeoPoint::try_new(45.0630, 7.7630).unwrap(),
                201,
                None,
            );
            // Oltre la retention (100s): fa potare il campione precedente
            // anche sul file, non solo in memoria.
            repository.insert_sample(&current_sample).unwrap();
        }

        let repository = FilePositionRepository::load(&path, 100).unwrap();
        let samples = repository.samples_for_period("pincopallo", 0, 300).unwrap();

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].received_at(), 201);

        cleanup(&path);
    }

    #[test]
    fn il_salvataggio_non_lascia_file_temporanei() {
        let path = temp_path("nessun_residuo");
        let mut repository = FilePositionRepository::load(&path, 1_000).unwrap();

        repository.open_session(7, "grace", 100).unwrap();

        assert!(!std::path::Path::new(&format!("{path}.tmp")).exists());
        assert!(std::path::Path::new(&path).exists());

        cleanup(&path);
    }

    #[test]
    fn campioni_ravvicinati_non_riscrivono_il_file_a_ogni_volta() {
        let path = temp_path("throttle");

        let mut repository = FilePositionRepository::load(&path, 1_000).unwrap();
        repository.open_session(1, "pincopallo", 1000).unwrap();

        let vicino = PositionSample::new(
            String::from("pincopallo"),
            1,
            GeoPoint::try_new(45.0, 7.0).unwrap(),
            1000 + MIN_PERSIST_INTERVAL_SECONDS - 1, // appena sotto la soglia
            None,
        );
        repository.insert_sample(&vicino).unwrap();

        // In memoria il campione c'è già: chi interroga questa stessa
        // istanza (es. /stats subito dopo) lo vede comunque.
        assert_eq!(
            repository
                .samples_for_period("pincopallo", 0, 3000)
                .unwrap()
                .len(),
            1
        );

        // Ma non è ancora su disco: un riavvio in questo istante lo perde.
        let dopo_riavvio_immediato = FilePositionRepository::load(&path, 1_000).unwrap();
        assert!(
            dopo_riavvio_immediato
                .samples_for_period("pincopallo", 0, 3000)
                .unwrap()
                .is_empty(),
            "un campione sotto la soglia non deve ancora essere su disco"
        );

        let lontano = PositionSample::new(
            String::from("pincopallo"),
            1,
            GeoPoint::try_new(45.0, 7.0).unwrap(),
            1000 + MIN_PERSIST_INTERVAL_SECONDS, // esattamente alla soglia: scrive
            None,
        );
        repository.insert_sample(&lontano).unwrap();

        // La scrittura scaduta porta su disco anche "vicino", rimasto solo
        // in memoria fino a questo momento: nessun campione perso, solo
        // ritardato.
        let dopo_seconda_scrittura = FilePositionRepository::load(&path, 1_000).unwrap();
        assert_eq!(
            dopo_seconda_scrittura
                .samples_for_period("pincopallo", 0, 3000)
                .unwrap()
                .len(),
            2,
            "la scrittura che scade deve portare su disco anche i campioni accumulati prima"
        );

        cleanup(&path);
    }
}
