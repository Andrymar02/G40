use common::framing;
use common::{ClientMessage, ServerMessage};
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

/// Nota: nello scheletro proposto la mappa condivisa è `HashMap<String,
/// TcpStream>`. Un `TcpStream` intero non si può però tenere sia nel task
/// che lo legge (`handle_client`) sia nella mappa condivisa da cui altri
/// task scrivono: sarebbero due proprietari dello stesso valore. Per far
/// girare lo scheletro si è quindi usato `socket.into_split()`, tenendo la
/// metà di lettura nel task del client e mettendo solo la metà di
/// scrittura nella mappa.
///
/// PROBLEMA (quello segnalato nella nota): per scrivere a un client
/// bisogna comunque fare il lock dell'intera `Mutex<HashMap<...>>` e
/// tenerlo per la durata di una `.await` di rete (vedi `send_to` e
/// `broadcast`). Con più client questo serializza tutte le scritture e
/// blocca anche i client che non c'entrano nulla con quel messaggio. È
/// esattamente il motivo per cui in Fase 2 va sostituito con un
/// `mpsc::Sender<ServerMessage>` per client (pattern "attore"): ogni client
/// avrebbe il proprio task che possiede la sua write-half e riceve i
/// messaggi da inoltrare via channel, senza lock condivisi sull'I/O.
type Clients = Arc<Mutex<HashMap<String, OwnedWriteHalf>>>;

#[tokio::main]
async fn main() -> io::Result<()> {
    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));

    println!("Server in ascolto su 0.0.0.0:8080");

    loop {
        let (socket, addr) = listener.accept().await?;
        let clients = clients.clone();
        println!("{:?} {:?}", socket, addr);
        tokio::spawn(async move {
            handle_client(socket, addr, clients).await;
        });
    }
}

async fn handle_client(socket: TcpStream, addr: SocketAddr, clients: Clients) {
    let (mut read_half, write_half) = socket.into_split();

    // La write-half viene spostata nella mappa condivisa al momento del
    // login (vedi sotto); fino ad allora resta qui, `Some`.
    let mut write_half = Some(write_half);
    // Username noto solo dopo un Login riuscito.
    // PROBLEMA: prima del login il client non è registrato nella mappa,
    // quindi non è ancora raggiungibile da nessuno (né broadcast né
    // messaggi diretti) — accettabile per uno scheletro, da rivedere
    // quando si implementerà l'auth vera.
    let mut username: Option<String> = None;

    loop {
        let msg: ClientMessage = match framing::read_message(&mut read_half).await {
            Ok(msg) => msg,
            Err(e) => {
                println!("[{addr}] connessione chiusa o errore di lettura: {e}");
                break;
            }
        };

        match msg {
            ClientMessage::Login { username: u, password: _ } => {
                // PROBLEMA: nessuna verifica reale della password e nessun
                // controllo di username duplicati (un secondo login con lo
                // stesso username sovrascrive silenziosamente il primo
                // nella mappa) — manca uno storage utenti, fuori scope per
                // questo scheletro. Qui si accetta sempre.
                match write_half.take() {
                    Some(w) => {
                        println!("[{addr}] login: {u}");
                        clients.lock().await.insert(u.clone(), w);
                        username = Some(u.clone());
                        send_to(&clients, &u, &ServerMessage::LoginOk).await;
                    }
                    None => {
                        // Già loggato in questa stessa connessione: ignora.
                        println!("[{addr}] login ripetuto ignorato");
                    }
                }
            }
            ClientMessage::Register { username: u, password: _ } => {
                // PROBLEMA: nessuna persistenza reale, "registrare" qui non
                // fa altro che loggare la richiesta. Da collegare a uno
                // storage utenti quando esisterà (fuori scope Fase 1).
                println!("[{addr}] register: {u}");
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

/// Invia `msg` a un singolo client cercandolo per username nella mappa
/// condivisa.
///
/// PROBLEMA: il lock resta preso per tutta la durata della `write_all`
/// dentro `send_message` (una `.await` di rete) — vedi il commento su
/// `Clients` in testa al file.
async fn send_to(clients: &Clients, username: &str, msg: &ServerMessage) {
    let mut guard = clients.lock().await;
    if let Some(write_half) = guard.get_mut(username) {
        if let Err(e) = framing::send_message(write_half, msg).await {
            println!("invio a {username} fallito: {e}");
        }
    }
}

/// Inoltra un messaggio di chat a tutti i client connessi tranne il
/// mittente (broadcast).
///
/// PROBLEMA: stesso discorso di `send_to`, amplificato — il lock resta
/// preso mentre si scrive in sequenza a *tutti* i client, quindi un client
/// lento a leggere rallenta l'invio a tutti gli altri. Nello scheletro va
/// bene per validare che il giro client→server→client funzioni; è il primo
/// candidato a essere rifatto col pattern attore (mpsc per client).
async fn broadcast(clients: &Clients, from: &str, text: &str) {
    let mut guard = clients.lock().await;
    for (username, write_half) in guard.iter_mut() {
        if username == from {
            continue;
        }
        let msg = ServerMessage::Broadcast {
            from: from.to_string(),
            text: text.to_string(),
        };
        if let Err(e) = framing::send_message(write_half, &msg).await {
            println!("invio a {username} fallito: {e}");
        }
    }

}
