use common::framing::{read_message, send_message};
use common::{ClientMessage, ServerMessage};
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

/// Intervallo di invio della posizione, come richiesto dalla consegna
/// ("la posizione di ciascun utente viene trasmessa al server ogni 30
/// secondi").
const POSITION_INTERVAL_SECONDS: u64 = 30;

/// Capacità del channel condiviso tra chi genera `ClientMessage` (loop chat
/// da stdin, task di posizione) e il `writer_task` che li scrive sul socket.
const WRITE_CHANNEL_CAPACITY: usize = 32;

#[tokio::main]
async fn main() -> io::Result<()> {
    // Username/password da riga di comando, per poter avviare più istanze
    // del client con identità diverse e testare broadcast/messaggi diretti
    // tra loro, es.:
    //   cargo run -p client -- alice pw1
    //   cargo run -p client -- bob pw2
    let mut args = std::env::args().skip(1);
    let username = args.next().unwrap_or_else(|| "test1".to_string());
    let password = args.next().unwrap_or_else(|| "pw".to_string());

    let mut stream = TcpStream::connect("127.0.0.1:8080").await?;

    // Tentativo di registrazione: se l'utente esiste già il server risponde
    // RegisterErr, che qui viene ignorato di proposito (si prosegue comunque
    // con il login, così lo stesso comando funziona sia al primo avvio sia
    // ai successivi).
    send_message(
        &mut stream,
        &ClientMessage::Register {
            username: username.clone(),
            password: password.clone(),
        },
    )
    .await?;
    let _: ServerMessage = read_message(&mut stream).await?;

    send_message(
        &mut stream,
        &ClientMessage::Login {
            username: username.clone(),
            password: password.clone(),
        },
    )
    .await?;
    match read_message(&mut stream).await? {
        ServerMessage::LoginOk => println!("Login riuscito come {username}."),
        ServerMessage::LoginErr(reason) => {
            eprintln!("Login fallito: {reason}");
            return Ok(());
        }
        other => {
            eprintln!("Risposta inattesa al login: {other:?}");
            return Ok(());
        }
    }

    println!("Connesso. Scrivi un testo e premi invio per il broadcast,");
    println!("'/msg <utente> <testo>' per un messaggio diretto, '/quit' per uscire.");
    println!("La posizione (simulata) viene inviata automaticamente ogni {POSITION_INTERVAL_SECONDS}s.");

    // Split del socket in due metà indipendenti (stesso pattern usato in
    // server::handle_client/writer_task): una per leggere ciò che manda il
    // server, una per scrivere. La scrittura però non può più essere
    // "posseduta" solo dal loop di stdin come prima: ora anche il task di
    // invio posizione deve poter scrivere sullo stesso socket, e
    // `OwnedWriteHalf` non è cloneable. Si usa quindi lo stesso trucco del
    // server: un solo task possiede la write_half, tutti gli altri gli
    // mandano `ClientMessage` da spedire tramite un `mpsc::channel`.
    let (mut read_half, write_half) = stream.into_split();

    let (write_tx, write_rx) = mpsc::channel::<ClientMessage>(WRITE_CHANNEL_CAPACITY);
    tokio::spawn(writer_task(write_half, write_rx));

    let reader_task = tokio::spawn(async move {
        loop {
            match read_message::<_, ServerMessage>(&mut read_half).await {
                Ok(ServerMessage::Broadcast { from, text }) => println!("[{from}] {text}"),
                Ok(ServerMessage::Direct { from, text }) => println!("[{from} -> te] {text}"),
                Ok(ServerMessage::DirectErr(reason)) => println!("(messaggio diretto non consegnato: {reason})"),
                Ok(ServerMessage::StateChanged { username, new_state }) => {
                    println!("({username} è ora {new_state:?})");
                }
                // LoginOk/LoginErr/RegisterOk/RegisterErr non dovrebbero più
                // arrivare qui (login già concluso sopra): li stampo comunque
                // invece di ignorarli silenziosamente, così un comportamento
                // inatteso del server si vede.
                Ok(other) => println!("{other:?}"),
                Err(e) => {
                    println!("Connessione al server persa: {e}");
                    break;
                }
            }
        }
    });

    tokio::spawn(send_position_updates(write_tx.clone(), username.clone()));

    // Loop principale: legge righe da stdin e le manda al server finché
    // l'utente non digita /quit o stdin si chiude (Ctrl+D).
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "/quit" {
            break;
        }

        let msg = if let Some(rest) = line.strip_prefix("/msg ") {
            match rest.split_once(' ') {
                Some((to, text)) if !text.is_empty() => ClientMessage::ChatDirect {
                    to: to.to_string(),
                    text: text.to_string(),
                },
                _ => {
                    println!("Uso: /msg <utente> <testo>");
                    continue;
                }
            }
        } else {
            ClientMessage::ChatToServer { text: line.to_string() }
        };

        if write_tx.send(msg).await.is_err() {
            println!("Invio fallito: connessione chiusa.");
            break;
        }
    }

    // Il task di lettura non termina da solo quando chiudo io la
    // connessione in scrittura (finché il server non se ne accorge e chiude
    // a sua volta): lo fermo esplicitamente prima di uscire da main.
    reader_task.abort();
    Ok(())
}

/// Unico task che scrive sul socket: riceve `ClientMessage` dal channel
/// condiviso (loop chat, task posizione) e li spedisce in ordine. Stesso
/// principio del `writer_task` lato server: un solo proprietario della metà
/// di scrittura, nessun lock da contendere per mandare un messaggio.
async fn writer_task(mut write_half: OwnedWriteHalf, mut rx: mpsc::Receiver<ClientMessage>) {
    while let Some(msg) = rx.recv().await {
        if let Err(e) = send_message(&mut write_half, &msg).await {
            println!("Invio fallito: {e}");
            break;
        }
    }
}

/// Simula il movimento del veicolo e manda un `PositionUpdate` ogni
/// [`POSITION_INTERVAL_SECONDS`]. Strategia scelta tra quelle proposte dalla
/// consegna: generatore pseudo-casuale di posizioni (le alternative — file
/// di coordinate, punto di partenza/arrivo, inserimento manuale — restano
/// possibili da aggiungere in seguito senza toccare il resto del client, dato
/// che qui fuori si vede solo un `ClientMessage::PositionUpdate` che arriva
/// sul channel).
///
/// Ad ogni tick, con probabilità 1/2 la posizione si sposta di un piccolo
/// offset casuale (simula "in movimento"), altrimenti resta identica (simula
/// "fermo"): serve per poter osservare in pratica sia la transizione
/// Idle->Moving (cambio coordinate) sia quella Moving->Idle (3 minuti di
/// coordinate ferme) gestite lato server.
async fn send_position_updates(tx: mpsc::Sender<ClientMessage>, username: String) {
    // Punto di partenza arbitrario (Torino, sede del PoliTO): serve solo da
    // base plausibile per gli spostamenti simulati.
    let mut lat = 45.0628;
    let mut lon = 7.6620;
    let mut rng = SimpleRng::new(seed_from(&username));

    loop {
        sleep(Duration::from_secs(POSITION_INTERVAL_SECONDS)).await;

        if rng.next_u64() % 2 == 0 {
            lat += rng.next_offset() * 0.001; // ~100 m per passo, a 45° di latitudine
            lon += rng.next_offset() * 0.001;
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let msg = ClientMessage::PositionUpdate { lat, lon, timestamp };
        if tx.send(msg).await.is_err() {
            break; // writer_task terminato: connessione chiusa.
        }
    }
}

/// Seed diverso per ogni client (dipende da username + istante di avvio),
/// così più istanze non generano la stessa identica sequenza di posizioni.
fn seed_from(username: &str) -> u64 {
    // FNV-1a: hash semplice, sufficiente qui (non serve crittografico).
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in username.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    hash ^ now
}

/// Generatore pseudo-casuale minimale (xorshift64). Evita di aggiungere la
/// dipendenza `rand` solo per simulare il movimento — coerente con
/// l'attenzione alla dimensione dell'eseguibile richiesta dalla consegna.
struct SimpleRng(u64);

impl SimpleRng {
    fn new(seed: u64) -> Self {
        // Seed 0 renderebbe xorshift degenere (resta sempre 0).
        Self(if seed == 0 { 0xdead_beef } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Float pseudo-casuale in [-1.0, 1.0).
    fn next_offset(&mut self) -> f64 {
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64; // [0,1)
        unit * 2.0 - 1.0
    }
}
