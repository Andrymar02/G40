//! Storage utenti minimale: username -> password in chiaro, persistito su
//! un file JSON.
//!
//! SCELTA DI GRUPPO: le password restano in chiaro, in memoria e su file.
//! Non e' una svista ne' un pezzo mancante. La consegna (Progetto 2.1)
//! richiede solo "una fase di registrazione, nella quale vengono definiti un
//! account e una password" e non pone alcun requisito di sicurezza: niente
//! hashing, niente cifratura, niente TLS. Introdurre `argon2`/`bcrypt`
//! avrebbe aggiunto una dipendenza e peso all'eseguibile, mentre la consegna
//! chiede esplicitamente attenzione alla dimensione del binario e al consumo
//! di CPU.
//!
//! Resta invece aperta la scrittura atomica dello storage: vedi `save`.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

/// Lunghezza massima accettata per username e password. Serve a impedire che
/// un client faccia crescere `users.json` senza limiti: il framing limita il
/// singolo messaggio a 1 MiB, ma nulla vieterebbe di registrare utenti con
/// campi da centinaia di kB.
const MAX_FIELD_LEN: usize = 64;

/// Lunghezza minima della password. Volutamente bassa: serve a scartare la
/// password vuota, non a imporre una politica di sicurezza — che con le
/// password in chiaro (vedi il commento in cima al modulo) sarebbe teatro.
const MIN_PASSWORD_LEN: usize = 4;

/// Storage condiviso degli utenti registrati (username -> password),
/// persistito sul file indicato da `path`. Separato dal registro delle
/// sessioni (in `session.rs`): qui c'è chi è *registrato* (persistente), lì
/// chi è *connesso adesso* (effimero, si azzera ad ogni riavvio).
pub struct UserStore {
    entries: Mutex<HashMap<String, String>>,
    path: String,
}

/// Handle condiviso tra i task, come `Clients` in `session.rs`.
pub type Users = Arc<UserStore>;

/// Controlla che l'username sia utilizzabile come chiave dello storage e
/// come destinatario dei comandi della console operatore.
///
/// Gli spazi sono vietati perche' la console usa `/msg <utente> <testo>`:
/// un username con spazi renderebbe ambiguo dove finisce il destinatario.
fn validate_username(username: &str) -> Result<(), String> {
    if username.is_empty() {
        return Err("username vuoto".to_string());
    }
    if username.len() > MAX_FIELD_LEN {
        return Err(format!(
            "username troppo lungo (max {MAX_FIELD_LEN} caratteri)"
        ));
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("username: ammessi solo lettere, cifre, '_' e '-'".to_string());
    }
    Ok(())
}

fn validate_password(password: &str) -> Result<(), String> {
    if password.len() < MIN_PASSWORD_LEN {
        return Err(format!(
            "password troppo corta (min {MIN_PASSWORD_LEN} caratteri)"
        ));
    }
    if password.len() > MAX_FIELD_LEN {
        return Err(format!(
            "password troppo lunga (max {MAX_FIELD_LEN} caratteri)"
        ));
    }
    Ok(())
}

impl UserStore {
    /// Carica la mappa utenti dal file indicato.
    ///
    /// Un file **assente** e' la condizione normale del primo avvio: si parte
    /// da una mappa vuota. Un file **presente ma illeggibile o malformato**
    /// e' invece un errore che ferma l'avvio: trattarlo come database vuoto
    /// (com'era prima) farebbe ripartire il server senza utenti e le
    /// registrazioni successive sovrascriverebbero il file, distruggendo
    /// definitivamente le credenziali che erano solo temporaneamente
    /// illeggibili.
    pub fn load(path: &str) -> Result<Users, String> {
        let map = match std::fs::read_to_string(path) {
            Ok(data) => serde_json::from_str(&data)
                .map_err(|e| format!("{path} non e' un database utenti valido: {e}"))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(format!("impossibile leggere {path}: {e}")),
        };

        Ok(Arc::new(UserStore {
            entries: Mutex::new(map),
            path: path.to_string(),
        }))
    }

    /// Registra un nuovo utente. Fallisce se i campi non sono validi o se lo
    /// username e' gia' presente.
    ///
    /// L'ordine e' salva-poi-commetti: la mappa in memoria viene aggiornata
    /// **solo dopo** che la scrittura su file e' riuscita. Prima l'utente
    /// veniva inserito in memoria e il salvataggio poteva fallire dopo,
    /// lasciando un utente che risultava registrato al server ma spariva al
    /// riavvio successivo.
    pub async fn register(&self, username: &str, password: &str) -> Result<(), String> {
        validate_username(username)?;
        validate_password(password)?;

        let mut guard = self.entries.lock().await;
        if guard.contains_key(username) {
            return Err("username gia' in uso".to_string());
        }

        // Snapshot con la modifica applicata: la mappa viva resta intatta
        // finche' il file non e' scritto.
        let mut candidate = guard.clone();
        candidate.insert(username.to_string(), password.to_string());

        let json = serde_json::to_string_pretty(&candidate).map_err(|e| e.to_string())?;
        self.save(&json)
            .await
            .map_err(|e| format!("salvataggio fallito: {e}"))?;

        *guard = candidate;
        Ok(())
    }

    /// Verifica che `username`/`password` corrispondano a un utente registrato.
    pub async fn verify(&self, username: &str, password: &str) -> bool {
        let guard = self.entries.lock().await;
        guard.get(username).is_some_and(|p| p == password)
    }

    /// Salvataggio atomico: si scrive su un file temporaneo e lo si sposta
    /// al posto di quello vero con `rename`.
    ///
    /// La versione precedente faceva `tokio::fs::write` direttamente sul
    /// file finale, che lo **tronca** e poi lo riscrive: un crash a meta'
    /// lasciava un JSON tagliato. Da quando `load` rifiuta i file corrotti,
    /// quel troncamento non e' piu' solo una perdita di dati — impedisce
    /// l'avvio del server.
    ///
    /// `rename` sullo stesso filesystem e' atomica: chi legge vede o il file
    /// vecchio per intero o quello nuovo per intero, mai uno stato di mezzo.
    /// Il temporaneo sta apposta nella stessa directory del file finale,
    /// perche' `rename` fra filesystem diversi fallisce (`EXDEV`) — usare
    /// `/tmp` romperebbe tutto quando la working directory e' su un altro
    /// disco.
    async fn save(&self, json: &str) -> io::Result<()> {
        let temp_path = format!("{}.tmp", self.path);

        match Self::write_temp(&temp_path, json).await {
            Ok(()) => tokio::fs::rename(&temp_path, &self.path).await,
            Err(e) => {
                // Un temporaneo scritto a meta' non serve a nessuno: si
                // rimuove subito, cosi' il tentativo successivo riparte
                // pulito. Se anche la rimozione fallisce non c'e' molto da
                // fare, e l'errore importante e' comunque quello originale.
                let _ = tokio::fs::remove_file(&temp_path).await;
                Err(e)
            }
        }
    }

    /// Scrive il contenuto sul temporaneo e ne forza la discesa su disco.
    ///
    /// `sync_all` prima della `rename` non e' pignoleria: senza, la rename
    /// puo' diventare visibile mentre i dati sono ancora solo nella page
    /// cache. Un crash della *macchina* (non del processo) lascerebbe allora
    /// `users.json` rinominato ma vuoto — esattamente il guasto che questa
    /// funzione esiste per evitare.
    async fn write_temp(temp_path: &str, json: &str) -> io::Result<()> {
        let mut file = tokio::fs::File::create(temp_path).await?;
        file.write_all(json.as_bytes()).await?;
        file.sync_all().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Percorso temporaneo univoco per non far collidere i test tra loro
    /// (girano in parallelo) ne' con `users.json` usato dal server vero.
    fn temp_path(name: &str) -> String {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!("georuggine_test_{name}_{unique}.json"))
            .to_string_lossy()
            .into_owned()
    }

    #[tokio::test]
    async fn register_then_verify_succeeds() {
        let path = temp_path("register_verify");
        let users = UserStore::load(&path).unwrap();

        let result = users.register("alice", "password123").await;
        assert!(result.is_ok());

        assert!(users.verify("alice", "password123").await);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn register_duplicate_username_fails() {
        let path = temp_path("duplicate");
        let users = UserStore::load(&path).unwrap();

        assert!(users.register("bob", "pw-valida").await.is_ok());
        // Stesso username, anche con password diversa: deve fallire.
        let result = users.register("bob", "altra-pw").await;
        assert!(result.is_err());
        // La password originale deve essere rimasta quella buona.
        assert!(users.verify("bob", "pw-valida").await);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn verify_rejects_wrong_password() {
        let path = temp_path("wrong_password");
        let users = UserStore::load(&path).unwrap();

        users.register("carol", "correct-password").await.unwrap();

        assert!(!users.verify("carol", "wrong-password").await);
        assert!(!users.verify("utente-inesistente", "qualsiasi").await);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn load_from_nonexistent_file_starts_empty() {
        let path = temp_path("nonexistent");
        // File assente = primo avvio: `load` deve partire da una mappa vuota
        // invece di fallire l'avvio del server.
        let users = UserStore::load(&path).unwrap();

        assert!(!users.verify("chiunque", "qualsiasi").await);
    }

    #[tokio::test]
    async fn load_da_file_corrotto_fallisce_invece_di_partire_vuoto() {
        let path = temp_path("corrotto");
        std::fs::write(&path, "{ questo non e' json valido").unwrap();

        // Il caso critico: partire da vuoto qui significherebbe
        // sovrascrivere le credenziali esistenti alla prima registrazione.
        assert!(UserStore::load(&path).is_err());

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn utenti_registrati_sopravvivono_al_riavvio() {
        let path = temp_path("persistenza");

        {
            let users = UserStore::load(&path).unwrap();
            users.register("dave", "password-dave").await.unwrap();
        }

        // Nuova istanza sullo stesso file = riavvio del server.
        let users = UserStore::load(&path).unwrap();
        assert!(users.verify("dave", "password-dave").await);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn register_rifiuta_credenziali_non_valide() {
        let path = temp_path("validazione");
        let users = UserStore::load(&path).unwrap();

        assert!(users.register("", "password-valida").await.is_err());
        assert!(
            users
                .register("con spazio", "password-valida")
                .await
                .is_err()
        );
        assert!(
            users
                .register("con/slash", "password-valida")
                .await
                .is_err()
        );
        assert!(
            users
                .register(&"a".repeat(MAX_FIELD_LEN + 1), "password-valida")
                .await
                .is_err()
        );

        assert!(users.register("erin", "").await.is_err());
        assert!(users.register("erin", "abc").await.is_err());
        assert!(
            users
                .register("erin", &"a".repeat(MAX_FIELD_LEN + 1))
                .await
                .is_err()
        );

        // Nessuno dei tentativi deve aver lasciato traccia.
        assert!(!users.verify("erin", "abc").await);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn il_salvataggio_non_lascia_file_temporanei() {
        let path = temp_path("nessun_residuo");
        let users = UserStore::load(&path).unwrap();

        users.register("grace", "password-grace").await.unwrap();

        // Il temporaneo deve essere stato spostato, non lasciato indietro.
        assert!(!std::path::Path::new(&format!("{path}.tmp")).exists());
        assert!(std::path::Path::new(&path).exists());

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn un_salvataggio_fallito_non_danneggia_il_file_esistente() {
        // Il punto della scrittura atomica: se il salvataggio nuovo va male,
        // il file gia' su disco deve restare valido e leggibile.
        let dir = std::env::temp_dir().join(format!(
            "georuggine_atomica_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("users.json").to_string_lossy().into_owned();

        let users = UserStore::load(&path).unwrap();
        users.register("henry", "password-henry").await.unwrap();
        let prima = std::fs::read_to_string(&path).unwrap();

        // Facciamo sparire la cartella (rinominandola altrove, non
        // cancellandola) cosi' la scrittura del temporaneo fallisce allo
        // stesso modo su Unix e su Windows: a differenza dei permessi
        // POSIX, una cartella "di sola lettura" su Windows non impedisce
        // la creazione di nuovi file al suo interno, quindi quell'
        // approccio non e' portabile. Un percorso che non esiste piu'
        // fallisce invece identicamente ovunque.
        let dir_spostata = std::env::temp_dir().join(format!(
            "georuggine_atomica_via_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::rename(&dir, &dir_spostata).unwrap();

        let esito = users.register("iris", "password-iris").await;

        // Ripristinata subito, per poter ripulire anche se le assert sotto
        // falliscono.
        std::fs::rename(&dir_spostata, &dir).unwrap();

        assert!(esito.is_err(), "il salvataggio doveva fallire");
        // Il file precedente e' intatto, byte per byte...
        assert_eq!(std::fs::read_to_string(&path).unwrap(), prima);
        // ...e ancora caricabile: nessun troncamento che bloccherebbe l'avvio.
        let ricaricato = UserStore::load(&path).unwrap();
        assert!(ricaricato.verify("henry", "password-henry").await);
        assert!(!ricaricato.verify("iris", "password-iris").await);

        let _ = std::fs::remove_dir_all(&dir);
    }
    
    #[tokio::test]
    async fn salvataggio_fallito_non_lascia_lutente_in_memoria() {
        // Percorso non scrivibile: la directory non esiste, quindi `save`
        // fallisce. L'utente non deve risultare registrato in memoria,
        // altrimenti al riavvio sparirebbe.
        let path = std::env::temp_dir()
            .join("georuggine_dir_inesistente")
            .join("users.json")
            .to_string_lossy()
            .into_owned();

        let users = UserStore::load(&path).unwrap();
        assert!(users.register("frank", "password-frank").await.is_err());
        assert!(!users.verify("frank", "password-frank").await);
    }
}
