mod simulator;

use common::framing::{read_message, send_message};
use common::{ClientMessage, ServerMessage};
use simulator::Simulator;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, sleep};

/// Intervallo di invio della posizione, come richiesto dalla consegna
/// ("la posizione di ciascun utente viene trasmessa al server ogni 30
/// secondi").
const POSITION_INTERVAL_SECONDS: u64 = 30;

/// Capacità del channel condiviso tra chi genera `ClientMessage` (loop chat
/// da stdin, task di posizione) e il `writer_task` che li scrive sul socket.
const WRITE_CHANNEL_CAPACITY: usize = 32;

/// Indirizzo:porta del server usato se non se ne passa uno esplicito da
/// riga di comando.
const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:8080";

/// Quante volte riprovare a riconnettersi dopo una perdita di connessione
/// *inattesa* (non un `/quit` esplicito, non un login fallito) prima di
/// arrendersi.
const MAX_RECONNECT_ATTEMPTS: u32 = 5;

/// Attesa prima del primo tentativo di riconnessione; raddoppia ad ogni
/// tentativo successivo (backoff esponenziale), fino a un massimo.
const RECONNECT_BASE_DELAY: Duration = Duration::from_secs(2);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);

fn print_usage() {
    eprintln!("Uso: client <register|login> <username> <password> [indirizzo:porta]");
    eprintln!("  register   registra un nuovo utente e termina subito (nessun login automatico)");
    eprintln!("  login      effettua il login e resta connesso in chat");
    eprintln!("  indirizzo:porta è opzionale, default {DEFAULT_SERVER_ADDR}");
}

#[tokio::main]
async fn main() -> io::Result<()> {
    // Comando/username/password/indirizzo da riga di comando, es.:
    //   cargo run -p client -- register alice pw1
    //   cargo run -p client -- login alice pw1
    //   cargo run -p client -- login bob pw2 127.0.0.1:9090
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_default();
    let (username, password) = match (args.next(), args.next()) {
        (Some(u), Some(p)) => (u, p),
        _ => {
            print_usage();
            return Ok(());
        }
    };
    let server_addr = args
        .next()
        .unwrap_or_else(|| DEFAULT_SERVER_ADDR.to_string());

    match mode.as_str() {
        "register" => run_register(&server_addr, username, password).await,
        "login" => run_login_and_chat(&server_addr, username, password).await,
        _ => {
            print_usage();
            Ok(())
        }
    }
}

/// Flusso di sola registrazione: manda `Register`, stampa l'esito e
/// termina. Non fa nessun login automatico a seguire — per connettersi
/// davvero va rilanciato il client con `login`.
async fn run_register(server_addr: &str, username: String, password: String) -> io::Result<()> {
    let mut stream = TcpStream::connect(server_addr).await?;
    send_message(
        &mut stream,
        &ClientMessage::Register {
            username: username.clone(),
            password,
        },
    )
    .await?;
    match read_message(&mut stream).await? {
        ServerMessage::RegisterOk => {
            println!("Registrazione riuscita per {username}. Ora puoi fare login.")
        }
        ServerMessage::RegisterErr(reason) => eprintln!("Registrazione fallita: {reason}"),
        other => eprintln!("Risposta inattesa alla registrazione: {other:?}"),
    }
    Ok(())
}

/// Come una singola sessione (connessione + login + chat) è terminata.
/// Serve a `run_login_and_chat` per decidere se ha senso riprovare a
/// riconnettersi: solo `ConnectionLost` lo giustifica, gli altri due sono
/// scelte esplicite (dell'utente o del server) che riprovare non cambierebbe.
enum SessionEnd {
    /// L'utente ha digitato `/quit`, o ha chiuso stdin (Ctrl+D).
    UserQuit,
    /// Il server ha rifiutato le credenziali: riprovare non servirebbe,
    /// sono sbagliate indipendentemente da quante volte ci si riconnette.
    LoginFailed(String),
    /// Connessione caduta in modo imprevisto (errore di rete durante la
    /// sessione). Unico caso in cui ha senso riprovare.
    ConnectionLost(String),
}

/// Login con riconnessione: apre sessioni ripetute finché l'utente non
/// esce esplicitamente o finché non si esauriscono i tentativi dopo una
/// perdita di connessione imprevista.
async fn run_login_and_chat(
    server_addr: &str,
    username: String,
    password: String,
) -> io::Result<()> {
    let mut attempt: u32 = 0;

    loop {
        match run_session(server_addr, &username, &password).await {
            SessionEnd::UserQuit => break,
            SessionEnd::LoginFailed(reason) => {
                eprintln!("Login fallito: {reason}");
                break;
            }
            SessionEnd::ConnectionLost(reason) => {
                attempt += 1;
                if attempt > MAX_RECONNECT_ATTEMPTS {
                    eprintln!(
                        "Connessione persa ({reason}). Esauriti i {MAX_RECONNECT_ATTEMPTS} tentativi di riconnessione: esco."
                    );
                    break;
                }
                let delay = std::cmp::min(
                    RECONNECT_BASE_DELAY.saturating_mul(1 << (attempt - 1).min(4)),
                    RECONNECT_MAX_DELAY,
                );
                eprintln!(
                    "Connessione persa ({reason}). Riprovo tra {}s (tentativo {attempt}/{MAX_RECONNECT_ATTEMPTS})...",
                    delay.as_secs()
                );
                sleep(delay).await;
            }
        }
    }

    Ok(())
}

/// Una singola sessione: connette, fa login, e se riesce resta in chat
/// (invio posizione periodico + messaggi da/verso stdin) finché non succede
/// una delle tre cose descritte in [`SessionEnd`]. Prima di ritornare, ferma
/// esplicitamente **tutti** i task che ha avviato (scrittura, lettura,
/// posizione): nessuno resta appeso oltre la vita della sessione.
///
/// PROBLEMA CORRETTO: la prima versione usava `?` su `TcpStream::connect`
/// e sull'invio/ricezione del login, quindi un fallimento *a quel punto*
/// (es. server ancora giù al secondo tentativo di riconnessione) usciva
/// con l'errore fuori da `run_session` **e fuori da `main`**, saltando
/// tutto il resto dei tentativi di riconnessione previsti. Ora ogni errore
/// di rete, qui, diventa esplicitamente `SessionEnd::ConnectionLost`.
async fn run_session(server_addr: &str, username: &str, password: &str) -> SessionEnd {
    let mut stream = match TcpStream::connect(server_addr).await {
        Ok(s) => s,
        Err(e) => return SessionEnd::ConnectionLost(e.to_string()),
    };

    if let Err(e) = send_message(
        &mut stream,
        &ClientMessage::Login {
            username: username.to_string(),
            password: password.to_string(),
        },
    )
    .await
    {
        return SessionEnd::ConnectionLost(e.to_string());
    }
    match read_message(&mut stream).await {
        Ok(ServerMessage::LoginOk) => println!("Login riuscito come {username}."),
        Ok(ServerMessage::LoginErr(reason)) => return SessionEnd::LoginFailed(reason),
        Ok(other) => return SessionEnd::LoginFailed(format!("risposta inattesa: {other:?}")),
        Err(e) => return SessionEnd::ConnectionLost(e.to_string()),
    }

    println!("Connesso. Scrivi un testo e premi invio per mandarlo al server,");
    println!("'/quit' per uscire. Il server può rispondere in broadcast o in diretto.");
    println!(
        "La posizione (simulata) viene inviata automaticamente ogni {POSITION_INTERVAL_SECONDS}s."
    );

    // Split del socket in due metà indipendenti: una per leggere ciò che
    // manda il server, una per scrivere. Un solo task (`writer_task`)
    // possiede la write_half; loop di stdin e task di posizione gli
    // mandano `ClientMessage` da spedire tramite un `mpsc::channel`.
    let (mut read_half, write_half) = stream.into_split();

    let (write_tx, write_rx) = mpsc::channel::<ClientMessage>(WRITE_CHANNEL_CAPACITY);
    let writer_handle = tokio::spawn(writer_task(write_half, write_rx));

    // Segnale "connessione persa": il task di lettura lo manda appena la
    // read fallisce, così il loop di stdin (che altrimenti resterebbe
    // bloccato in attesa di input) se ne accorge subito con `select!`,
    // invece di scoprirlo solo al prossimo invio dell'utente.
    let (disconnect_tx, mut disconnect_rx) = oneshot::channel::<String>();

    let reader_handle = tokio::spawn(async move {
        loop {
            match read_message::<_, ServerMessage>(&mut read_half).await {
                Ok(ServerMessage::Broadcast { from, text }) => println!("[{from}] {text}"),
                Ok(ServerMessage::Direct { from, text }) => println!("[{from} -> te] {text}"),
                Ok(ServerMessage::StateChanged {
                    username,
                    new_state,
                }) => {
                    println!("({username} è ora {new_state:?})");
                }
                // LoginOk/LoginErr/RegisterOk/RegisterErr non dovrebbero più
                // arrivare qui (login già concluso sopra): li stampo comunque
                // invece di ignorarli silenziosamente, così un comportamento
                // inatteso del server si vede.
                Ok(other) => println!("{other:?}"),
                Err(e) => {
                    let _ = disconnect_tx.send(e.to_string());
                    break;
                }
            }
        }
    });

    let position_handle = tokio::spawn(send_position_updates(
        write_tx.clone(),
        username.to_string(),
    ));

    // Loop principale: legge righe da stdin e le manda al server, ma
    // esce anche subito se arriva il segnale di disconnessione dal task
    // di lettura, senza dover aspettare che l'utente scriva qualcosa.
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let end = loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { break SessionEnd::UserQuit };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if line == "/quit" {
                    break SessionEnd::UserQuit;
                }

                let msg = ClientMessage::ChatToServer { text: line.to_string() };
                if write_tx.send(msg).await.is_err() {
                    break SessionEnd::ConnectionLost("canale di scrittura chiuso".to_string());
                }
            }
            reason = &mut disconnect_rx => {
                let reason = reason.unwrap_or_else(|_| "connessione chiusa".to_string());
                break SessionEnd::ConnectionLost(reason);
            }
        }
    };

    // Terminazione pulita: fermo tutti e tre i task avviati da questa
    // sessione, non solo quello di lettura. Se dovessimo riconnetterci,
    // la sessione successiva parte da zero senza nulla di questa appeso.
    reader_handle.abort();
    writer_handle.abort();
    position_handle.abort();

    end
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

/// Manda un `PositionUpdate` ogni [`POSITION_INTERVAL_SECONDS`], usando
/// `simulator::Simulator` per calcolare la prossima posizione: questa
/// funzione si occupa solo della parte di rete (temporizzazione, timestamp,
/// invio sul channel), non di come si generano le coordinate — quella
/// logica è isolata in `simulator.rs` apposta (vedi il modulo per il
/// perché).
async fn send_position_updates(tx: mpsc::Sender<ClientMessage>, username: String) {
    let mut sim = Simulator::for_user(&username);

    loop {
        sleep(Duration::from_secs(POSITION_INTERVAL_SECONDS)).await;

        let (lat, lon) = sim.next_position();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let msg = ClientMessage::PositionUpdate {
            lat,
            lon,
            timestamp,
        };
        if tx.send(msg).await.is_err() {
            break; // writer_task terminato: connessione chiusa.
        }
    }
}
