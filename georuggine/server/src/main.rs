mod users;

use common::framing;
use common::{ClientMessage, ServerMessage};
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

/// Pattern attore: ogni client è raggiungibile tramite il `Sender` della sua
/// coda di messaggi in uscita, non più tramite la sua `OwnedWriteHalf`
/// diretta. Chi vuole scrivere a un client fa `sender.send(msg).await`:
/// nessun lock condiviso viene tenuto durante l'I/O di rete, solo durante
/// la lookup/clone del sender nella mappa.
type Clients = Arc<Mutex<HashMap<String, mpsc::Sender<ServerMessage>>>>;

#[tokio::main]
async fn main() -> io::Result<()> {
    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
    let users: Users = users::UserStore::load(USERS_FILE);

    println!("Server in ascolto su 0.0.0.0:8080");

    loop {
        // Accetto nuovo client connesso
        let (socket, addr) = listener.accept().await?;
        let clients = clients.clone();
        let users = users.clone();
        println!("{:?} {:?}", socket, addr);
        // Creo task tokio per quel client
        tokio::spawn(async move {
            handle_client(socket, addr, clients, users).await;
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

async fn handle_client(socket: TcpStream, addr: SocketAddr, clients: Clients, users: Users) {
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
                // PROBLEMA: qui andrebbe calcolato/aggiornato lo UserState
                // e spedito un ServerMessage::StateChanged agli altri
                // client interessati. Non implementato in questo
                // scheletro: la logica di stato (quando si passa da Idle a
                // Moving, ecc.) va decisa come gruppo prima di scriverla.
            }
            ClientMessage::ChatToServer { text } => match &username {
                Some(u) => broadcast(&clients, u, &text).await,
                None => println!("[{addr}] chat ignorata: utente non loggato"),
            },
        }
    }

    if let Some(u) = username {
        clients.lock().await.remove(&u);
        println!("[{addr}] disconnesso: {u}");
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
