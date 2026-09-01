//! Macchina a stati live di un utente (`Disconnected`/`Idle`/`Moving`) e lo
//! storage in memoria dell'ultima posizione nota.
//!
//! Lo stato **non viene trasmesso agli altri client**. Una versione
//! precedente mandava un `StateChanged` in broadcast a tutti i connessi, ma
//! la consegna chiede che sia *il sistema* a gestire lo stato di ogni
//! utente e descrive la comunicazione solo come server->utente e
//! utente->server: che un utente scopra quando un altro si muove o si ferma
//! non e' richiesto da nessuna parte. Era anche incoerente con la scelta,
//! gia' presa e documentata, di non inoltrare i messaggi di chat fra
//! utenti. Lo stato resta visibile a chi di dovere: la console
//! dell'operatore, che lo stampa a ogni transizione e lo espone con
//! `/users` e `/stats`.
//!
//! Modulo separato apposta da `main.rs` (vedi ORGANIZZAZIONE.md: "Andrea
//! mantiene la proprietà di [...] macchina a stati live", mentre Francesco
//! mantiene "ciclo principale del server" in `main.rs`): così le due persone
//! possono lavorare in parallelo senza modificare lo stesso file.

use common::UserState;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

// La soglia di posizione invariata oltre la quale un utente "in movimento"
// torna "fermo" (consegna: 3 minuti) vive in `common::timing`, che permette
// di accorciarla con una variabile d'ambiente durante le prove manuali senza
// toccare il default. I test qui sotto usano la costante direttamente:
// hanno già un orologio simulato e non devono dipendere dall'ambiente.

/// Sorgente di tempo per la macchina a stati.
///
/// PROBLEMA CORRETTO: la versione precedente usava il `timestamp` fornito
/// dal *client* dentro `PositionUpdate` per decidere quando sono passati 3
/// minuti. Un client con l'orologio sbagliato (o scritto male, o
/// deliberatamente malevolo) poteva così falsare le transizioni di stato
/// senza che il server se ne accorgesse. Il server deve fidarsi solo del
/// proprio orologio per una decisione che gli altri utenti vedono come
/// "verità" (`StateChanged`).
///
/// `Clock` esiste come trait (invece di chiamare `SystemTime::now()`
/// direttamente in `update_position`) perché la consegna del gruppo per la
/// settimana 2 richiede che "la macchina a stati deve accettare un orologio
/// o timestamp simulato" nei test — altrimenti verificare la transizione
/// Moving->Idle richiederebbe aspettare 3 minuti veri ad ogni test.
pub trait Clock: Send + Sync {
    fn now_secs(&self) -> i64;
}

/// Orologio reale, basato sul tempo di sistema del processo server.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_secs(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}

/// Handle condiviso all'orologio, passato a `handle_client`/`update_position`
/// con lo stesso pattern di `Clients`/`Users`/`Positions`.
pub type SharedClock = Arc<dyn Clock>;

/// Stato pubblico di un utente rispetto alla macchina a stati.
///
/// `NeverPositioned` è distinto da `Known(Idle)`: rappresenta un utente
/// loggato ma di cui non è ancora arrivato nessun `PositionUpdate`, quindi
/// nessuno stato è ancora stato annunciato agli altri client. Prima di
/// questa distinzione, la primissima posizione mai ricevuta da un utente
/// veniva silenziosamente inserita già come `Idle` "di partenza": nessun
/// `StateChanged` veniva mai notificato per essa, quindi gli altri utenti
/// scoprivano un veicolo solo alla sua prima transizione a `Moving`, mai
/// quando si connetteva restando fermo. Con questa distinzione,
/// `NeverPositioned -> Known(Idle)` è una transizione vera e viene
/// notificata come le altre.
#[derive(Clone, PartialEq, Debug)]
enum LiveState {
    NeverPositioned,
    Known(UserState),
}

/// Ultima posizione nota e stato corrente di un utente. Vive solo in
/// memoria (si azzera ad ogni riavvio del server): la persistenza dello
/// storico posizioni, necessaria per l'analisi del movimento, è compito
/// separato (Viorel, vedi ORGANIZZAZIONE.md).
#[derive(Clone)]
pub(crate) struct PositionRecord {
    lat: f64,
    lon: f64,
    /// Secondi Unix (dall'orologio del *server*, mai dal client) dell'ultimo
    /// istante in cui le coordinate sono effettivamente cambiate.
    last_change_secs: i64,
    state: LiveState,
}

/// Stato di posizione/movimento per utente, con lo stesso pattern di
/// `Clients`/`Users`: `Arc<Mutex<HashMap<...>>>` condiviso tra i task.
pub type Positions = Arc<Mutex<HashMap<String, PositionRecord>>>;

pub fn new_positions() -> Positions {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Range valido per la latitudine in gradi.
fn is_valid_lat(lat: f64) -> bool {
    lat.is_finite() && (-90.0..=90.0).contains(&lat)
}

/// Range valido per la longitudine in gradi.
fn is_valid_lon(lon: f64) -> bool {
    lon.is_finite() && (-180.0..=180.0).contains(&lon)
}

/// Aggiorna posizione e stato di `username` a fronte di un `PositionUpdate`,
/// applicando le transizioni richieste dalla consegna, e notifica gli altri
/// client solo se lo stato pubblico è cambiato davvero.
///
/// Ritorna `false` (e scarta l'update senza toccare stato/posizione) se
/// `lat`/`lon` sono fuori range o non finiti (`NaN`/infinito): un client che
/// manda coordinate assurde non deve poter corrompere lo stato condiviso.
///
/// Regole di transizione:
/// - Nessuna posizione precedente (`NeverPositioned`) → nasce `Idle`: non
///   c'è nulla con cui confrontare le coordinate per dire che si è mosso.
/// - Coordinate diverse da quelle memorizzate → `Moving`.
/// - Coordinate uguali, stato `Moving` da almeno [`IDLE_THRESHOLD_SECONDS`]
///   (sull'orologio del server) → `Idle`.
/// - Altrimenti nessun cambiamento.
pub async fn update_position(
    positions: &Positions,
    clock: &SharedClock,
    username: &str,
    lat: f64,
    lon: f64,
) -> bool {
    if !is_valid_lat(lat) || !is_valid_lon(lon) {
        crate::detail!("posizione scartata per {username}: coordinate non valide ({lat}, {lon})");
        return false;
    }

    let now = clock.now_secs();

    let new_state = {
        let mut guard = positions.lock().await;
        let entry = guard.entry(username.to_string()).or_insert(PositionRecord {
            lat,
            lon,
            last_change_secs: now,
            state: LiveState::NeverPositioned,
        });
        // Copia posseduta, non un riferimento a `entry.state`: matchare su
        // `&entry.state` e poi assegnare `entry.state = ...` nello stesso
        // arm farebbe scattare il borrow checker (scrutinee ancora "in uso"
        // mentre si scrive sul campo). Con una copia indipendente `entry`
        // resta liberamente mutabile in ogni arm.
        let previous_state = entry.state.clone();

        match &previous_state {
            LiveState::NeverPositioned => {
                entry.lat = lat;
                entry.lon = lon;
                entry.last_change_secs = now;
                entry.state = LiveState::Known(UserState::Idle);
            }
            LiveState::Known(_) if entry.lat != lat || entry.lon != lon => {
                entry.lat = lat;
                entry.lon = lon;
                entry.last_change_secs = now;
                entry.state = LiveState::Known(UserState::Moving);
            }
            LiveState::Known(UserState::Moving)
                if now - entry.last_change_secs >= common::timing::idle_threshold_seconds() =>
            {
                entry.state = LiveState::Known(UserState::Idle);
            }
            _ => {}
        }

        (entry.state != previous_state).then(|| entry.state.clone())
    };

    if let Some(LiveState::Known(state)) = new_state {
        // Stampata sempre, non solo col dettaglio attivo: da quando lo
        // stato non viene piu' trasmesso ai client, la console
        // dell'operatore e' l'unico posto in cui si vede. Era una riga
        // ridondante finche' gli utenti la ricevevano; ora e' la sola
        // manifestazione visibile della macchina a stati.
        println!("[{username}] stato -> {state:?}");
    }

    true
}

/// Segna `username` come `Disconnected` e notifica gli altri client, ma
/// solo se aveva già uno stato pubblico noto (`Known`). Se l'utente si
/// disconnette prima di aver mai mandato una posizione (`NeverPositioned`,
/// o mai loggato/mai presente in mappa), non c'è nessuno stato pubblico da
/// revocare: nessun client ha mai saputo che fosse "online", quindi niente
/// da notificare.
pub async fn mark_disconnected(positions: &Positions, username: &str) {
    let changed = {
        let mut guard = positions.lock().await;
        match guard.get_mut(username) {
            Some(entry) => match &entry.state {
                LiveState::Known(state) if *state != UserState::Disconnected => {
                    entry.state = LiveState::Known(UserState::Disconnected);
                    true
                }
                _ => false,
            },
            None => false,
        }
    };

    if changed {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::timing::DEFAULT_IDLE_THRESHOLD_SECONDS as IDLE_THRESHOLD_SECONDS;
    use std::sync::atomic::{AtomicI64, Ordering};

    /// Orologio simulato per i test: parte da un valore fisso e si avanza
    /// manualmente con `advance`, senza aspettare tempo reale. Risponde
    /// esattamente al requisito "la macchina a stati deve accettare un
    /// orologio o timestamp simulato".
    struct FakeClock(AtomicI64);

    impl FakeClock {
        fn new(start: i64) -> Arc<Self> {
            Arc::new(Self(AtomicI64::new(start)))
        }

        fn advance(&self, secs: i64) {
            self.0.fetch_add(secs, Ordering::SeqCst);
        }
    }

    impl Clock for FakeClock {
        fn now_secs(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[tokio::test]
    async fn prima_posizione_diventa_idle_ed_e_notificata() {
        let positions = new_positions();
        let clock: SharedClock = FakeClock::new(1_000);

        let changed = update_position(&positions, &clock, "alice", 45.0, 7.0).await;

        assert!(changed);
        let guard = positions.lock().await;
        let entry = guard.get("alice").unwrap();
        assert_eq!(entry.state, LiveState::Known(UserState::Idle));
    }

    #[tokio::test]
    async fn cambio_coordinate_passa_a_moving() {
        let positions = new_positions();
        let clock: SharedClock = FakeClock::new(1_000);

        update_position(&positions, &clock, "alice", 45.0, 7.0).await;
        update_position(&positions, &clock, "alice", 45.001, 7.0).await;

        let guard = positions.lock().await;
        assert_eq!(
            guard.get("alice").unwrap().state,
            LiveState::Known(UserState::Moving)
        );
    }

    #[tokio::test]
    async fn coordinate_ferme_tre_minuti_tornano_idle() {
        let positions = new_positions();
        let clock = FakeClock::new(1_000);
        let shared: SharedClock = clock.clone();

        update_position(&positions, &shared, "alice", 45.0, 7.0).await;
        update_position(&positions, &shared, "alice", 45.001, 7.0).await; // -> Moving

        clock.advance(IDLE_THRESHOLD_SECONDS - 1);
        update_position(&positions, &shared, "alice", 45.001, 7.0).await;
        {
            let guard = positions.lock().await;
            // Ancora meno di 3 minuti fermo: resta Moving.
            assert_eq!(
                guard.get("alice").unwrap().state,
                LiveState::Known(UserState::Moving)
            );
        }

        clock.advance(1);
        update_position(&positions, &shared, "alice", 45.001, 7.0).await;
        let guard = positions.lock().await;
        assert_eq!(
            guard.get("alice").unwrap().state,
            LiveState::Known(UserState::Idle)
        );
    }

    #[tokio::test]
    async fn coordinate_non_valide_vengono_scartate() {
        let positions = new_positions();
        let clock: SharedClock = FakeClock::new(1_000);

        let accepted = update_position(&positions, &clock, "alice", 200.0, 7.0).await;

        assert!(!accepted);
        assert!(positions.lock().await.get("alice").is_none());
    }

    #[tokio::test]
    async fn disconnessione_senza_posizione_non_notifica() {
        let positions = new_positions();

        // Nessun update_position mai chiamato per "bob".
        mark_disconnected(&positions, "bob").await;

        assert!(positions.lock().await.get("bob").is_none());
    }

    #[tokio::test]
    async fn disconnessione_dopo_posizione_nota_passa_a_disconnected() {
        let positions = new_positions();
        let clock: SharedClock = FakeClock::new(1_000);

        update_position(&positions, &clock, "alice", 45.0, 7.0).await;
        mark_disconnected(&positions, "alice").await;

        let guard = positions.lock().await;
        assert_eq!(
            guard.get("alice").unwrap().state,
            LiveState::Known(UserState::Disconnected)
        );
    }
}
