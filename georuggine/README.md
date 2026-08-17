# georuggine

Applicazione client-server in Rust: gli utenti si collegano al server via TCP, fanno login, condividono la propria posizione geografica e chattano tra loro (broadcast/diretti). Il server tiene traccia dello stato di ogni utente connesso.

## Struttura del workspace

Cargo workspace con tre crate, per non duplicare i tipi del protocollo tra client e server:

```
georuggine/
├── Cargo.toml       # [workspace] — root, elenca i tre membri
├── common/          # tipi condivisi: protocollo messaggi + framing su TCP
├── server/          # binario server (accetta connessioni, un task per client)
└── client/          # binario client (si connette, manda/riceve messaggi)
```

`server` e `client` dipendono entrambi da `common` via path locale: se il protocollo cambia, si cambia in un solo posto.

## Stato attuale — cosa c'è

### `common` — protocollo e framing (✅ implementato)

- **`ClientMessage`** (`Login`, `Register`, `PositionUpdate`, `ChatToServer`) e **`ServerMessage`** (`LoginOk`, `LoginErr`, `Broadcast`, `Direct`, `StateChanged`) — enum serializzabili con `serde`.
- **`UserState`** (`Disconnected`, `Idle`, `Moving`) — stato di un utente.
- **Modulo `framing`**: serializzazione JSON + framing *length-prefixed* su TCP (4 byte di lunghezza big-endian + payload), asincrono (`tokio::io::AsyncRead`/`AsyncWrite`). Include un cap (`MAX_MESSAGE_SIZE`, 1 MiB) contro payload dichiarati enormi.
- 3 test automatici (roundtrip `ClientMessage`/`ServerMessage`, rifiuto messaggi oversize) — `cargo test -p common`.

### `server` (🟡 scheletro funzionante, non production-ready)

- Accetta connessioni su `0.0.0.0:8080`, un task `tokio::spawn` per client.
- Gestisce `Login` (sempre accettato, nessuna verifica password/duplicati), `ChatToServer` (broadcast a tutti tranne il mittente), logga `Register`/`PositionUpdate` senza elaborarli.
- Stato condiviso: `Arc<Mutex<HashMap<String, OwnedWriteHalf>>>` — funziona ma **non è il design finale** (vedi sotto).

### `client` (🟡 scheletro minimo)

- Si connette, manda un `Login` con credenziali hardcoded, stampa la risposta ed esce. Non resta connesso, non manda altro.

### Verificato manualmente

Login end-to-end testato avviando `server` e `client` in due terminali: il client riceve `LoginOk`, il server logga login e disconnessione.

## Come si esegue

```bash
# terminale 1
cargo run -p server

# terminale 2
cargo run -p client
```

Test automatici (solo `common` per ora):
```bash
cargo test -p common
```

## Decisioni già prese

| Decisione | Scelta | Perché |
|---|---|---|
| Formato serializzazione | JSON (`serde_json`) | Leggibile/debuggabile; isolato in `framing`, si può sostituire con `bincode` senza toccare client/server |
| Framing TCP | Length-prefixed (4 byte + payload) | TCP non ha confini di messaggio: serve un modo esplicito per sapere dove finisce un messaggio |
| I/O | Asincrono con `tokio` | Un task per connessione client, non thread OS |

## Cosa manca / cosa cambierà (roadmap)

**Da rifare — priorità alta:**
- **Pattern attore per lo stato condiviso.** Oggi scrivere a un client richiede il lock dell'intera `Mutex<HashMap<...>>` per tutta la durata della scrittura in rete: con più client questo serializza tutte le scritture e un client lento rallenta gli altri (broadcast in particolare). Da sostituire con un `mpsc::Sender<ServerMessage>` per client: ogni client ha il proprio task che possiede la sua metà di scrittura e riceve messaggi da un channel, senza lock condivisi sull'I/O.
- **Client che resta connesso.** Il client attuale fa un solo giro login e chiude. Serve un loop che legga da stdin (o da una UI) e mandi `PositionUpdate`/`ChatToServer`, mentre in parallelo legge messaggi in arrivo dal server (serviranno due task, uno per direzione, o `tokio::select!`).

**Da implementare — logica applicativa:**
- Autenticazione vera: verifica password (con hashing, non testo in chiaro), storage utenti persistente, rifiuto username duplicati/`LoginErr` con motivo.
- `Register` non fa nulla di reale: va collegato allo storage utenti quando esisterà.
- `PositionUpdate` non aggiorna né propaga lo `UserState` — va decisa come gruppo la logica di transizione (es. quando un utente passa da `Idle` a `Moving`) prima di implementarla, e poi mandare `ServerMessage::StateChanged` agli utenti interessati.
- Messaggi diretti (`ServerMessage::Direct`) — il tipo esiste nel protocollo ma nessun percorso lo usa ancora.

**Decisioni ancora aperte, da discutere come gruppo:**
- Sicurezza del trasporto (TLS?) — al momento tutto viaggia in chiaro, incluse le password nel messaggio `Login`.
- Se/quando passare da JSON a un formato binario (`bincode`) per prestazioni.
- Persistenza (file? database?) per utenti e cronologia messaggi.
- Gestione riconnessioni/timeout lato client e server.

## File di configurazione

- **`.gitignore`** (alla radice del repo) esclude `target/`, backup di `rustfmt`, file di editor/OS. `Cargo.lock` è invece **committato di proposito**: il workspace produce binari (`server`, `client`), quindi tutti devono buildare con le stesse versioni esatte delle dipendenze.
