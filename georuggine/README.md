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

- **`ClientMessage`** (`Login`, `Register`, `PositionUpdate`, `ChatToServer`) e **`ServerMessage`** (`LoginOk`, `LoginErr`, `RegisterOk`, `RegisterErr`, `Broadcast`, `Direct`, `StateChanged`) — enum serializzabili con `serde`.
- **`UserState`** (`Disconnected`, `Idle`, `Moving`) — stato di un utente.
- **Modulo `framing`**: serializzazione JSON + framing *length-prefixed* su TCP (4 byte di lunghezza big-endian + payload), asincrono (`tokio::io::AsyncRead`/`AsyncWrite`). Include un cap (`MAX_MESSAGE_SIZE`, 1 MiB) contro payload dichiarati enormi.
- 3 test automatici (roundtrip `ClientMessage`/`ServerMessage`, rifiuto messaggi oversize) — `cargo test -p common`.

### `server` (🟡 scheletro funzionante, non production-ready)

- Accetta connessioni su `0.0.0.0:8080`, un task `tokio::spawn` per client.
- Gestisce `Login`/`Register` con verifica reale contro lo storage utenti (✅, vedi sotto), `ChatToServer` (broadcast a tutti tranne il mittente), logga `PositionUpdate` senza elaborarlo.
- Stato condiviso: `Arc<Mutex<HashMap<String, mpsc::Sender<ServerMessage>>>>` (pattern attore, ✅ implementato). Ogni client ha un `writer_task` dedicato che possiede la propria `OwnedWriteHalf` e riceve i messaggi da spedire da un `mpsc::channel` (capacità 32); `broadcast` tiene il lock della mappa solo per la lookup/clone del `Sender`, mai durante la scrittura in rete.
- **Autenticazione base (✅ implementata, versione minima)**: `server::users::UserStore` (`struct` con `impl`, non funzioni libere: `UserStore::load(path)`, poi `store.register(user, pw)`/`store.verify(user, pw)`), condiviso tra i task come `Arc<UserStore>` (alias `Users`), stesso pattern di `Clients`. Dentro, un `HashMap<String, String>` (`username -> password`, in chiaro, vedi limitazioni sotto) persistito sul file JSON passato a `load` (`users.json`, path in `USERS_FILE`), caricato all'avvio e riscritto ad ogni registrazione riuscita. Username duplicato → `RegisterErr`; credenziali errate al login → `LoginErr`. Separato dalla mappa `Clients`: quella è "chi è connesso ora" (effimera), questa è "chi è registrato" (persistente).
  - **Limitazioni note, accettate per questa prima versione**: password in chiaro sia in memoria che su file (da sostituire con hashing, es. `argon2`, quando si affronterà la sicurezza); il file viene riscritto per intero ad ogni registrazione (va bene per pochi utenti, non scala); nessuna gestione di corruzione/scrittura concorrente sul file oltre al lock in memoria.

### `client` (🟡 scheletro minimo)

- Si connette, manda un `Login` con credenziali hardcoded, stampa la risposta ed esce. Non resta connesso, non manda altro.

### Verificato manualmente

- Login end-to-end (anche dopo il refactor a `mpsc`) testato avviando `server` e `client` in due terminali: il client riceve `LoginOk` tramite il nuovo `writer_task`, il server logga login, position update e disconnessione.
- Autenticazione testata con un piccolo client TCP ad hoc (non nel repo): registrazione nuova → `RegisterOk`; registrazione con username già in uso → `RegisterErr("username già in uso")`; login con password sbagliata → `LoginErr("credenziali non valide")`; login corretto → `LoginOk`; **riavviato il server**, login con le stesse credenziali → ancora `LoginOk` (conferma che `users.json` persiste correttamente).
- **Non ancora verificato**: `broadcast`/messaggi tra più client contemporaneamente. Richiede almeno due connessioni client attive insieme (una che manda `ChatToServer`, una in ascolto) — il client attuale non lo supporta ancora (si veda "Client che resta connesso" nella roadmap).

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
- **Client che resta connesso.** Il client attuale fa un solo giro login e chiude. Serve un loop che legga da stdin (o da una UI) e mandi `PositionUpdate`/`ChatToServer`, mentre in parallelo legge messaggi in arrivo dal server (serviranno due task, uno per direzione, o `tokio::select!`). Finché non c'è, **il broadcast non è verificabile manualmente**: servono almeno due client connessi insieme, uno che manda chat e uno in ascolto — cosa che lo scheletro attuale non supporta (vedi "Verificato manualmente" sotto).

**Miglioria possibile, non bloccante:**
- `broadcast` (in `server`) itera ancora i destinatari in sequenza, non in parallelo: un client con canale pieno rallenta comunque l'invio ai successivi nel giro (solo senza più tenere il lock condiviso, quindi senza bloccare login/disconnessioni). Se servirà vero parallelismo si può passare a un fan-out con `join_all` sulle singole `send`.

**Da implementare — logica applicativa:**
- `PositionUpdate` non aggiorna né propaga lo `UserState` — va decisa come gruppo la logica di transizione (es. quando un utente passa da `Idle` a `Moving`) prima di implementarla, e poi mandare `ServerMessage::StateChanged` agli utenti interessati.
- Messaggi diretti (`ServerMessage::Direct`) — il tipo esiste nel protocollo ma nessun percorso lo usa ancora.

**Decisioni ancora aperte, da discutere come gruppo:**
- Sicurezza del trasporto (TLS?) — al momento tutto viaggia in chiaro, incluse le password nel messaggio `Login`.
- Hashing delle password (`argon2`/`bcrypt`) — oggi sono in chiaro sia in memoria che nel file `users.json`, accettato come scelta temporanea per la prima versione dell'autenticazione.
- Se/quando passare da JSON a un formato binario (`bincode`) per prestazioni.
- Persistenza per la cronologia messaggi (gli utenti ora persistono già su file, vedi sopra) — file o database?
- Gestione riconnessioni/timeout lato client e server.

