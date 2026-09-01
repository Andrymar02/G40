//! Registro delle sessioni attive: chi è connesso *adesso* e come
//! raggiungerlo.
//!
//! Sostituisce la vecchia `Arc<Mutex<HashMap<String, Sender>>>` indicizzata
//! per solo username. Il problema di quella forma era che l'username da solo
//! non identifica una *connessione*: due socket diversi dello stesso utente
//! erano indistinguibili, quindi
//!
//! * un secondo login sovrascriveva silenziosamente il sender del primo;
//! * la disconnessione della vecchia connessione rimuoveva dalla mappa la
//!   voce della nuova, lasciando un utente connesso ma irraggiungibile.
//!
//! Qui ogni connessione riceve un [`SessionId`] univoco alla accept, e ogni
//! operazione distruttiva (logout, pulizia dei canali chiusi) confronta l'id
//! prima di rimuovere: si tocca solo la propria sessione, mai quella di
//! qualcun altro.

use common::ServerMessage;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex, mpsc};

/// Identificatore di una singola connessione, univoco per l'intera vita del
/// processo. Non viene mai riusato: un id vecchio non può collidere con una
/// sessione nuova, che è la proprietà su cui si regge tutta la pulizia.
pub type SessionId = u64;

/// Una sessione autenticata: l'id della connessione e il canale verso il suo
/// `writer_task`.
struct Session {
    id: SessionId,
    sender: mpsc::Sender<ServerMessage>,
}

/// Motivo per cui un login viene rifiutato dal registro (le credenziali sono
/// già state verificate altrove: qui si controlla solo la disponibilità).
#[derive(Debug, PartialEq)]
pub enum LoginRejected {
    /// Esiste già una sessione attiva per quell'username. Come da
    /// ORGANIZZAZIONE.md ("rifiutare un secondo login con lo stesso
    /// username") vince chi è arrivato prima: la connessione esistente non
    /// viene toccata.
    AlreadyConnected,
}

/// Esito di un invio a un singolo utente.
#[derive(Debug, PartialEq)]
pub enum DeliveryError {
    /// Nessuna sessione attiva per quell'username.
    NotConnected,
    /// La sessione esisteva ma il suo canale era chiuso (writer task
    /// terminato): la voce è stata rimossa dal registro.
    ChannelClosed,
}

/// Registro condiviso tra tutti i task del server.
pub struct SessionRegistry {
    sessions: Mutex<HashMap<String, Session>>,
    next_id: AtomicU64,
}

/// Handle condiviso, con lo stesso pattern di `Users`/`Positions`.
pub type Clients = Arc<SessionRegistry>;

impl SessionRegistry {
    pub fn new() -> Clients {
        Arc::new(SessionRegistry {
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        })
    }

    /// Assegna un id alla connessione appena accettata, *prima* che l'utente
    /// si autentichi: serve già in fase di pulizia, perché una connessione
    /// può chiudersi senza aver mai fatto login.
    pub fn allocate_session(&self) -> SessionId {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Registra la sessione `id` come titolare di `username`.
    ///
    /// Fallisce se l'username ha già una sessione attiva: il chiamante deve
    /// rispondere `LoginErr` e non considerare il client autenticato.
    pub async fn login(
        &self,
        username: &str,
        id: SessionId,
        sender: mpsc::Sender<ServerMessage>,
    ) -> Result<(), LoginRejected> {
        let mut guard = self.sessions.lock().await;
        if guard.contains_key(username) {
            return Err(LoginRejected::AlreadyConnected);
        }
        guard.insert(username.to_string(), Session { id, sender });
        Ok(())
    }

    /// Rimuove la sessione `id` di `username`, ma **solo se è ancora quella
    /// registrata**. Restituisce `true` se la rimozione è avvenuta.
    ///
    /// Il confronto sull'id è il punto centrale: una connessione che termina
    /// tardi non deve poter sfrattare la sessione di qualcun altro che nel
    /// frattempo ha preso quell'username.
    pub async fn logout(&self, username: &str, id: SessionId) -> bool {
        let mut guard = self.sessions.lock().await;
        match guard.get(username) {
            Some(session) if session.id == id => {
                guard.remove(username);
                true
            }
            _ => false,
        }
    }

    /// Manda un messaggio al solo `username`, se connesso.
    ///
    /// Il lock viene rilasciato prima dell'invio: un client lento non deve
    /// bloccare login e logout degli altri.
    pub async fn send_to(&self, username: &str, msg: ServerMessage) -> Result<(), DeliveryError> {
        let target = {
            let guard = self.sessions.lock().await;
            guard
                .get(username)
                .map(|session| (session.id, session.sender.clone()))
        };

        let (id, sender) = target.ok_or(DeliveryError::NotConnected)?;

        if sender.send(msg).await.is_err() {
            self.prune(&[(username.to_string(), id)]).await;
            return Err(DeliveryError::ChannelClosed);
        }
        Ok(())
    }

    /// Manda `msg` a tutte le sessioni attive.
    ///
    /// Esisteva anche una variante che saltava un utente, usata dalle
    /// notifiche di cambio stato agli altri client: quelle non vengono piu'
    /// inviate (vedi `state.rs`), quindi non serve piu' escludere nessuno.
    pub async fn broadcast(&self, msg: ServerMessage) {
        let targets: Vec<(String, SessionId, mpsc::Sender<ServerMessage>)> = {
            let guard = self.sessions.lock().await;
            guard
                .iter()
                .map(|(username, session)| (username.clone(), session.id, session.sender.clone()))
                .collect()
        };

        // I canali risultati chiusi si accumulano qui e si rimuovono in un
        // colpo solo alla fine, per non riprendere il lock a ogni invio.
        let mut stale = Vec::new();
        for (username, id, sender) in targets {
            if sender.send(msg.clone()).await.is_err() {
                crate::detail!("invio a {username} fallito: canale chiuso");
                stale.push((username, id));
            }
        }

        if !stale.is_empty() {
            self.prune(&stale).await;
        }
    }

    /// Elimina le sessioni elencate, sempre confrontando l'id: se nel
    /// frattempo l'username è stato preso da una connessione nuova, la voce
    /// nuova resta.
    async fn prune(&self, stale: &[(String, SessionId)]) {
        let mut guard = self.sessions.lock().await;
        for (username, id) in stale {
            if guard.get(username).is_some_and(|s| s.id == *id) {
                guard.remove(username);
            }
        }
    }

    /// Elenco degli utenti connessi, ordinato. Usato dalla console operatore.
    pub async fn connected_usernames(&self) -> Vec<String> {
        let guard = self.sessions.lock().await;
        let mut names: Vec<String> = guard.keys().cloned().collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sessione fittizia: tiene vivo il `Receiver` per non far risultare il
    /// canale chiuso, e permette di leggere cosa è stato consegnato.
    fn fake_session() -> (mpsc::Sender<ServerMessage>, mpsc::Receiver<ServerMessage>) {
        mpsc::channel(8)
    }

    #[tokio::test]
    async fn login_su_username_libero_riesce() {
        let registry = SessionRegistry::new();
        let (tx, _rx) = fake_session();
        let id = registry.allocate_session();

        assert!(registry.login("alice", id, tx).await.is_ok());
        assert_eq!(registry.connected_usernames().await, vec!["alice"]);
    }

    #[tokio::test]
    async fn secondo_login_stesso_username_viene_rifiutato() {
        let registry = SessionRegistry::new();
        let (tx1, mut rx1) = fake_session();
        let (tx2, _rx2) = fake_session();

        let id1 = registry.allocate_session();
        let id2 = registry.allocate_session();
        registry.login("alice", id1, tx1).await.unwrap();

        // Seconda connessione, stesso username: deve essere respinta.
        assert_eq!(
            registry.login("alice", id2, tx2).await,
            Err(LoginRejected::AlreadyConnected)
        );

        // ...e la prima sessione deve essere rimasta raggiungibile.
        registry
            .send_to("alice", ServerMessage::LoginOk)
            .await
            .unwrap();
        assert!(rx1.recv().await.is_some());
    }

    #[tokio::test]
    async fn logout_di_una_sessione_vecchia_non_rimuove_quella_corrente() {
        let registry = SessionRegistry::new();
        let (tx_vecchia, _rx_vecchia) = fake_session();
        let (tx_nuova, mut rx_nuova) = fake_session();

        let id_vecchia = registry.allocate_session();
        let id_nuova = registry.allocate_session();

        registry
            .login("alice", id_vecchia, tx_vecchia)
            .await
            .unwrap();
        registry.logout("alice", id_vecchia).await;
        registry.login("alice", id_nuova, tx_nuova).await.unwrap();

        // Il task della vecchia connessione termina in ritardo e prova a
        // ripulire: non deve toccare la sessione nuova.
        assert!(!registry.logout("alice", id_vecchia).await);

        registry
            .send_to("alice", ServerMessage::LoginOk)
            .await
            .unwrap();
        assert!(rx_nuova.recv().await.is_some());
    }

    #[tokio::test]
    async fn logout_con_id_corretto_rimuove_la_sessione() {
        let registry = SessionRegistry::new();
        let (tx, _rx) = fake_session();
        let id = registry.allocate_session();

        registry.login("alice", id, tx).await.unwrap();

        assert!(registry.logout("alice", id).await);
        assert!(registry.connected_usernames().await.is_empty());
        // Dopo il logout l'username torna disponibile.
        let (tx2, _rx2) = fake_session();
        let id2 = registry.allocate_session();
        assert!(registry.login("alice", id2, tx2).await.is_ok());
    }

    #[tokio::test]
    async fn invio_a_utente_non_connesso_segnala_not_connected() {
        let registry = SessionRegistry::new();

        assert_eq!(
            registry.send_to("nessuno", ServerMessage::LoginOk).await,
            Err(DeliveryError::NotConnected)
        );
    }

    #[tokio::test]
    async fn canale_chiuso_viene_rimosso_dal_registro() {
        let registry = SessionRegistry::new();
        let (tx, rx) = fake_session();
        let id = registry.allocate_session();
        registry.login("alice", id, tx).await.unwrap();

        // Il writer task termina: il receiver viene droppato.
        drop(rx);

        assert_eq!(
            registry.send_to("alice", ServerMessage::LoginOk).await,
            Err(DeliveryError::ChannelClosed)
        );
        // La voce morta non deve restare a occupare l'username.
        assert!(registry.connected_usernames().await.is_empty());
    }

    #[tokio::test]
    async fn broadcast_rimuove_i_canali_chiusi_e_consegna_agli_altri() {
        let registry = SessionRegistry::new();
        let (tx_viva, mut rx_viva) = fake_session();
        let (tx_morta, rx_morta) = fake_session();

        let id_viva = registry.allocate_session();
        let id_morta = registry.allocate_session();
        registry.login("alice", id_viva, tx_viva).await.unwrap();
        registry.login("bob", id_morta, tx_morta).await.unwrap();
        drop(rx_morta);

        registry
            .broadcast(ServerMessage::Broadcast {
                from: "server".to_string(),
                text: "ciao".to_string(),
            })
            .await;

        assert!(rx_viva.recv().await.is_some());
        assert_eq!(registry.connected_usernames().await, vec!["alice"]);
    }

    #[tokio::test]
    async fn gli_id_di_sessione_non_si_ripetono() {
        let registry = SessionRegistry::new();
        let primo = registry.allocate_session();
        let secondo = registry.allocate_session();

        assert_ne!(primo, secondo);
    }
}
