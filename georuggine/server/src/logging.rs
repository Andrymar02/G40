//! Livello di dettaglio dell'output del server.
//!
//! Il server stampava una riga per ogni evento di rete: apertura della
//! connessione, ogni `PositionUpdate` (uno ogni 30 secondi per ogni client
//! connesso), chiusura, errori di invio. Con due o tre veicoli il terminale
//! dell'operatore scorreva da solo e le cose che contano davvero — chi si è
//! collegato, chi si muove, cosa scrivono gli utenti — ci annegavano dentro.
//!
//! La divisione è per **destinatario**, non per gravità:
//!
//! * di default si stampa ciò che interessa a chi *usa* il server come
//!   operatore: registrazioni, login, cambi di stato, messaggi degli utenti,
//!   disconnessioni, errori veri;
//! * con `GEORUGGINE_VERBOSE` si aggiunge la traccia di rete che serve a chi
//!   *sviluppa*: singole posizioni, aperture e chiusure di socket, invii
//!   falliti.
//!
//! Nessun messaggio è stato eliminato: quelli di dettaglio sono a un
//! interruttore di distanza.

use std::sync::OnceLock;

const VERBOSE_VAR: &str = "GEORUGGINE_VERBOSE";

/// `true` se è stata chiesta la traccia di rete completa.
///
/// Accetta `1`, `true`, `yes`, `si`, `on` (senza distinzione fra maiuscole e
/// minuscole). Qualunque altro valore vale "spento": una variabile impostata
/// a `0` o `false` deve poter spegnere il dettaglio, non accenderlo per il
/// solo fatto di esistere.
pub fn verbose() -> bool {
    static SLOT: OnceLock<bool> = OnceLock::new();
    *SLOT.get_or_init(|| match std::env::var(VERBOSE_VAR) {
        Ok(raw) => matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "si" | "on"
        ),
        Err(_) => false,
    })
}

/// Riga stampata solo con il dettaglio attivo.
///
/// È un macro e non una funzione perché così gli argomenti non vengono
/// nemmeno formattati quando il dettaglio è spento: con un client che manda
/// una posizione ogni 30 secondi, formattare per poi buttare via sarebbe
/// lavoro sprecato a ogni messaggio.
#[macro_export]
macro_rules! detail {
    ($($arg:tt)*) => {
        if $crate::logging::verbose() {
            println!($($arg)*);
        }
    };
}

/// Riga da stampare all'avvio quando il dettaglio è attivo, così non resta
/// acceso a insaputa di chi guarda il terminale.
pub fn startup_notice() -> Option<String> {
    verbose().then(|| format!("{VERBOSE_VAR} attivo: traccia di rete completa"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `verbose()` usa `OnceLock`, quindi in un processo di test la prima
    /// lettura fisserebbe il valore per tutte le altre: la logica di
    /// interpretazione si prova a parte.
    fn interpreta(raw: &str) -> bool {
        matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "si" | "on"
        )
    }

    #[test]
    fn accende_con_i_valori_affermativi() {
        for acceso in ["1", "true", "TRUE", "yes", "si", "on", " 1 "] {
            assert!(interpreta(acceso), "{acceso:?} doveva accendere");
        }
    }

    #[test]
    fn resta_spento_con_tutto_il_resto() {
        // "0" e "false" sono il caso che conta: una variabile impostata per
        // spegnere non deve accendere per il fatto di esistere.
        for spento in ["0", "false", "no", "", "  ", "boh"] {
            assert!(!interpreta(spento), "{spento:?} doveva restare spento");
        }
    }

    #[test]
    fn senza_variabile_il_dettaglio_e_spento() {
        // I test girano senza la variabile impostata.
        assert!(!verbose());
        assert!(startup_notice().is_none());
    }
}
