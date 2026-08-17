//! Protocollo applicativo condiviso tra client e server.
//!
//! DECISIONI DA PRENDERE COME GRUPPO (non da soli):
//!
//! 1. Formato di serializzazione: JSON (`serde_json`) vs binario (`bincode`).
//!    Qui si parte con JSON perché è leggibile e comodo da debuggare (si può
//!    ispezionare il payload a occhio, con `nc`/Wireshark, senza decoder).
//!    Se in futuro serve più velocità/compattezza si può passare a `bincode`
//!    cambiando solo le funzioni in [`framing`]: il resto del codice
//!    (client, server) non deve conoscere il formato scelto.
//!
//! 2. Framing su TCP: come si delimita un messaggio nello stream di byte.
//!    Qui si usa length-prefixed (4 byte di lunghezza + payload), vedi
//!    [`framing`] per i dettagli e il perché.

use serde::{Deserialize, Serialize};

/// Messaggi che il client può inviare al server.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ClientMessage {
    /// Richiesta di login con credenziali in chiaro sul livello applicativo
    /// (la sicurezza del trasporto, es. TLS, è una decisione da prendere a
    /// parte come gruppo: qui non è ancora gestita).
    Login { username: String, password: String },
    /// Richiesta di registrazione di un nuovo utente.
    Register { username: String, password: String },
    /// Aggiornamento periodico della posizione geografica del client.
    PositionUpdate { lat: f64, lon: f64, timestamp: i64 },
    /// Messaggio di chat da inoltrare (il server decide a chi: broadcast o diretto).
    ChatToServer { text: String },
}

/// Messaggi che il server può inviare al client.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ServerMessage {
    /// Login riuscito.
    LoginOk,
    /// Login fallito, con motivo leggibile da mostrare all'utente.
    LoginErr(String),
    /// Messaggio di chat inviato a tutti gli utenti connessi.
    Broadcast { from: String, text: String },
    /// Messaggio di chat privato a un singolo utente.
    Direct { from: String, text: String },
    /// Notifica che lo stato di un utente è cambiato (es. per aggiornare la UI degli altri client).
    StateChanged { username: String, new_state: UserState },
}

/// Stato di un utente lato server, replicato ai client interessati.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum UserState {
    Disconnected,
    Idle,
    Moving,
}

/// Serializzazione dei messaggi e framing su stream TCP.
///
/// TCP è uno stream di byte senza confini di messaggio: senza un modo per
/// sapere dove finisce un messaggio e dove inizia il successivo, chi legge
/// non può distinguere "due messaggi piccoli arrivati insieme" da "un
/// messaggio grande arrivato a metà". Il framing length-prefixed risolve
/// il problema: si leggono sempre 4 byte (la lunghezza), poi esattamente
/// quel numero di byte di payload.
///
/// DECISIONE DI GRUPPO presa: I/O asincrono con `tokio`, non più
/// `std::net::TcpStream` sincrono. Di conseguenza server e client dovranno
/// usare `tokio::net::TcpStream` e girare dentro un runtime tokio
/// (`#[tokio::main]`).
pub mod framing {
    use serde::{de::DeserializeOwned, Serialize};
    use std::io;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

    /// Limite massimo per il payload di un singolo messaggio.
    ///
    /// PROBLEMA: senza questo limite, `read_message` alloca `len` byte
    /// (vedi sotto) fidandosi ciecamente del prefisso di lunghezza letto
    /// dalla rete. Un peer che manda `len = u32::MAX` farebbe allocare
    /// ~4 GB prima ancora di validare il contenuto: è un vettore di DoS
    /// banale. Il valore scelto qui (1 MiB) è arbitrario per la versione
    /// base — da rivedere come gruppo in base ai messaggi reali del
    /// protocollo (es. se si spediscono allegati/mappe andrà alzato).
    const MAX_MESSAGE_SIZE: u32 = 1024 * 1024;

    /// Serializza `msg` in JSON e lo antepone con un prefisso di 4 byte
    /// big-endian che ne indica la lunghezza in byte.
    ///
    /// PROBLEMA: `payload.len() as u32` tronca silenziosamente se il
    /// payload superasse i 4 GB. Con `MAX_MESSAGE_SIZE` così basso non è
    /// un rischio concreto ora, ma se in futuro il limite viene rimosso o
    /// alzato molto, questo cast va rivisto (es. con un controllo esplicito
    /// prima del cast).
    fn encode<T: Serialize>(msg: &T) -> io::Result<Vec<u8>> {
        let payload = serde_json::to_vec(msg)?;
        let len = payload.len() as u32;

        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&len.to_be_bytes());
        framed.extend_from_slice(&payload);
        Ok(framed)
    }

    /// Serializza `msg` e lo scrive per intero su `stream`, già con il
    /// prefisso di lunghezza.
    ///
    /// Generica su `AsyncWrite` (non `tokio::net::TcpStream` direttamente)
    /// così nei test si può usare `tokio::io::duplex(...)` invece di aprire
    /// un socket TCP vero — vedi i test in fondo al file.
    pub async fn send_message<S, T>(stream: &mut S, msg: &T) -> io::Result<()>
    where
        S: AsyncWrite + Unpin,
        T: Serialize,
    {
        let framed = encode(msg)?;
        stream.write_all(&framed).await
    }

    /// Legge da `stream` un messaggio length-prefixed: prima i 4 byte di
    /// lunghezza, poi esattamente quel numero di byte di payload, infine
    /// deserializza il payload nel tipo `T` richiesto (`ClientMessage` o
    /// `ServerMessage` a seconda di chi chiama).
    pub async fn read_message<S, T>(stream: &mut S) -> io::Result<T>
    where
        S: AsyncRead + Unpin,
        T: DeserializeOwned,
    {
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf);

        // PROBLEMA (mitigato qui): senza questo controllo, la riga sotto
        // (`vec![0u8; len]`) alloca la lunghezza dichiarata dal mittente
        // senza validarla. Vedi il commento su `MAX_MESSAGE_SIZE`.
        if len > MAX_MESSAGE_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("messaggio troppo grande: {len} byte (max {MAX_MESSAGE_SIZE})"),
            ));
        }

        let mut payload = vec![0u8; len as usize];
        stream.read_exact(&mut payload).await?;

        Ok(serde_json::from_slice(&payload)?)

    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn client_message_roundtrip() {
        let (mut tx, mut rx) = tokio::io::duplex(1024);

        let msg = ClientMessage::PositionUpdate {
            lat: 45.07,
            lon: 7.68,
            timestamp: 1_755_000_000,
        };

        framing::send_message(&mut tx, &msg).await.unwrap();
        let decoded: ClientMessage = framing::read_message(&mut rx).await.unwrap();

        match decoded {
            ClientMessage::PositionUpdate { lat, lon, timestamp } => {
                assert_eq!(lat, 45.07);
                assert_eq!(lon, 7.68);
                assert_eq!(timestamp, 1_755_000_000);
            }
            _ => panic!("variante inattesa"),
        }
    }

    #[tokio::test]
    async fn server_message_roundtrip() {
        let (mut tx, mut rx) = tokio::io::duplex(1024);

        let msg = ServerMessage::StateChanged {
            username: "alice".to_string(),
            new_state: UserState::Moving,
        };

        framing::send_message(&mut tx, &msg).await.unwrap();
        let decoded: ServerMessage = framing::read_message(&mut rx).await.unwrap();

        match decoded {
            ServerMessage::StateChanged { username, new_state } => {
                assert_eq!(username, "alice");
                assert_eq!(new_state, UserState::Moving);
            }
            _ => panic!("variante inattesa"),
        }
    }

    #[tokio::test]
    async fn read_message_rejects_oversized_length_prefix() {
        // Verifica il cap di MAX_MESSAGE_SIZE: un prefisso di lunghezza
        // troppo grande deve essere rifiutato prima di provare ad
        // allocare/leggere il payload.
        let (mut tx, mut rx) = tokio::io::duplex(16);

        tokio::io::AsyncWriteExt::write_all(&mut tx, &u32::MAX.to_be_bytes())
            .await
            .unwrap();

        let result: std::io::Result<ClientMessage> = framing::read_message(&mut rx).await;
        assert!(result.is_err());
    }
}
