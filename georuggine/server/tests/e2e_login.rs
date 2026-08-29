use common::framing::{read_message, send_message};
use common::{ClientMessage, ServerMessage};
use std::process::{Child, Command};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::time::{Duration, sleep};

// Guardia (RAII) per chiudere il server alla fine del test
struct ServerGuard(Child);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

/// Percorso di un file utenti temporaneo e univoco per il test `name`.
///
/// PROBLEMA CORRETTO: prima ogni test lanciava il server vero senza passargli
/// un file utenti, quindi tutti e 5 finivano su `users.json` nella working
/// directory di `cargo test` — lo stesso file usato da chiunque avesse
/// lanciato il server a mano in precedenza. Se quel file conteneva gia'
/// "alice"/"bob"/"carol" con password diverse da quelle dei test, la
/// `Register` falliva ("username gia' in uso") e anche la `Login`
/// successiva (password non corrispondente): l'utente non si autenticava
/// mai e il test restava bloccato in attesa di un messaggio che non
/// sarebbe mai arrivato. Stesso principio gia' usato da
/// `users::tests::temp_path` per i test unitari di `UserStore`.
fn temp_users_file(name: &str) -> String {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir()
        .join(format!("georuggine_e2e_{name}_{unique}.json"))
        .to_string_lossy()
        .into_owned()
}

/// Rimuove il file utenti del test (e l'eventuale temporaneo `.tmp` lasciato
/// da una scrittura in corso) a fine test. Best-effort: se il test e' andato
/// in panic prima di arrivare qui, il file resta — stesso compromesso gia'
/// accettato dai test unitari di `UserStore`.
fn cleanup_users_file(path: &str) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{path}.tmp"));
}

/// Avvia il server vero su `addr` con il file utenti `users_file`, aspetta
/// che accetti connessioni (max 15s) e ritorna sia il processo (da tenere
/// vivo con `ServerGuard`) sia la connessione di prova, riusabile come primo
/// client del test.
///
/// Prima questo stesso blocco di ~15 righe era copiato in ognuno dei 5 test:
/// estratto qui per non doverlo aggiornare in 5 posti diversi ad ogni
/// modifica (es. timeout, backoff).
async fn spawn_server(addr: &str, users_file: &str) -> (Child, TcpStream) {
    let server_process = Command::new("cargo")
        .args(["run", "-p", "server", "--", addr, users_file])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("Impossibile avviare il server");

    let mut tentativi = 0;
    let stream = loop {
        match TcpStream::connect(addr).await {
            Ok(stream) => break stream,
            Err(e) => {
                tentativi += 1;
                if tentativi > 30 {
                    panic!("Il server non è partito dopo 15 secondi! Errore: {e}");
                }
                sleep(Duration::from_millis(500)).await;
            }
        }
    };

    (server_process, stream)
}

#[tokio::test]
async fn e2e_doppio_login_rifiutato() {
    let addr = "127.0.0.1:8091";
    let users_file = temp_users_file("doppio_login");
    let (server_process, mut client1) = spawn_server(addr, &users_file).await;
    let _guard = ServerGuard(server_process);

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

    cleanup_users_file(&users_file);
}

#[tokio::test]
async fn e2e_disconnessione_notificata_agli_altri() {
    let addr = "127.0.0.1:8092";
    let users_file = temp_users_file("disconnessione");
    let (server_process, mut client_a) = spawn_server(addr, &users_file).await;
    let _guard = ServerGuard(server_process);

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

    cleanup_users_file(&users_file);
}

#[tokio::test]
async fn e2e_chat_utente_inesistente_non_crasha() {
    use std::io::Write;

    let addr = "127.0.0.1:8093";
    let users_file = temp_users_file("chat_inesistente");
    let (mut server_process, _client) = spawn_server(addr, &users_file).await;

    let mut server_stdin = server_process.stdin.take().unwrap();
    let mut guard = ServerGuard(server_process);

    // Simula l'operatore del server che sbaglia nome utente
    // Scriviamo il comando seguito dall'invio (\n)
    writeln!(server_stdin, "/to utente_fantasma Ciao, ci sei?").unwrap();

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

    cleanup_users_file(&users_file);
}

#[tokio::test]
async fn e2e_aggiornamento_posizione_notifica_stato() {
    let addr = "127.0.0.1:8094";
    let users_file = temp_users_file("posizione_stato");
    let (server_process, mut client_a) = spawn_server(addr, &users_file).await;
    let _guard = ServerGuard(server_process);

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

    cleanup_users_file(&users_file);
}

#[tokio::test]
async fn e2e_messaggio_diretto_valido_arriva_al_destinatario() {
    use std::io::Write;

    // Qui il destinatario esiste ed e' davvero connesso, a differenza del
    // test della chat verso un utente inesistente.
    let addr = "127.0.0.1:8095";
    let users_file = temp_users_file("messaggio_diretto");
    let (mut server_process, mut client) = spawn_server(addr, &users_file).await;

    let mut server_stdin = server_process.stdin.take().unwrap();
    let _guard = ServerGuard(server_process);

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
    writeln!(server_stdin, "/to carol Ciao Carol, ci sei?").unwrap();

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

    cleanup_users_file(&users_file);
}

#[tokio::test]
async fn e2e_reconnect_after_disconnect_opens_new_tracking_session() {
    let addr = "127.0.0.1:8096";
    let users_file = temp_users_file("tracking_reconnect");

    let (server_process, mut first_client) = spawn_server(addr, &users_file).await;

    let _guard = ServerGuard(server_process);

    send_message(
        &mut first_client,
        &ClientMessage::Register {
            username: "alice".to_string(),
            password: "password".to_string(),
        },
    )
    .await
    .unwrap();

    let _ = read_message::<_, ServerMessage>(&mut first_client)
        .await
        .unwrap();

    send_message(
        &mut first_client,
        &ClientMessage::Login {
            username: "alice".to_string(),
            password: "password".to_string(),
        },
    )
    .await
    .unwrap();

    assert!(matches!(
        read_message::<_, ServerMessage>(&mut first_client)
            .await
            .unwrap(),
        ServerMessage::LoginOk
    ));

    drop(first_client);

    sleep(Duration::from_millis(500)).await;

    let mut second_client = TcpStream::connect(addr)
        .await
        .expect("seconda connessione fallita");

    send_message(
        &mut second_client,
        &ClientMessage::Login {
            username: "alice".to_string(),
            password: "password".to_string(),
        },
    )
    .await
    .unwrap();

    let response = tokio::time::timeout(
        Duration::from_secs(3),
        read_message::<_, ServerMessage>(&mut second_client),
    )
    .await
    .expect("il server non ha risposto")
    .unwrap();

    assert!(
        matches!(response, ServerMessage::LoginOk),
        "Alice non può riconnettersi: {response:?}"
    );

    cleanup_users_file(&users_file);
}
