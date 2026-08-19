mod users;

use common::framing;
use common::{ClientMessage, ServerMessage, UserState};
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use users::Users;

/// File in cui persistono le credenziali registrate (JSON, vedi `users`).
/// Percorso relativo alla working directory da cui si lancia `cargo run -p
/// server` (di norma la radice del workspace).
const USERS_FILE: &str = "users.json";

/// Capacità del channel per-client: quanti `ServerMessage` possono restare
/// in coda prima che chi manda (`send_to`/`broadcast`) resti in attesa sul
/// `.send().await`. Un valore arbitrario per questa fase; da rivedere in
/// base al traffico reale.
const CLIENT_CHANNEL_CAPACITY: usize = 32;

/// Soglia (in secondi) di posizione invariata oltre la quale un utente
/// "in movimento" torna "fermo", come richiesto dalla consegna (3 minuti).
/// Confrontata contro `timestamp` dei `PositionUpdate` (fornito dal client),
/// non contro l'orologio del server: coerente con il fatto che è già il
/// client a scandire l'invio ogni 30 secondi.
const IDLE_THRESHOLD_SECONDS: i64 = 180;

/// Pattern attore: ogni client è raggiungibile tramite il `Sender` della sua
/// coda di messaggi in uscita, non più tramite la sua `OwnedWriteHalf`
/// diretta. Chi vuole scrivere a un client fa `sender.send(msg).await`:
/// nessun lock condiviso viene tenuto durante l'I/O di rete, solo durante
/// la lookup/clone del sender nella mappa.
type Clients = Arc<Mutex<HashMap<String, mpsc::Sender<ServerMessage>>>>;

/// Ultima posizione nota e stato corrente di un utente. Vive solo in
/// memoria (si azzera ad ogni riavvio del server): la persistenza dello
/// storico posizioni, necessaria per l'analisi del movimento (tragitto,
/// velocità media, ecc.), è un pezzo successivo, non ancora implementato.
#[derive(Clone)]
struct PositionRecord {
    lat: f64,
    lon: f64,
    /// Timestamp (fornito dal client) dell'ultimo `PositionUpdate` in cui
    /// le coordinate sono effettivamente cambiate. Usato per calcolare da
    /// quanto tempo l'utente è fermo.
    last_change_ts: i64,
    state: UserState,
}

/// Stato di posizione/movimento per utente, con lo stesso pattern di
/// `Clients`/`Users`: `Arc<Mutex<HashMap<...>>>` condiviso tra i task.
type Positions = Arc<Mutex<HashMap<String, PositionRecord>>>;

#[tokio::main]
async fn main() -> io::Result<()> {
    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
    let users: Users = users::UserStore::load(USERS_FILE);
    let positions: Positions = Arc::new(Mutex::new(HashMap::new()));

    println!("Server in ascolto su 0.0.0.0:8080");

    loop {
        // Accetto nuovo client connesso
        let (socket, addr) = listener.accept().await?;
        let clients = clients.clone();
        let users = users.clone();
        let positions = positions.clone();
        println!("{:?} {:?}", socket, addr);
        // Creo task tokio per quel client
        tokio::spawn(async move {
            handle_client(socket, addr, clients, users, positions).await;
        });
    }
}

/// Task dedicato alla scrittura verso un client: è l'unico proprietario
/// della sua `write_half`, quindi le scritture non contendono mai un lock
/// condiviso. Riceve i messaggi da spedire dal channel `rx` e termina
/// quando tutti i `Sender` clonati vengono droppati (client rimosso dalla
/// mappa) o quando la scrittura sul socket fallisce.
async fn writer_task(mut write_half: OwnedWriteHalf, mut rx: mpsc::Receiver<ServerMessage>, addr: SocketAddr) {
    while let Some(msg) = rx.recv().await {
        if let Err(e) = framing::send_message(&mut write_half, &msg).await {
            println!("[{addr}] invio fallito: {e}");
            break;
        }
    }
}

async fn handle_client(
    socket: TcpStream,
    addr: SocketAddr,
    clients: Clients,
    users: Users,
    positions: Positions,
) {
    let (mut read_half, write_half) = socket.into_split();

    let (tx, rx) = mpsc::channel::<ServerMessage>(CLIENT_CHANNEL_CAPACITY);
    tokio::spawn(writer_task(write_half, rx, addr));

    // Username noto solo dopo un Login riuscito; usato anche come guard
    // contro un secondo Login sulla stessa connessione.
    let mut username: Option<String> = None;

    loop {
        let msg: ClientMessage = match framing::read_message(&mut read_half).await {
            Ok(msg) => msg,
            // uscita dal loop handle avviene qui quando altro lato tcp si stacca (client)
            Err(e) => {
                println!("[{addr}] connessione chiusa o errore di lettura: {e}");
                break;
            }
        };

        match msg {
            ClientMessage::Login { username: u, password: p } => {
                if username.is_some() {
                    // Già loggato in questa stessa connessione: ignora.
                    println!("[{addr}] login ripetuto ignorato");
                } else if users.verify(&u, &p).await {
                    println!("[{addr}] login: {u}");
                    clients.lock().await.insert(u.clone(), tx.clone());
                    username = Some(u.clone());
                    let _ = tx.send(ServerMessage::LoginOk).await;
                } else {
                    println!("[{addr}] login fallito: {u}");
                    let _ = tx
                        .send(ServerMessage::LoginErr("credenziali non valide".to_string()))
                        .await;
                }
            }
            ClientMessage::Register { username: u, password: p } => {
                match users.register(&u, &p).await {
                    Ok(()) => {
                        println!("[{addr}] registrazione: {u}");
                        let _ = tx.send(ServerMessage::RegisterOk).await;
                    }
                    Err(reason) => {
                        println!("[{addr}] registrazione fallita per {u}: {reason}");
                        let _ = tx.send(ServerMessage::RegisterErr(reason)).await;
                    }
                }
            }
            ClientMessage::PositionUpdate { lat, lon, timestamp } => {
                println!("[{addr}] posizione: ({lat}, {lon}) @ {timestamp}");
                match &username {
                    Some(u) => update_position(&clients, &positions, u, lat, lon, timestamp).await,
                    None => println!("[{addr}] posizione ignorata: utente non loggato"),
                }
            }
            ClientMessage::ChatToServer { text } => match &username {
                Some(u) => broadcast(&clients, u, &text).await,
                None => println!("[{addr}] chat ignorata: utente non loggato"),
            },
            ClientMessage::ChatDirect { to, text } => match &username {
                Some(u) => direct_message(&clients, &tx, u, &to, &text).await,
                None => println!("[{addr}] messaggio diretto ignorato: utente non loggato"),
            },
        }
    }

    if let Some(u) = username {
        clients.lock().await.remove(&u);
        println!("[{addr}] disconnesso: {u}");
        mark_disconnected(&clients, &positions, &u).await;
    }
}

/// Aggiorna posizione e stato di `username` a fronte di un `PositionUpdate`,
/// applicando le transizioni richieste dalla consegna, e notifica gli altri
/// client solo se lo stato è effettivamente cambiato.
///
/// - Se le coordinate sono diverse da quelle memorizzate (comprese quelle
///   della primissima posizione mai ricevuta) → stato `Moving`.
/// - Se sono uguali e l'utente era `Moving` da almeno [`IDLE_THRESHOLD_SECONDS`]
///   (cioè da almeno 3 minuti, calcolati sul `timestamp` fornito dal client)
///   → stato `Idle`.
/// - Altrimenti nessun cambiamento (es. utente già `Idle` e fermo, o
///   `Moving` da meno di 3 minuti).
async fn update_position(
    clients: &Clients,
    positions: &Positions,
    username: &str,
    lat: f64,
    lon: f64,
    timestamp: i64,
) {
    let new_state = {
        let mut guard = positions.lock().await;
        let entry = guard.entry(username.to_string()).or_insert(PositionRecord {
            lat,
            lon,
            last_change_ts: timestamp,
            state: UserState::Idle,
        });
        let previous_state = entry.state.clone();

        if entry.lat != lat || entry.lon != lon {
            entry.lat = lat;
            entry.lon = lon;
            entry.last_change_ts = timestamp;
            entry.state = UserState::Moving;
        } else if entry.state == UserState::Moving
            && timestamp - entry.last_change_ts >= IDLE_THRESHOLD_SECONDS
        {
            entry.state = UserState::Idle;
        }

        (entry.state != previous_state).then(|| entry.state.clone())
    };

    if let Some(state) = new_state {
        println!("[{username}] stato -> {state:?}");
        broadcast_state_change(clients, username, state).await;
    }
}

/// Segna `username` come `Disconnected` (se aveva già una posizione nota) e
/// notifica gli altri client. Chiamata quando il loop di lettura di
/// `handle_client` esce, cioè quando il client si disconnette.
async fn mark_disconnected(clients: &Clients, positions: &Positions, username: &str) {
    let changed = {
        let mut guard = positions.lock().await;
        match guard.get_mut(username) {
            Some(entry) if entry.state != UserState::Disconnected => {
                entry.state = UserState::Disconnected;
                true
            }
            // Mai ricevuta una posizione, o già segnato: niente da notificare.
            _ => false,
        }
    };

    if changed {
        broadcast_state_change(clients, username, UserState::Disconnected).await;
    }
}

/// Notifica a tutti i client connessi (tranne l'utente il cui stato è
/// cambiato) un `ServerMessage::StateChanged`. Stesso pattern lock-poi-invia
/// di `broadcast`/`direct_message`.
async fn broadcast_state_change(clients: &Clients, username: &str, new_state: UserState) {
    let senders: Vec<(String, mpsc::Sender<ServerMessage>)> = {
        let guard = clients.lock().await;
        guard
            .iter()
            .filter(|(u, _)| u.as_str() != username)
            .map(|(u, sender)| (u.clone(), sender.clone()))
            .collect()
    };

    for (u, sender) in senders {
        let msg = ServerMessage::StateChanged {
            username: username.to_string(),
            new_state: new_state.clone(),
        };
        if sender.send(msg).await.is_err() {
            println!("notifica stato a {u} fallita: canale chiuso");
        }
    }
}

/// Inoltra un messaggio di chat a tutti i client connessi tranne il
/// mittente (broadcast).
///
/// Come `send_to`, il lock è tenuto solo per clonare i `Sender` rilevanti;
/// l'invio ai singoli client (e l'eventuale attesa per backpressure) avviene
/// dopo averlo rilasciato, quindi un client lento non blocca più login,
/// disconnessioni o l'inserimento di altri client nella mappa.
async fn broadcast(clients: &Clients, from: &str, text: &str) {
    let senders: Vec<(String, mpsc::Sender<ServerMessage>)> = {
        let guard = clients.lock().await;
        guard
            .iter()
            .filter(|(username, _)| username.as_str() != from)
            .map(|(username, sender)| (username.clone(), sender.clone()))
            .collect()
    };

    for (username, sender) in senders {
        let msg = ServerMessage::Broadcast {
            from: from.to_string(),
            text: text.to_string(),
        };
        if sender.send(msg).await.is_err() {
            println!("invio a {username} fallito: canale chiuso");
        }
    }
}

/// Inoltra un messaggio di chat a un solo utente (`to`), se connesso.
///
/// `sender_tx` è il canale del *mittente* (non del destinatario): serve per
/// rispedirgli un [`ServerMessage::DirectErr`] quando la consegna non è
/// possibile (destinatario non connesso, o suo canale chiuso). Come in
/// `broadcast`, il lock su `clients` è tenuto solo per la lookup/clone del
/// `Sender`, mai durante l'invio.
async fn direct_message(
    clients: &Clients,
    sender_tx: &mpsc::Sender<ServerMessage>,
    from: &str,
    to: &str,
    text: &str,
) {
    if to == from {
        let _ = sender_tx
            .send(ServerMessage::DirectErr(
                "non puoi mandare un messaggio diretto a te stesso".to_string(),
            ))
            .await;
        return;
    }

    let target = {
        let guard = clients.lock().await;
        guard.get(to).cloned()
    };

    match target {
        Some(target_tx) => {
            let msg = ServerMessage::Direct {
                from: from.to_string(),
                text: text.to_string(),
            };
            if target_tx.send(msg).await.is_err() {
                let _ = sender_tx
                    .send(ServerMessage::DirectErr(format!(
                        "invio a {to} fallito: canale chiuso"
                    )))
                    .await;
            }
        }
        None => {
            let _ = sender_tx
                .send(ServerMessage::DirectErr(format!("utente '{to}' non connesso")))
                .await;
        }
    }
}
