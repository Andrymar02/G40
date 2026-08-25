use common::framing::{read_message, send_message};
use common::{ClientMessage, ServerMessage};
use std::process::{Child, Command};
use tokio::net::TcpStream;
use tokio::time::{sleep, Duration};

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
    let server_process = Command::new("cargo")
        .args(["run", "-p", "server"])
        .spawn()
        .expect("Impossibile avviare il server");
    
    let _guard = ServerGuard(server_process);
    let addr = "127.0.0.1:8080";
    
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
    send_message(&mut client1, &ClientMessage::Register {
        username: "utente_test_e2e".to_string(),
        password: "password123".to_string(),
    }).await.unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client1).await.unwrap();

    // Login col primo client
    send_message(&mut client1, &ClientMessage::Login {
        username: "utente_test_e2e".to_string(),
        password: "password123".to_string(),
    }).await.unwrap();
    
    match read_message(&mut client1).await.unwrap() {
        ServerMessage::LoginOk => {} 
        other => panic!("Login fallito per client 1: {:?}", other),
    }

    // Login il secondo client (Bob)
    let mut client2 = TcpStream::connect(addr).await.expect("Client 2 non connesso");
    
    send_message(&mut client2, &ClientMessage::Login {
        username: "utente_test_e2e".to_string(), // Stesso username
        password: "password123".to_string(),
    }).await.unwrap();

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
    let server_process = Command::new("cargo")
        .args(["run", "-p", "server"])
        .spawn()
        .expect("Impossibile avviare il server");
    
    let _guard = ServerGuard(server_process);
    let addr = "127.0.0.1:8080";
    
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
                if tentativi > 30 { panic!("Server non partito: {}", e); }
                sleep(Duration::from_millis(500)).await;
            }
        }
    }

    // Client A (Alice) si registra e fa login
    send_message(&mut client_a, &ClientMessage::Register {
        username: "alice".to_string(), password: "password".to_string(),
    }).await.unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client_a).await.unwrap();

    send_message(&mut client_a, &ClientMessage::Login {
        username: "alice".to_string(), password: "password".to_string(),
    }).await.unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client_a).await.unwrap();

    // Client B (Bob) si connette, si registra e fa login
    let mut client_b = TcpStream::connect(addr).await.expect("Client B non connesso");
    
    send_message(&mut client_b, &ClientMessage::Register {
        username: "bob".to_string(), password: "password".to_string(),
    }).await.unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client_b).await.unwrap();

    send_message(&mut client_b, &ClientMessage::Login {
        username: "bob".to_string(), password: "password".to_string(),
    }).await.unwrap();
    let _ = read_message::<_, ServerMessage>(&mut client_b).await.unwrap();

    // Alice manda la prima posizione per entrare ufficialmente nello stato "Idle"
    send_message(&mut client_a, &ClientMessage::PositionUpdate {
        lat: 45.0,
        lon: 9.0,
        timestamp: 1690000000,
    }).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Alice perde la connessione
    drop(client_a);

    // VERIFICA: Bob deve ricevere un ServerMessage::StateChanged
    // Usiamo un timeout di 3 secondi per evitare che il test si blocchi all'infinito se fallisce
    let timeout_result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let msg = read_message::<_, ServerMessage>(&mut client_b).await.unwrap();
            
            // Controlliamo se è il messaggio che aspettiamo
            if let ServerMessage::StateChanged { username, new_state } = msg {
                // new_state è un enum (Disconnected, Idle, Moving). Lo convertiamo in stringa per comodità
                if username == "alice" && format!("{:?}", new_state).contains("Disconnected") {
                    return true; // Trovato. Esce dal loop e dal timeout
                }
            }
        }
    }).await;

    // Se il timeout scade, timeout_result sarà un Err
    assert!(timeout_result.is_ok(), "Bob non ha mai ricevuto la notifica di disconnessione di Alice!");
}