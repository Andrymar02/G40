mod console;
mod cpu_log;
mod demo;
#[macro_use]
mod logging;
mod session;
mod state;
mod users;

use common::framing;
use common::{ClientMessage, ServerMessage};
use console::{ConsoleCommand, parse_console_line};
use server::tracking::{
    GeoPoint, MovementReport, PositionSample, ReportPeriod, SharedTracking, new_tracking,
};
use session::{DeliveryError, LoginRejected, SessionId, SessionRegistry};
use state::{Positions, SharedClock, SystemClock};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};
use users::Users;

pub(crate) use session::Clients;

/// File in cui persistono le credenziali registrate (JSON, vedi `users`).
/// Percorso relativo alla working directory da cui si lancia `cargo run -p
/// server` (di norma la radice del workspace).
const USERS_FILE: &str = "users.json";

/// Indirizzo:porta di bind di default, usato se non se ne passa uno da riga
/// di comando (`cargo run -p server -- <indirizzo:porta>`).
const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";

/// Silenzio massimo tollerato su una connessione prima di considerarla
/// morta e chiuderla.
///
/// PROBLEMA CHIUSO QUI: senza un timeout, `read_message` resta in attesa per
/// sempre su un socket che non riceve né dati né FIN. Succede quando il
/// client sparisce a livello di rete (cavo staccato, wifi caduto, VM
/// sospesa) o resta congelato: il processo non muore, quindi il kernel non
/// manda nulla, e la sessione resta registrata bloccando quell'username a
/// tempo indeterminato. TCP di suo non se ne accorge: `SO_KEEPALIVE` è
/// spento di default e il primo sondaggio arriverebbe comunque dopo due ore.
///
/// Il valore è legato al protocollo, non scelto a caso: la consegna impone
/// al client di mandare la posizione ogni 30 secondi, quindi 90 secondi di
/// silenzio sono tre campioni persi di fila. Un margine più stretto
/// scollegherebbe client sani su una rete lenta.
/// Il valore effettivo viene da `common::timing`, che consente di
/// accorciarlo con una variabile d'ambiente per le prove manuali.
fn client_idle_timeout() -> Duration {
    common::timing::client_idle_timeout()
}

/// Capacità del channel per-client: quanti `ServerMessage` possono restare
/// in coda prima che chi manda (`send_to`/`broadcast`) resti in attesa sul
/// `.send().await`. Un valore arbitrario per questa fase; da rivedere in
/// base al traffico reale.
const CLIENT_CHANNEL_CAPACITY: usize = 32;

#[tokio::main]
async fn main() -> io::Result<()> {
    // Indirizzo:porta e file utenti configurabili da riga di comando, con
    // default se omessi — utile per far girare più server sulla stessa
    // macchina durante i test (ognuno con il proprio file utenti isolato,
    // vedi server/tests/e2e_login.rs), o per un ambiente diverso da quello
    // locale.
    let mut args = std::env::args().skip(1);
    let bind_addr = args.next().unwrap_or_else(|| DEFAULT_BIND_ADDR.to_string());
    let users_file = args.next().unwrap_or_else(|| USERS_FILE.to_string());

    // Uno storage utenti illeggibile ferma l'avvio: vedi `users::UserStore::load`.
    // Si carica prima del bind, per non lasciare aperta una porta su un
    // server che non puo' autenticare nessuno.
    let users: Users = match users::UserStore::load(&users_file) {
        Ok(users) => users,
        Err(reason) => {
            eprintln!("Avvio interrotto: {reason}");
            eprintln!(
                "Controllare {users_file}: rimuoverlo solo se si accetta di perdere le registrazioni."
            );
            return Err(io::Error::new(io::ErrorKind::InvalidData, reason));
        }
    };

    let listener = TcpListener::bind(&bind_addr).await?;
    let clients: Clients = SessionRegistry::new();
    let positions: Positions = state::new_positions();
    let clock: SharedClock = Arc::new(SystemClock);

    let tracking: SharedTracking = new_tracking();

    println!("Server in ascolto su {bind_addr}");
    for avviso in common::timing::server_overrides() {
        println!("{avviso}");
    }
    if let Some(avviso) = logging::startup_notice() {
        println!("{avviso}");
    }
    if let Some(avviso) = demo::startup_notice() {
        println!("{avviso}");
    }

    tokio::spawn(cpu_log::run());

    // Il loop di accept gira in un task a parte: il task principale resta
    // libero per la console dell'operatore (broadcast/diretto), che deve
    // poter leggere da stdin senza aspettare nuove connessioni.
    {
        let clients = clients.clone();
        let users = users.clone();
        let positions = positions.clone();
        let clock = clock.clone();

        let tracking = tracking.clone();

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

                let tracking = tracking.clone();

                tokio::spawn(async move {
                    handle_client(socket, addr, clients, users, positions, clock, tracking).await;
                });
            }
        });
    }

    admin_console(clients, tracking, clock).await;
    Ok(())
}

pub(crate) const STATS_USAGE: &str =
    "Uso: /stats <utente> <day|week|month|custom start_utc end_utc>";

fn parse_stats_args(input: &str) -> Result<(&str, ReportPeriod), &'static str> {
    let mut args = input.split_whitespace();

    let username = args.next();
    let period = args.next();
    let start = args.next();
    let end = args.next();
    let extra = args.next();

    match (username, period, start, end, extra) {
        (Some(username), Some("day"), None, None, None) => Ok((username, ReportPeriod::CurrentDay)),
        (Some(username), Some("week"), None, None, None) => {
            Ok((username, ReportPeriod::CurrentWeek))
        }
        (Some(username), Some("month"), None, None, None) => {
            Ok((username, ReportPeriod::CurrentMonth))
        }
        (Some(username), Some("custom"), Some(start), Some(end), None) => {
            let start_utc = start.parse::<i64>().map_err(|_| STATS_USAGE)?;

            let end_utc = end.parse::<i64>().map_err(|_| STATS_USAGE)?;

            Ok((username, ReportPeriod::Custom { start_utc, end_utc }))
        }
        _ => Err(STATS_USAGE),
    }
}

async fn build_stats_report(
    tracking: &SharedTracking,
    input: &str,
    now_utc: i64,
) -> Result<MovementReport, String> {
    let (username, period) = parse_stats_args(input).map_err(|error| error.to_string())?;

    tracking
        .movement_report(username, period, now_utc)
        .await
        .map_err(|error| format!("{error:?}"))
}

/// Console dell'operatore lato server: legge righe da stdin del processo
/// server (non della rete) e decide se mandarle in broadcast o a un singolo
/// utente. È l'unica sorgente di `ServerMessage::Broadcast`/`Direct`: per
/// consegna, la scelta broadcast-vs-diretto spetta al server, non a un
/// client che indica un destinatario (i messaggi che i client mandano con
/// `ChatToServer` restano infatti solo lato server, vedi `handle_client`).
async fn admin_console(clients: Clients, tracking: SharedTracking, clock: SharedClock) {
    println!("{}", console::CONSOLE_HELP);

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let Some(command) = parse_console_line(&line) else {
            continue;
        };

        match command {
            ConsoleCommand::Broadcast(text) => broadcast(&clients, text).await,

            ConsoleCommand::Direct { to, text } => direct_message(&clients, to, text).await,

            ConsoleCommand::Stats(args) => {
                let now_utc = clock.now_secs();
                match build_stats_report(&tracking, args, now_utc).await {
                    Ok(report) => println!("{}", console::format_report(&report)),
                    Err(error) => println!("statistiche non disponibili: {error}"),
                }
            }

            ConsoleCommand::Users => {
                let connected = clients.connected_usernames().await;
                if connected.is_empty() {
                    println!("nessun utente connesso");
                } else {
                    println!("connessi ({}): {}", connected.len(), connected.join(", "));
                }
            }

            ConsoleCommand::Help => println!("{}", console::CONSOLE_HELP),

            ConsoleCommand::Seed { username, days } => {
                let now_utc = clock.now_secs();
                match demo::seed(&tracking, username, days, now_utc).await {
                    Ok(riepilogo) => println!(
                        "storico NON reale generato per {username}: {} campioni in {} sessioni,\n\
                         dal {} al {}\nProva ora: /stats {username} day | week | month",
                        riepilogo.samples,
                        riepilogo.sessions,
                        console::format_instant(riepilogo.first_utc),
                        console::format_instant(riepilogo.last_utc),
                    ),
                    Err(demo::SeedError::GiaPopolato) => println!(
                        "storico di {username} già generato in questa esecuzione.\n\
                         Rigenerarlo lo sommerebbe al precedente, raddoppiando i numeri:\n\
                         riavvia il server per ripartire da uno storico vuoto,\n\
                         oppure usa un altro utente."
                    ),
                    Err(demo::SeedError::Tracking(error)) => {
                        println!("generazione fallita: {error:?}")
                    }
                }
            }

            ConsoleCommand::Usage(message) => println!("{message}"),
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
            detail!("[{addr}] invio fallito: {e}");
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
    tracking: SharedTracking,
) {
    let (mut read_half, write_half) = socket.into_split();

    let (tx, rx) = mpsc::channel::<ServerMessage>(CLIENT_CHANNEL_CAPACITY);
    tokio::spawn(writer_task(write_half, rx, addr));

    // Id assegnato subito, prima dell'autenticazione: serve comunque in
    // chiusura, perche' una connessione puo' terminare senza aver mai fatto
    // login. Identifica *questa* connessione, cosi' la pulizia finale non
    // puo' rimuovere la sessione di qualcun altro.
    let session_id: SessionId = clients.allocate_session();

    // Username noto solo dopo un Login riuscito; usato anche come guard
    // contro un secondo Login sulla stessa connessione.
    let mut username: Option<String> = None;

    loop {
        let read = timeout(client_idle_timeout(), framing::read_message(&mut read_half));

        let msg: ClientMessage = match read.await {
            Ok(Ok(msg)) => msg,
            // uscita dal loop handle avviene qui quando altro lato tcp si stacca (client)
            Ok(Err(e)) => {
                detail!("[{addr}] connessione chiusa o errore di lettura: {e}");
                break;
            }
            // Nessun byte per il timeout di inattività: il client non c'è più
            // anche se il socket risulta ancora aperto. Si esce dal ciclo e
            // la pulizia in fondo libera l'username.
            Err(_scaduto) => {
                detail!(
                    "[{addr}] nessun messaggio per {} secondi: connessione considerata caduta",
                    client_idle_timeout().as_secs()
                );
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
                    detail!("[{addr}] login ripetuto ignorato");
                } else if !users.verify(&u, &p).await {
                    detail!("[{addr}] login fallito: {u}");
                    let _ = tx
                        .send(ServerMessage::LoginErr(
                            "credenziali non valide".to_string(),
                        ))
                        .await;
                } else {
                    // Credenziali giuste, ma l'username puo' essere gia'
                    // occupato da un'altra connessione: vince chi e' entrato
                    // per primo, questa viene respinta.
                    match clients.login(&u, session_id, tx.clone()).await {
                        Ok(()) => {
                            let connected_at = clock.now_secs();

                            if let Err(error) =
                                tracking.open_session(session_id, &u, connected_at).await
                            {
                                let _ = clients.logout(&u, session_id).await;

                                detail!("[{addr}] apertura tracking fallita per {u}: {error:?}");

                                let _ = tx
                                    .send(ServerMessage::LoginErr(
                                        "errore interno durante il login".to_string(),
                                    ))
                                    .await;

                                continue;
                            }

                            detail!("[{addr}] login: {u} (sessione {session_id})");

                            username = Some(u.clone());

                            let _ = tx.send(ServerMessage::LoginOk).await;
                        }
                        Err(LoginRejected::AlreadyConnected) => {
                            detail!("[{addr}] login rifiutato: {u} è già connesso");
                            let _ = tx
                                .send(ServerMessage::LoginErr(
                                    "utente già connesso da un'altra sessione".to_string(),
                                ))
                                .await;
                        }
                    }
                }
            }
            ClientMessage::Register {
                username: u,
                password: p,
            } => match users.register(&u, &p).await {
                Ok(()) => {
                    detail!("[{addr}] registrazione: {u}");
                    let _ = tx.send(ServerMessage::RegisterOk).await;
                }
                Err(reason) => {
                    detail!("[{addr}] registrazione fallita per {u}: {reason}");
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
                detail!("[{addr}] posizione: ({lat}, {lon}) @ {timestamp}");
                match &username {
                    Some(u) => {
                        let point = match GeoPoint::try_new(lat, lon) {
                            Ok(point) => point,
                            Err(error) => {
                                detail!("[{addr}] posizione ignorata per {u}: {error:?}");
                                continue;
                            }
                        };

                        let accepted =
                            state::update_position(&clients, &positions, &clock, u, lat, lon).await;

                        if accepted {
                            let received_at = clock.now_secs();

                            let sample = PositionSample::new(
                                u.clone(),
                                session_id,
                                point,
                                received_at,
                                Some(timestamp),
                            );

                            if let Err(error) = tracking.record_position(sample).await {
                                detail!(
                                    "[{addr}] salvataggio posizione fallito per {u}: {error:?}"
                                );
                            }
                        }
                    }
                    None => detail!("[{addr}] posizione ignorata: utente non loggato"),
                }
            }
            ClientMessage::ChatToServer { text } => match &username {
                // Solo ricevuto e loggato: la consegna non chiede che un
                // messaggio mandato da un utente venga girato ad altri
                // utenti, quindi qui non c'è nessun broadcast/inoltro.
                Some(u) => detail!("[{addr}] messaggio da {u}: {text}"),
                None => detail!("[{addr}] messaggio ignorato: utente non loggato"),
            },
        }
    }

    if let Some(u) = username {
        // Si rimuove solo se la sessione registrata e' ancora questa: una
        // connessione che termina in ritardo non deve sfrattare quella che
        // nel frattempo ha preso lo stesso username.
        let disconnected_at = clock.now_secs();

        if clients.logout(&u, session_id).await {
            detail!("[{addr}] disconnesso: {u} (sessione {session_id})");

            if let Err(error) = tracking.close_session(session_id, disconnected_at).await {
                detail!("[{addr}] chiusura tracking fallita per {u}: {error:?}");
            }

            state::mark_disconnected(&clients, &positions, &u).await;
        } else {
            detail!("[{addr}] sessione {session_id} chiusa: {u} è già su un'altra sessione");
        }
    }
}

/// Manda un messaggio di testo in broadcast a tutti i client connessi.
/// Chiamata solo dalla console dell'operatore (`admin_console`): è il
/// server stesso il mittente, non un client che chiede di essere rimbalzato
/// ad altri.
///
/// La gestione del lock e la rimozione dei canali chiusi stanno nel registro
/// (`session.rs`), non qui.
async fn broadcast(clients: &Clients, text: &str) {
    clients
        .broadcast(ServerMessage::Broadcast {
            from: "server".to_string(),
            text: text.to_string(),
        })
        .await;
}

/// Manda un messaggio di testo a un solo utente (`to`), se connesso.
/// Come `broadcast`, chiamata solo da `admin_console`: è il server a
/// scegliere il destinatario, non un client. Eventuali errori (utente non
/// connesso, canale chiuso) restano lato server: non c'è nessun client
/// "mittente" a cui rispedirli.
async fn direct_message(clients: &Clients, to: &str, text: &str) {
    let msg = ServerMessage::Direct {
        from: "server".to_string(),
        text: text.to_string(),
    };

    match clients.send_to(to, msg).await {
        Ok(()) => {}
        Err(DeliveryError::NotConnected) => println!("utente '{to}' non connesso"),
        Err(DeliveryError::ChannelClosed) => {
            println!("invio a {to} fallito: canale chiuso, sessione rimossa")
        }
    }
}

#[cfg(test)]
mod stats_command_tests {
    use super::{build_stats_report, parse_stats_args};
    use server::tracking::{ReportPeriod, new_tracking};

    #[test]
    fn stats_command_parses_current_day() {
        let (username, period) = parse_stats_args("alice day").unwrap();

        assert_eq!(username, "alice");
        assert_eq!(period, ReportPeriod::CurrentDay);
    }

    #[test]
    fn stats_command_parses_current_week() {
        let (username, period) = parse_stats_args("alice week").unwrap();

        assert_eq!(username, "alice");
        assert_eq!(period, ReportPeriod::CurrentWeek);
    }

    #[test]
    fn stats_command_parses_current_month() {
        let (username, period) = parse_stats_args("alice month").unwrap();

        assert_eq!(username, "alice");
        assert_eq!(period, ReportPeriod::CurrentMonth);
    }

    #[test]
    fn stats_command_parses_custom_period() {
        let (username, period) = parse_stats_args("alice custom 100 400").unwrap();

        assert_eq!(username, "alice");
        assert_eq!(
            period,
            ReportPeriod::Custom {
                start_utc: 100,
                end_utc: 400,
            }
        );
    }

    #[tokio::test]
    async fn stats_command_builds_empty_custom_report() {
        let tracking = new_tracking();

        let report = build_stats_report(&tracking, "alice custom 100 400", 999)
            .await
            .unwrap();

        assert_eq!(report.username, "alice");
        assert_eq!(report.period_start, 100);
        assert_eq!(report.period_end, 400);
        assert_eq!(report.sample_count, 0);
    }
}
