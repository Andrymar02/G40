//! Storage utenti minimale: username -> password in chiaro, persistito su
//! un file JSON.
//!
//! PROBLEMA (accettato per questa prima versione, da rivedere): password in
//! chiaro sia in memoria che su file. Per una versione successiva andrebbe
//! sostituito con un hash (es. `argon2`/`bcrypt`), ma è fuori scope qui —
//! decisione già segnata come aperta nel README.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Storage condiviso degli utenti registrati (username -> password),
/// persistito sul file indicato da `path`. Separato da `Clients` (in
/// `main.rs`): qui c'è chi è *registrato* (persistente), lì chi è
/// *connesso adesso* (effimero, si azzera ad ogni riavvio).
pub struct UserStore {
    entries: Mutex<HashMap<String, String>>,
    path: String,
}

/// Handle condiviso tra i task, come `Clients` in `main.rs`.
pub type Users = Arc<UserStore>;

impl UserStore {
    /// Carica la mappa utenti dal file indicato. Se il file non esiste
    /// ancora (primo avvio) o è illeggibile/corrotto, parte da una mappa
    /// vuota invece di fallire l'avvio del server.
    pub fn load(path: &str) -> Users {
        let map = std::fs::read_to_string(path)
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
            .unwrap_or_default();
        Arc::new(UserStore {
            entries: Mutex::new(map),
            path: path.to_string(),
        })
    }

    /// Registra un nuovo utente: fallisce se lo username è già presente.
    /// In caso di successo, salva subito la mappa aggiornata su file.
    pub async fn register(&self, username: &str, password: &str) -> Result<(), String> {
        let mut guard = self.entries.lock().await;
        if guard.contains_key(username) {
            return Err("username già in uso".to_string());
        }
        guard.insert(username.to_string(), password.to_string());

        let json = serde_json::to_string_pretty(&*guard).map_err(|e| e.to_string())?;
        self.save(&json).await.map_err(|e| e.to_string())
    }

    /// Verifica che `username`/`password` corrispondano a un utente registrato.
    pub async fn verify(&self, username: &str, password: &str) -> bool {
        let guard = self.entries.lock().await;
        guard.get(username).is_some_and(|p| p == password)
    }

    async fn save(&self, json: &str) -> io::Result<()> {
        tokio::fs::write(&self.path, json).await
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
        let users = UserStore::load(&path);

        let result = users.register("alice", "password123").await;
        assert!(result.is_ok());

        assert!(users.verify("alice", "password123").await);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn register_duplicate_username_fails() {
        let path = temp_path("duplicate");
        let users = UserStore::load(&path);

        assert!(users.register("bob", "pw1").await.is_ok());
        // Stesso username, anche con password diversa: deve fallire.
        let result = users.register("bob", "pw2").await;
        assert!(result.is_err());

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn verify_rejects_wrong_password() {
        let path = temp_path("wrong_password");
        let users = UserStore::load(&path);

        users.register("carol", "correct-password").await.unwrap();

        assert!(!users.verify("carol", "wrong-password").await);
        assert!(!users.verify("utente-inesistente", "qualsiasi").await);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn load_from_nonexistent_file_starts_empty() {
        let path = temp_path("nonexistent");
        // Non creiamo il file: `load` deve partire da una mappa vuota
        // invece di fallire l'avvio del server.
        let users = UserStore::load(&path);

        assert!(!users.verify("chiunque", "qualsiasi").await);
    }
}
