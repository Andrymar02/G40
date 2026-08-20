mod state;
mod users;

use common::framing;
use common::{ClientMessage, ServerMessage};
use state::{Positions, SharedClock, SystemClock};
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use users::Users;

/// File in cui persistono le credenziali registrate (JSON, vedi `users`).
/// Percorso relativo alla working directory da cui si lancia `cargo run -p
/// server` (di norma la radice del workspace).
const USERS_FILE: &str = "users.json";

/// Indirizzo:porta di bind di default, usato se non se ne passa uno da riga
/// di comando (`cargo run -p server -- <indirizzo:porta>`).
const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";

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
///
/// `pub(crate)` perché `state.rs` (la macchina a stati) deve poterla usare
/// per notificare `StateChanged` agli altri client connessi.
pub(crate) type Clients = Arc<Mutex<HashMap<String, mpsc::Sender<ServerMessage>>>>;

#[tokio::main]
async fn main() -> io::Result<()> {
    // Indirizzo:porta configurabile da riga di comando, con default se
    // omesso — utile per far girare più server sulla stessa macchina
    // durante i test, o per un ambiente diverso da quello locale.
    let bind_addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_BIND_ADDR.to_string());

    let listener = TcpListener::bind(&bind_addr).await?;
    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
    let users: Users = users::UserStore::load(USERS_FILE);
    let positions: Positions = state::new_positions();
    let clock: SharedClock = Arc::new(SystemClock);

    println!("Server in ascolto su {bind_addr}");

    // Il loop di accept gira in un task a parte: il task principale resta
    // libero per la console dell'operatore (broadcast/diretto), che deve
    // poter leggere da stdin senza aspettare nuove connessioni.
    {
        let clients = clients.clone();
        let users = users.clone();
        let positions = positions.clone();
        let clock = clock.clone();
        tokio::spawn(async move {
            loop {
                let (socket, addr) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        println!("errore in accept: {e}");
                        continue;
                    }
                };
                let clients = clients.clone();
                let users = users.clone();
                let positions = positions.clone();
                let clock = clock.clone();
                println!("{:?} {:?}", socket, addr);
                tokio::spawn(async move {
                    handle_client(socket, addr, clients, users, positions, clock).await;
                });
            }
        });
    }

    admin_console(clients).await;
    Ok(())
}

/// Console dell'operatore lato server: legge righe da stdin del processo
/// server (non della rete) e decide se mandarle in broadcast o a un singolo
/// utente. È l'unica sorgente di `ServerMessage::Broadcast`/`Direct`: per
/// consegna, la scelta broadcast-vs-diretto spetta al server, non a un
/// client che indica un destinatario (i messaggi che i client mandano con
/// `ChatToServer` restano infatti solo lato server, vedi `handle_client`).
async fn admin_console(clients: Clients) {
    println!("Console operatore: testo libero = broadcast, '/msg <utente> <testo>' = diretto.");
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(rest) = line.strip_prefix("/msg ") {
            match rest.split_once(' ') {
                Some((to, text)) if !text.is_empty() => direct_message(&clients, to, text).await,
                _ => println!("Uso: /msg <utente> <testo>"),
            }
        } else {
            broadcast(&clients, line).await;
        }
    }
}

/// Task dedicato alla scrittura verso un client: è l'unico proprietario
/// della sua `write_half`, quindi le scritture non contendono mai un lock
/// condiviso. Riceve i messaggi da spedire dal channel `rx` e termina
/// quando tutti i `Sender` clonati vengono droppati (client rimosso dalla
/// mappa) o quando la scrittura sul socket fallisce.
async fn writer_task(
    mut write_half: OwnedWriteHalf,
    mut rx: mpsc::Receiver<ServerMessage>,
    addr: SocketAddr,
) {
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
    clock: SharedClock,
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
            ClientMessage::Login {
                username: u,
                password: p,
            } => {
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
                        .send(ServerMessage::LoginErr(
                            "credenziali non valide".to_string(),
                        ))
                        .await;
                }
            }
            ClientMessage::Register {
                username: u,
                password: p,
            } => match users.register(&u, &p).await {
                Ok(()) => {
                    println!("[{addr}] registrazione: {u}");
                    let _ = tx.send(ServerMessage::RegisterOk).await;
                }
                Err(reason) => {
                    println!("[{addr}] registrazione fallita per {u}: {reason}");
                    let _ = tx.send(ServerMessage::RegisterErr(reason)).await;
                }
            },
            ClientMessage::PositionUpdate {
                lat,
                lon,
                timestamp,
            } => {
                // `timestamp` è quello dichiarato dal client: loggato per
                // debug, ma la macchina a stati (in `state::update_position`)
                // non lo usa più per le transizioni — vedi commento su
                // `state::Clock`. Resta nel protocollo perché potrà servire
                // allo storico posizioni (Viorel).
                println!("[{addr}] posizione: ({lat}, {lon}) @ {timestamp}");
                match &username {
                    Some(u) => {
                        state::update_position(&clients, &positions, &clock, u, lat, lon).await;
                    }
                    None => println!("[{addr}] posizione ignorata: utente non loggato"),
                }
            }
            ClientMessage::ChatToServer { text } => match &username {
                // Solo ricevuto e loggato: la consegna non chiede che un
                // messaggio mandato da un utente venga girato ad altri
                // utenti, quindi qui non c'è nessun broadcast/inoltro.
                Some(u) => println!("[{addr}] messaggio da {u}: {text}"),
                None => println!("[{addr}] messaggio ignorato: utente non loggato"),
            },
        }
    }

    if let Some(u) = username {
        clients.lock().await.remove(&u);
        println!("[{addr}] disconnesso: {u}");
        state::mark_disconnected(&clients, &positions, &u).await;
    }
}

/// Manda un messaggio di testo in broadcast a tutti i client connessi.
/// Chiamata solo dalla console dell'operatore (`admin_console`): è il
/// server stesso il mittente, non un client che chiede di essere rimbalzato
/// ad altri.
///
/// Il lock su `clients` è tenuto solo per clonare i `Sender`; l'invio ai
/// singoli client (e l'eventuale attesa per backpressure) avviene dopo
/// averlo rilasciato, così un client lento non blocca l'inserimento/rimozione
/// di altri client nella mappa.
async fn broadcast(clients: &Clients, text: &str) {
    let senders: Vec<(String, mpsc::Sender<ServerMessage>)> = {
        let guard = clients.lock().await;
        guard
            .iter()
            .map(|(username, sender)| (username.clone(), sender.clone()))
            .collect()
    };

    for (username, sender) in senders {
        let msg = ServerMessage::Broadcast {
            from: "server".to_string(),
            text: text.to_string(),
        };
        if sender.send(msg).await.is_err() {
            println!("invio a {username} fallito: canale chiuso");
        }
    }
}

/// Manda un messaggio di testo a un solo utente (`to`), se connesso.
/// Come `broadcast`, chiamata solo da `admin_console`: è il server a
/// scegliere il destinatario, non un client. Eventuali errori (utente non
/// connesso, canale chiuso) restano lato server: non c'è nessun client
/// "mittente" a cui rispedirli.
async fn direct_message(clients: &Clients, to: &str, text: &str) {
    let target = {
        let guard = clients.lock().await;
        guard.get(to).cloned()
    };

    match target {
        Some(target_tx) => {
            let msg = ServerMessage::Direct {
                from: "server".to_string(),
                text: text.to_string(),
            };
            if target_tx.send(msg).await.is_err() {
                println!("invio a {to} fallito: canale chiuso");
            }
        }
        None => println!("utente '{to}' non connesso"),
    }
}
