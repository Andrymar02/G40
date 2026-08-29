//! Tempi del protocollo, con override da variabile d'ambiente per le prove
//! manuali.
//!
//! I valori di default sono quelli fissati dalla consegna e **non cambiano**:
//! posizione ogni 30 secondi, ritorno a "fermo" dopo 3 minuti di coordinate
//! invariate. Il problema che questo modulo risolve è un altro: provare a
//! mano la macchina a stati costringe ad aspettare mezzo minuto per vedere
//! comparire il primo stato e tre minuti per vedere `Moving -> Idle`. Su una
//! sessione di verifica con più tentativi diventa impraticabile, e chi prova
//! finisce per non provare.
//!
//! I test automatici non usano nulla di tutto questo: la macchina a stati ha
//! già un orologio simulato (`Clock`) e i test end-to-end parlano il
//! protocollo direttamente. Questo serve solo alla persona davanti a tre
//! terminali.
//!
//! Ogni override attivo viene **annunciato all'avvio**: un tempo accorciato
//! non deve mai passare inosservato, altrimenti si rischia di fare una demo
//! con intervalli finti senza accorgersene.

use std::sync::OnceLock;
use std::time::Duration;

/// Intervallo fra due invii di posizione, lato client (consegna: 30 secondi).
pub const DEFAULT_POSITION_INTERVAL_SECONDS: u64 = 30;

/// Secondi di coordinate invariate dopo i quali un utente "in movimento"
/// torna "fermo" (consegna: 3 minuti).
pub const DEFAULT_IDLE_THRESHOLD_SECONDS: i64 = 180;

/// Silenzio massimo su una connessione prima di considerarla caduta.
/// Non viene dalla consegna: è tre volte l'intervallo di posizione.
pub const DEFAULT_CLIENT_IDLE_TIMEOUT_SECONDS: u64 = 90;

const POSITION_INTERVAL_VAR: &str = "GEORUGGINE_POSITION_INTERVAL";
const IDLE_THRESHOLD_VAR: &str = "GEORUGGINE_IDLE_THRESHOLD";
const CLIENT_IDLE_TIMEOUT_VAR: &str = "GEORUGGINE_CLIENT_TIMEOUT";

/// Legge un intero positivo da una variabile d'ambiente, con fallback.
///
/// Un valore assente lascia il default in silenzio; un valore **illeggibile
/// o zero** viene segnalato e ignorato, invece di far partire il programma
/// con un tempo che l'utente crede di aver impostato e che non è quello.
fn override_seconds(var: &str, default: u64) -> (u64, bool) {
    let Ok(raw) = std::env::var(var) else {
        return (default, false);
    };

    match raw.trim().parse::<u64>() {
        Ok(value) if value > 0 => (value, true),
        _ => {
            eprintln!("{var}: valore non valido ({raw:?}), uso il default di {default} secondi");
            (default, false)
        }
    }
}

fn cached(slot: &'static OnceLock<(u64, bool)>, var: &str, default: u64) -> (u64, bool) {
    // Letta una volta sola: il valore non deve cambiare a metà esecuzione,
    // e stampare l'avviso a ogni chiamata allagherebbe il terminale.
    *slot.get_or_init(|| override_seconds(var, default))
}

/// Ogni quanto il client manda la propria posizione.
pub fn position_interval() -> Duration {
    static SLOT: OnceLock<(u64, bool)> = OnceLock::new();
    let (seconds, _) = cached(
        &SLOT,
        POSITION_INTERVAL_VAR,
        DEFAULT_POSITION_INTERVAL_SECONDS,
    );
    Duration::from_secs(seconds)
}

/// Dopo quanti secondi di coordinate invariate si torna "fermo".
pub fn idle_threshold_seconds() -> i64 {
    static SLOT: OnceLock<(u64, bool)> = OnceLock::new();
    let (seconds, _) = cached(
        &SLOT,
        IDLE_THRESHOLD_VAR,
        DEFAULT_IDLE_THRESHOLD_SECONDS as u64,
    );
    seconds as i64
}

/// Dopo quanto silenzio il server considera caduta una connessione.
pub fn client_idle_timeout() -> Duration {
    static SLOT: OnceLock<(u64, bool)> = OnceLock::new();
    let (seconds, _) = cached(
        &SLOT,
        CLIENT_IDLE_TIMEOUT_VAR,
        DEFAULT_CLIENT_IDLE_TIMEOUT_SECONDS,
    );
    Duration::from_secs(seconds)
}

/// Descrizione di un override attivo, o `None` se si usa il default.
fn avviso(var: &str, valore: u64, default: u64, descrizione: &str) -> Option<String> {
    (valore != default).then(|| {
        format!("ATTENZIONE {var}={valore}s: {descrizione} accorciato (default {default}s)")
    })
}

/// Override attivi che riguardano il **server**, da stampare al suo avvio.
///
/// Server e client hanno elenchi separati apposta: annunciare un tempo che
/// il processo non usa (l'intervallo di posizione è deciso dal client)
/// farebbe credere all'operatore che il server si comporti diversamente da
/// come si comporta davvero.
pub fn server_overrides() -> Vec<String> {
    [
        avviso(
            IDLE_THRESHOLD_VAR,
            idle_threshold_seconds() as u64,
            DEFAULT_IDLE_THRESHOLD_SECONDS as u64,
            "ritorno a fermo",
        ),
        avviso(
            CLIENT_IDLE_TIMEOUT_VAR,
            client_idle_timeout().as_secs(),
            DEFAULT_CLIENT_IDLE_TIMEOUT_SECONDS,
            "timeout di inattività",
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Override attivi che riguardano il **client**, da stampare al suo avvio.
pub fn client_overrides() -> Vec<String> {
    [avviso(
        POSITION_INTERVAL_VAR,
        position_interval().as_secs(),
        DEFAULT_POSITION_INTERVAL_SECONDS,
        "invio della posizione",
    )]
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i_default_sono_quelli_della_consegna() {
        assert_eq!(DEFAULT_POSITION_INTERVAL_SECONDS, 30);
        assert_eq!(DEFAULT_IDLE_THRESHOLD_SECONDS, 180);
    }

    // `override_seconds` è provata direttamente invece che attraverso le
    // funzioni pubbliche: quelle usano `OnceLock`, quindi in un processo di
    // test la prima lettura fisserebbe il valore per tutte le altre.
    #[test]
    fn una_variabile_assente_lascia_il_default() {
        let (valore, attivo) = override_seconds("GEORUGGINE_VAR_CHE_NON_ESISTE", 30);
        assert_eq!(valore, 30);
        assert!(!attivo);
    }

    #[test]
    fn un_valore_valido_sostituisce_il_default() {
        let var = "GEORUGGINE_TEST_VALIDO";
        unsafe { std::env::set_var(var, "3") };
        let (valore, attivo) = override_seconds(var, 30);
        unsafe { std::env::remove_var(var) };

        assert_eq!(valore, 3);
        assert!(attivo);
    }

    #[test]
    fn valori_assurdi_non_passano() {
        // Zero manderebbe il client in un ciclo di invii senza pausa, e una
        // stringa non numerica è quasi sempre un errore di battitura: in
        // entrambi i casi si torna al default invece di obbedire.
        for cattivo in ["0", "abc", "-5", "", "  "] {
            let var = "GEORUGGINE_TEST_CATTIVO";
            unsafe { std::env::set_var(var, cattivo) };
            let (valore, attivo) = override_seconds(var, 30);
            unsafe { std::env::remove_var(var) };

            assert_eq!(valore, 30, "valore {cattivo:?} non doveva essere accettato");
            assert!(!attivo);
        }
    }

    #[test]
    fn senza_override_nessuno_dei_due_elenchi_stampa_nulla() {
        // I test girano senza le variabili impostate: entrambi gli elenchi
        // devono essere vuoti, cosi' l'esecuzione normale non guadagna rumore.
        assert!(server_overrides().is_empty());
        assert!(client_overrides().is_empty());
    }

    #[test]
    fn gli_spazi_attorno_al_numero_sono_tollerati() {
        let var = "GEORUGGINE_TEST_SPAZI";
        unsafe { std::env::set_var(var, "  5  ") };
        let (valore, attivo) = override_seconds(var, 30);
        unsafe { std::env::remove_var(var) };

        assert_eq!(valore, 5);
        assert!(attivo);
    }
}
