//! Popolamento dello storico con dati sintetici, per la demo.
//!
//! PERCHÉ SERVE: lo storico delle posizioni vive solo in memoria e si
//! riempie mentre il server gira. Alla prima esecuzione `/stats bob week` e
//! `/stats bob month` riportano zero campioni, perché una settimana di dati
//! richiederebbe una settimana di server acceso. Mostrare le statistiche
//! richieste dalla consegna (tragitto, velocità media, durata del movimento
//! e delle pause su giorno, settimana e mese) diventa impossibile.
//!
//! PERCHÉ NON SI PUÒ FARE DAL CLIENT: il server ignora il `timestamp`
//! dichiarato nel `PositionUpdate` e usa il proprio orologio per
//! `received_at` (scelta deliberata: un client con l'ora sbagliata non deve
//! poter falsare le statistiche). Mandare posizioni retrodatate dalla rete
//! non funziona: arriverebbero comunque tutte con l'ora corrente.
//!
//! COME È PROTETTO: il comando esiste solo se `GEORUGGINE_DEMO` è attivo, si
//! chiama `/seed` e annuncia a video che i dati sono inventati. Non è una
//! funzionalità del prodotto ed è tenuto fuori dal percorso normale apposta:
//! dati fabbricati che si confondono con dati veri sarebbero peggio che non
//! avere dati.

use server::tracking::{GeoPoint, PositionSample, SharedTracking, TrackingError};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

const DEMO_VAR: &str = "GEORUGGINE_DEMO";

/// Giorni massimi generabili: oltre la retention dello storico (35 giorni) i
/// campioni più vecchi verrebbero comunque potati all'inserimento.
pub const MAX_DAYS: i64 = 30;

/// Id di sessione da cui parte la generazione. Sta lontano dagli id veri,
/// che partono da 1: guardando un campione si capisce subito se è inventato.
const DEMO_SESSION_BASE: u64 = 1_000_000;

/// Prossimo id di sessione da usare, condiviso fra tutte le chiamate a
/// [`seed`].
///
/// PROBLEMA CORRETTO: prima il contatore era una variabile locale che
/// ripartiva da `DEMO_SESSION_BASE` a ogni invocazione, quindi la seconda
/// `/seed` — con qualunque utente, non solo lo stesso — riapriva una
/// sessione già esistente e falliva con `SessionAlreadyExists`.
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(DEMO_SESSION_BASE);

/// Utenti già popolati in questa esecuzione del server.
///
/// Rigenerare lo storico dello stesso utente non lo sostituirebbe: lo
/// aggiungerebbe a quello di prima, raddoppiando tragitto e durate. Il
/// repository non espone un modo per cancellare i campioni di un utente,
/// quindi la seconda richiesta viene rifiutata invece di produrre numeri
/// sbagliati in silenzio.
static SEEDED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn gia_popolato(username: &str) -> bool {
    let guard = SEEDED.lock().expect("lock avvelenato");
    guard
        .as_ref()
        .is_some_and(|utenti| utenti.contains(username))
}

fn segna_popolato(username: &str) {
    let mut guard = SEEDED.lock().expect("lock avvelenato");
    guard
        .get_or_insert_with(HashSet::new)
        .insert(username.to_string());
}

/// Perché una generazione non è andata a buon fine.
#[derive(Debug)]
pub enum SeedError {
    /// Lo storico di quell'utente è già stato generato in questa esecuzione.
    GiaPopolato,
    /// Errore proveniente dal modulo di tracking.
    Tracking(TrackingError),
}

impl From<TrackingError> for SeedError {
    fn from(error: TrackingError) -> Self {
        SeedError::Tracking(error)
    }
}

/// Distanza fra due campioni consecutivi, come quella vera del client.
const SAMPLE_INTERVAL: i64 = 30;

/// Campioni per viaggio: 30 minuti di percorso.
const SAMPLES_PER_TRIP: i64 = 60;

/// Campioni fermi in mezzo al viaggio. A 30 secondi l'uno sono 240 secondi,
/// sopra la soglia di 180 di `analytics`: senza questo tratto il report
/// mostrerebbe sempre "Durata pause: 0s" e la grandezza chiesta dalla
/// consegna non si vedrebbe mai.
const STATIONARY_SAMPLES: i64 = 8;

/// Indice del campione da cui comincia la sosta.
const STATIONARY_START: i64 = 20;

/// Due viaggi al giorno, distanti abbastanza da non fondersi in un unico
/// segmento (il limite è 75 secondi di distacco).
const TRIP_OFFSETS_SECONDS: [i64; 2] = [-2 * 3600, -6 * 3600];

/// Punto di partenza dei percorsi: Torino, come il simulatore del client.
const START_LAT: f64 = 45.0628;
const START_LON: f64 = 7.6620;

/// Metri percorsi fra due campioni. A 30 secondi l'uno sono circa 30 km/h,
/// una velocità urbana plausibile per un veicolo. Un passo puramente
/// casuale, senza direzione, produceva 2 km/h: passo d'uomo, che in un
/// report di una flotta si nota subito.
const METERS_PER_SAMPLE: f64 = 250.0;

/// Metri in un grado di latitudine, e di longitudine a 45° (dove cos(45°)
/// accorcia il grado di circa un terzo).
const METERS_PER_DEG_LAT: f64 = 111_320.0;
const METERS_PER_DEG_LON: f64 = 78_710.0;

/// `true` se la generazione di storico finto è abilitata.
pub fn enabled() -> bool {
    static SLOT: OnceLock<bool> = OnceLock::new();
    *SLOT.get_or_init(|| match std::env::var(DEMO_VAR) {
        Ok(raw) => matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "si" | "on"
        ),
        Err(_) => false,
    })
}

/// Riga di avviso da stampare all'avvio quando la modalità è attiva.
pub fn startup_notice() -> Option<String> {
    enabled()
        .then(|| format!("{DEMO_VAR} attivo: comando /seed disponibile, genera storico NON reale"))
}

/// Riepilogo di ciò che è stato generato.
pub struct Seeded {
    pub sessions: usize,
    pub samples: usize,
    pub first_utc: i64,
    pub last_utc: i64,
}

/// Percorso deterministico: lo stesso utente e lo stesso giorno producono
/// sempre lo stesso tragitto, così una demo ripetuta mostra gli stessi
/// numeri e si può preparare cosa dire.
///
/// Il veicolo mantiene una **direzione di marcia** che ruota di poco a ogni
/// campione, invece di saltare a caso in ogni verso: un cammino casuale puro
/// resta attorno al punto di partenza e produce un tragitto che non porta da
/// nessuna parte.
struct Walk {
    state: u64,
    heading: f64,
}

impl Walk {
    fn new(username: &str, day: i64) -> Self {
        // FNV-1a sull'username, mescolato col giorno.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in username.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
        }
        hash ^= (day as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut walk = Self {
            state: hash.max(1),
            heading: 0.0,
        };
        // Direzione iniziale diversa per utente e giorno.
        walk.heading = walk.unit() * std::f64::consts::TAU;
        walk
    }

    /// Numero in [0, 1) dalla sequenza.
    fn unit(&mut self) -> f64 {
        (self.next_u64() % 10_000) as f64 / 10_000.0
    }

    fn next_u64(&mut self) -> u64 {
        // xorshift64, come il simulatore del client: nessuna dipendenza.
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }

    /// Avanza di un campione e restituisce lo spostamento in gradi
    /// (latitudine, longitudine).
    ///
    /// La direzione ruota al massimo di circa 34° per campione e la distanza
    /// varia fra il 70% e il 130% di [`METERS_PER_SAMPLE`]: abbastanza per
    /// non sembrare un righello, poco per restare un percorso plausibile.
    fn step(&mut self) -> (f64, f64) {
        self.heading += (self.unit() - 0.5) * 1.2;

        let meters = METERS_PER_SAMPLE * (0.7 + self.unit() * 0.6);

        (
            self.heading.cos() * meters / METERS_PER_DEG_LAT,
            self.heading.sin() * meters / METERS_PER_DEG_LON,
        )
    }
}

/// Riempie lo storico di `username` con `days` giorni di viaggi finti.
///
/// I campioni vengono inseriti in ordine cronologico crescente: il
/// repository pota all'inserimento usando `received_at - retention` come
/// soglia, quindi inserire prima i più recenti farebbe sparire i più vecchi
/// appena aggiunti.
pub async fn seed(
    tracking: &SharedTracking,
    username: &str,
    days: i64,
    now_utc: i64,
) -> Result<Seeded, SeedError> {
    if gia_popolato(username) {
        return Err(SeedError::GiaPopolato);
    }

    let days = days.clamp(1, MAX_DAYS);

    let mut sessions = 0usize;
    let mut samples = 0usize;
    let mut first_utc = i64::MAX;
    let mut last_utc = i64::MIN;

    // Dal giorno più lontano a oggi, e dentro ogni giorno dal viaggio più
    // lontano: l'ordine cronologico è ciò che tiene in vita i campioni.
    for day in (0..days).rev() {
        let day_anchor = now_utc - day * 86_400;

        for offset in TRIP_OFFSETS_SECONDS.iter().rev() {
            let trip_start = day_anchor + offset;

            // Un viaggio nel futuro non avrebbe senso e verrebbe comunque
            // escluso da ogni periodo: si salta.
            if trip_start >= now_utc {
                continue;
            }

            let mut walk = Walk::new(username, day * 10 + sessions as i64);
            let mut lat = START_LAT;
            let mut lon = START_LON;

            // Id preso dal contatore globale: mai riusato per tutta la vita
            // del processo, nemmeno fra chiamate diverse.
            let session_id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
            tracking
                .open_session(session_id, username, trip_start - 1)
                .await?;

            for index in 0..SAMPLES_PER_TRIP {
                let at = trip_start + index * SAMPLE_INTERVAL;
                if at >= now_utc {
                    break;
                }

                // Il tratto fermo: le coordinate non cambiano, così
                // `analytics` lo riconosce come pausa.
                let fermo =
                    (STATIONARY_START..STATIONARY_START + STATIONARY_SAMPLES).contains(&index);
                if !fermo {
                    let (d_lat, d_lon) = walk.step();
                    lat += d_lat;
                    lon += d_lon;
                }

                // Se il passo casuale portasse fuori range il punto viene
                // scartato: meglio un campione in meno che un dato assurdo.
                let Ok(point) = GeoPoint::try_new(lat, lon) else {
                    continue;
                };

                let sample = PositionSample::new(username.to_string(), session_id, point, at, None);
                tracking.record_position(sample).await?;

                samples += 1;
                first_utc = first_utc.min(at);
                last_utc = last_utc.max(at);
            }

            let trip_end = (trip_start + SAMPLES_PER_TRIP * SAMPLE_INTERVAL).min(now_utc - 1);
            tracking.close_session(session_id, trip_end).await?;
            sessions += 1;
        }
    }

    segna_popolato(username);

    Ok(Seeded {
        sessions,
        samples,
        first_utc,
        last_utc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passi(username: &str, day: i64, quanti: usize) -> Vec<(f64, f64)> {
        let mut walk = Walk::new(username, day);
        (0..quanti).map(|_| walk.step()).collect()
    }

    #[test]
    fn lo_stesso_utente_e_giorno_danno_lo_stesso_percorso() {
        assert_eq!(
            passi("bob", 3, 10),
            passi("bob", 3, 10),
            "la demo deve essere ripetibile"
        );
    }

    #[test]
    fn utenti_e_giorni_diversi_danno_percorsi_diversi() {
        assert_ne!(passi("bob", 3, 10), passi("carol", 3, 10));
        assert_ne!(passi("bob", 3, 10), passi("bob", 4, 10));
    }

    #[test]
    fn la_velocita_resta_da_veicolo_urbano() {
        // Ogni campione dista 30 secondi: la distanza percorsa deve stare
        // fra i 20 e i 45 km/h, non passo d'uomo e non un aereo.
        for (d_lat, d_lon) in passi("bob", 1, 400) {
            let metri = ((d_lat * METERS_PER_DEG_LAT).powi(2)
                + (d_lon * METERS_PER_DEG_LON).powi(2))
            .sqrt();
            let km_h = metri / SAMPLE_INTERVAL as f64 * 3.6;

            assert!(
                (20.0..=45.0).contains(&km_h),
                "velocità implausibile: {km_h:.1} km/h"
            );
        }
    }

    #[test]
    fn gli_id_di_sessione_non_si_ripetono_fra_chiamate() {
        // Il difetto che questo test previene: un contatore che ripartiva
        // dalla base a ogni invocazione faceva fallire la seconda /seed con
        // SessionAlreadyExists, anche cambiando utente.
        let primo = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        let secondo = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);

        assert!(secondo > primo);
        assert!(
            primo >= DEMO_SESSION_BASE,
            "deve restare fuori dagli id veri"
        );
    }

    #[test]
    fn un_utente_si_popola_una_volta_sola() {
        assert!(!gia_popolato("utente_mai_visto"));

        segna_popolato("utente_di_prova");
        assert!(gia_popolato("utente_di_prova"));
        // Un altro utente resta libero.
        assert!(!gia_popolato("un_altro_utente"));
    }

    #[test]
    fn senza_la_variabile_la_demo_e_spenta() {
        assert!(!enabled());
        assert!(startup_notice().is_none());
    }
}
