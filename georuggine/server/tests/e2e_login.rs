use common::framing::{read_message, send_message};
use common::{ClientMessage, ServerMessage};
use std::process::{Child, Command};
use tokio::net::TcpStream;
use tokio::time::{Duration, sleep};

// Guardia (RAII) per chiudere il server alla fine del test
struct ServerGuard(Child);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

#[tokio::test]
async fn e2e_doppio_login_rifiutato() {
    // Avvia il server in background
    let addr = "127.0.0.1:8091";
    let server_process = Command::new("cargo")
        .args(["run", "-p", "server", "--", addr])
        .spawn()
        .expect("Impossibile avviare il server");

    let _guard = ServerGuard(server_process);

    let mut client1;
    let mut tentativi = 0;

    // Aspetta che il server sia pronto a rispondere (Max 15 secondi)
    loop {
        match TcpStream::connect(addr).await {
            Ok(stream) => {
                client1 = stream;
                break; // Connesso
            }
            Err(e) => {
                tentativi += 1;
                if tentativi > 30 {
                    panic!("Il server non è partito dopo 15 secondi! Errore: {}", e);
                }
                sleep(Duration::from_millis(500)).await;
            }
        }
    }

    // Registra utente (ignoriamo l'esito, se c'è già va bene uguale)
    send_message(
        &mut client1,
        &ClientMessage::Register {
            username: "utente_test_e2e".to_string(),
            password: "password123".to_string(),
        },
    )
    .await
    .unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client1)
        .await
        .unwrap();

    // Login col primo client
    send_message(
        &mut client1,
        &ClientMessage::Login {
            username: "utente_test_e2e".to_string(),
            password: "password123".to_string(),
        },
    )
    .await
    .unwrap();

    match read_message(&mut client1).await.unwrap() {
        ServerMessage::LoginOk => {}
        other => panic!("Login fallito per client 1: {:?}", other),
    }

    // Login il secondo client (Bob)
    let mut client2 = TcpStream::connect(addr)
        .await
        .expect("Client 2 non connesso");

    send_message(
        &mut client2,
        &ClientMessage::Login {
            username: "utente_test_e2e".to_string(), // Stesso username
            password: "password123".to_string(),
        },
    )
    .await
    .unwrap();

    // VERIFICA: Il server deve respingerlo
    match read_message(&mut client2).await.unwrap() {
        ServerMessage::LoginErr(_) => {
            // TEST SUPERATO
        }
        other => panic!("Ci aspettavamo LoginErr, invece: {:?}", other),
    }
}

#[tokio::test]
async fn e2e_disconnessione_notificata_agli_altri() {
    // Avvia il server in background
    let addr = "127.0.0.1:8092";
    let server_process = Command::new("cargo")
        .args(["run", "-p", "server", "--", addr])
        .spawn()
        .expect("Impossibile avviare il server");

    let _guard = ServerGuard(server_process);

    let mut client_a;
    let mut tentativi = 0;

    // Aspetta che il server sia pronto
    loop {
        match TcpStream::connect(addr).await {
            Ok(stream) => {
                client_a = stream;
                break;
            }
            Err(e) => {
                tentativi += 1;
                if tentativi > 30 {
                    panic!("Server non partito: {}", e);
                }
                sleep(Duration::from_millis(500)).await;
            }
        }
    }

    // Client A (Alice) si registra e fa login
    send_message(
        &mut client_a,
        &ClientMessage::Register {
            username: "alice".to_string(),
            password: "password".to_string(),
        },
    )
    .await
    .unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client_a)
        .await
        .unwrap();

    send_message(
        &mut client_a,
        &ClientMessage::Login {
            username: "alice".to_string(),
            password: "password".to_string(),
        },
    )
    .await
    .unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client_a)
        .await
        .unwrap();

    // Client B (Bob) si connette, si registra e fa login
    let mut client_b = TcpStream::connect(addr)
        .await
        .expect("Client B non connesso");

    send_message(
        &mut client_b,
        &ClientMessage::Register {
            username: "bob".to_string(),
            password: "password".to_string(),
        },
    )
    .await
    .unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client_b)
        .await
        .unwrap();

    send_message(
        &mut client_b,
        &ClientMessage::Login {
            username: "bob".to_string(),
            password: "password".to_string(),
        },
    )
    .await
    .unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client_b)
        .await
        .unwrap();

    // Alice manda la prima posizione per entrare ufficialmente nello stato "Idle"
    send_message(
        &mut client_a,
        &ClientMessage::PositionUpdate {
            lat: 45.0,
            lon: 9.0,
            timestamp: 1690000000,
        },
    )
    .await
    .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Alice perde la connessione
    drop(client_a);

    // VERIFICA: Bob deve ricevere un ServerMessage::StateChanged
    // Usiamo un timeout di 3 secondi per evitare che il test si blocchi all'infinito se fallisce
    let timeout_result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let msg = read_message::<_, ServerMessage>(&mut client_b)
                .await
                .unwrap();

            // Controlliamo se è il messaggio che aspettiamo
            if let ServerMessage::StateChanged {
                username,
                new_state,
            } = msg
            {
                // new_state è un enum (Disconnected, Idle, Moving). Lo convertiamo in stringa per comodità
                if username == "alice" && format!("{:?}", new_state).contains("Disconnected") {
                    return true; // Trovato. Esce dal loop e dal timeout
                }
            }
        }
    })
    .await;

    // Se il timeout scade, timeout_result sarà un Err
    assert!(
        timeout_result.is_ok(),
        "Bob non ha mai ricevuto la notifica di disconnessione di Alice!"
    );
}

#[tokio::test]
async fn e2e_chat_utente_inesistente_non_crasha() {
    use std::io::Write;
    use std::process::Stdio;

    // Avvia il server abilitando l'input da terminale (piped)
    let addr = "127.0.0.1:8093";
    let mut server_process = Command::new("cargo")
        .args(["run", "-p", "server", "--", addr])
        .stdin(Stdio::piped())
        .spawn()
        .expect("Impossibile avviare il server");

    let mut server_stdin = server_process.stdin.take().unwrap();
    let mut guard = ServerGuard(server_process);

    // Aspetta che il server sia pronto connettendo un client "esploratore"
    let _client;
    let mut tentativi = 0;
    loop {
        match TcpStream::connect(addr).await {
            Ok(stream) => {
                _client = stream;
                break;
            }
            Err(e) => {
                tentativi += 1;
                if tentativi > 30 {
                    panic!("Server non partito: {}", e);
                }
                sleep(Duration::from_millis(500)).await;
            }
        }
    }

    // Simula l'operatore del server che sbaglia nome utente
    // Scriviamo il comando seguito dall'invio (\n)
    writeln!(server_stdin, "/msg utente_fantasma Ciao, ci sei?").unwrap();

    sleep(Duration::from_millis(500)).await;

    // VERIFICA: Il server è ancora vivo?
    // try_wait() restituisce None se il processo sta girando nel modo corretto,
    // oppure Some(status) se il server è andato in panico ed è morto.
    let status = guard
        .0
        .try_wait()
        .expect("Errore nel controllo del processo");

    assert!(
        status.is_none(),
        "IL SERVER E' CRASHATO! Mandare un messaggio a un utente inesistente lo ha ucciso."
    );
}

#[tokio::test]
async fn e2e_aggiornamento_posizione_notifica_stato() {
    // Avvia il server in background
    let addr = "127.0.0.1:8094";
    let server_process = Command::new("cargo")
        .args(["run", "-p", "server", "--", addr])
        .spawn()
        .expect("Impossibile avviare il server");

    let _guard = ServerGuard(server_process);

    let mut client_a;
    let mut tentativi = 0;

    // Aspetta che il server sia pronto
    loop {
        match TcpStream::connect(addr).await {
            Ok(stream) => {
                client_a = stream;
                break;
            }
            Err(e) => {
                tentativi += 1;
                if tentativi > 30 {
                    panic!("Server non partito: {}", e);
                }
                sleep(Duration::from_millis(500)).await;
            }
        }
    }

    // Client A (alice) si registra e fa login
    send_message(
        &mut client_a,
        &ClientMessage::Register {
            username: "alice".to_string(),
            password: "password".to_string(),
        },
    )
    .await
    .unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client_a)
        .await
        .unwrap();

    send_message(
        &mut client_a,
        &ClientMessage::Login {
            username: "alice".to_string(),
            password: "password".to_string(),
        },
    )
    .await
    .unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client_a)
        .await
        .unwrap();

    // Client B (bob) si connette, si registra e fa login: e' lui che deve
    // ricevere la notifica del cambio di stato di alice.
    let mut client_b = TcpStream::connect(addr)
        .await
        .expect("Client B non connesso");

    send_message(
        &mut client_b,
        &ClientMessage::Register {
            username: "bob".to_string(),
            password: "password".to_string(),
        },
    )
    .await
    .unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client_b)
        .await
        .unwrap();

    send_message(
        &mut client_b,
        &ClientMessage::Login {
            username: "bob".to_string(),
            password: "password".to_string(),
        },
    )
    .await
    .unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client_b)
        .await
        .unwrap();

    // AZIONE: alice manda la prima posizione via TCP reale (non a livello
    // di unit test come in state.rs).
    send_message(
        &mut client_a,
        &ClientMessage::PositionUpdate {
            lat: 45.07,
            lon: 7.68,
            timestamp: 1_700_000_000,
        },
    )
    .await
    .unwrap();

    // VERIFICA: bob deve ricevere lo StateChanged di alice -> Idle (prima
    // posizione mai ricevuta = notificata, vedi state::tests in state.rs).
    let timeout_result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let msg = read_message::<_, ServerMessage>(&mut client_b)
                .await
                .unwrap();

            if let ServerMessage::StateChanged {
                username,
                new_state,
            } = msg
                && username == "alice"
                && format!("{:?}", new_state).contains("Idle")
            {
                return true;
            }
        }
    })
    .await;

    assert!(
        timeout_result.is_ok(),
        "Bob non ha mai ricevuto la notifica del cambio di stato di Alice!"
    );
}

#[tokio::test]
async fn e2e_messaggio_diretto_valido_arriva_al_destinatario() {
    use std::io::Write;
    use std::process::Stdio;

    // Avvia il server abilitando l'input da terminale (piped), come nel
    // test della chat verso un utente inesistente: qui pero' il
    // destinatario esiste ed e' davvero connesso.
    let addr = "127.0.0.1:8095";
    let mut server_process = Command::new("cargo")
        .args(["run", "-p", "server", "--", addr])
        .stdin(Stdio::piped())
        .spawn()
        .expect("Impossibile avviare il server");

    let mut server_stdin = server_process.stdin.take().unwrap();
    let _guard = ServerGuard(server_process);

    let mut client;
    let mut tentativi = 0;
    loop {
        match TcpStream::connect(addr).await {
            Ok(stream) => {
                client = stream;
                break;
            }
            Err(e) => {
                tentativi += 1;
                if tentativi > 30 {
                    panic!("Server non partito: {}", e);
                }
                sleep(Duration::from_millis(500)).await;
            }
        }
    }

    // Il destinatario si registra e fa login, cosi' risulta connesso.
    send_message(
        &mut client,
        &ClientMessage::Register {
            username: "carol".to_string(),
            password: "password".to_string(),
        },
    )
    .await
    .unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client).await.unwrap();

    send_message(
        &mut client,
        &ClientMessage::Login {
            username: "carol".to_string(),
            password: "password".to_string(),
        },
    )
    .await
    .unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client).await.unwrap();

    // AZIONE: l'operatore del server manda un messaggio diretto a carol,
    // che e' davvero connessa.
    writeln!(server_stdin, "/msg carol Ciao Carol, ci sei?").unwrap();

    // VERIFICA: carol deve ricevere esattamente quel messaggio, come Direct.
    let timeout_result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let msg = read_message::<_, ServerMessage>(&mut client).await.unwrap();

            if let ServerMessage::Direct { from, text } = msg
                && from == "server"
                && text == "Ciao Carol, ci sei?"
            {
                return true;
            }
        }
    })
    .await;

    assert!(
        timeout_result.is_ok(),
        "Carol non ha mai ricevuto il messaggio diretto!"
    );
}
