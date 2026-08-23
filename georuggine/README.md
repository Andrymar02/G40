# georuggine

Applicazione client-server in Rust per la geolocalizzazione di una flotta di veicoli: gli utenti si registrano, fanno login, condividono la propria posizione (simulata) ogni 30 secondi e scambiano messaggi di testo con il server. Il server tiene traccia dello stato di ogni utente (`Disconnected`/`Idle`/`Moving`) e può rispondere in broadcast o a un singolo utente.

Organizzazione del lavoro di gruppo, timeline e responsabilità: vedi `ORGANIZZAZIONE.md`.

## Struttura del workspace

Cargo workspace con tre crate, per non duplicare i tipi del protocollo tra client e server:

```
georuggine/
├── Cargo.toml       # [workspace] — root, elenca i tre membri
├── common/          # tipi condivisi: protocollo messaggi + framing su TCP
├── server/          # binario server
│   └── src/
│       ├── main.rs   # ciclo principale, console operatore, connessioni
│       ├── state.rs  # macchina a stati live (Idle/Moving/Disconnected)
│       └── users.rs  # storage utenti (registrazione/login)
└── client/          # binario client (si connette, manda/riceve messaggi)
```

`server` e `client` dipendono entrambi da `common` via path locale: se il protocollo cambia, si cambia in un solo posto.

## Stato attuale — cosa c'è

### `common` — protocollo e framing (✅ implementato)

- **`ClientMessage`**: `Login`, `Register`, `PositionUpdate { lat, lon, timestamp }`, `ChatToServer { text }`.
- **`ServerMessage`**: `LoginOk`, `LoginErr`, `RegisterOk`, `RegisterErr`, `Broadcast { from, text }`, `Direct { from, text }`, `StateChanged { username, new_state }`.
- **`UserState`**: `Disconnected`, `Idle`, `Moving`.
- **Modulo `framing`**: serializzazione JSON + framing *length-prefixed* su TCP (4 byte di lunghezza big-endian + payload), asincrono. Cap `MAX_MESSAGE_SIZE` (1 MiB) contro payload dichiarati enormi.
- 3 test automatici — `cargo test -p common`.

Comunicazione: solo client→server (`ChatToServer`, ricevuto e basta, mai rimbalzato ad altri utenti) e server→client in broadcast o diretto (decisi dal server, non da un client che indica un destinatario — vedi console operatore sotto).

### `server` (🟡 funzionante, non production-ready)

- Accetta connessioni su un indirizzo configurabile (default `0.0.0.0:8080`, sovrascrivibile: `cargo run -p server -- <indirizzo:porta>`), un task `tokio::spawn` per client.
- **Autenticazione** (`users.rs`): `UserStore` con `HashMap<String, String>` (username → password **in chiaro**, vedi limitazioni sotto) persistito su `users.json`. Username duplicato → `RegisterErr`; credenziali errate → `LoginErr`.
- **Stato condiviso client connessi**: `Arc<Mutex<HashMap<String, mpsc::Sender<ServerMessage>>>>` (pattern attore). Ogni client ha un `writer_task` dedicato che possiede la propria `OwnedWriteHalf`; i lock sulla mappa si tengono solo per lookup/clone del `Sender`, mai durante la scrittura in rete.
- **Macchina a stati live** (`state.rs`, ✅): traccia per utente ultima posizione nota e stato (`Disconnected`/`Idle`/`Moving`).
  - Transizioni: prima posizione mai ricevuta → `Idle`; coordinate cambiate → `Moving`; coordinate ferme da ≥3 minuti (`IDLE_THRESHOLD_SECONDS`) → `Idle`; disconnessione → `Disconnected`. Notifica `StateChanged` agli altri client solo quando lo stato cambia davvero.
  - **Orologio del server**, non timestamp del client: le transizioni usano un trait `Clock` (`SystemClock` in produzione, un orologio simulato nei test) — un client con l'orologio sbagliato non può falsare le transizioni.
  - **Validazione coordinate**: `lat`/`lon` fuori range (o `NaN`/infinito) vengono scartate, loggate, e non toccano lo stato.
  - 6 test automatici (transizioni, validazione, disconnessione) — `cargo test -p server`, nessuno aspetta 3 minuti reali grazie all'orologio simulato.
- **Console operatore** (`admin_console` in `main.rs`, ✅): legge righe da stdin del *processo server*. Testo libero → broadcast a tutti i client connessi; `/msg <utente> <testo>` → diretto a un singolo utente. È l'unica sorgente di `Broadcast`/`Direct`.
  - **Limite noto**: se lo stdin del processo non è un terminale interattivo (es. lanciato in background senza input), la console legge EOF subito e il server termina. Da rivedere se serve far girare il server senza operatore collegato.

### `client` (🟡 interattivo, funzionante)

- Due comandi separati (niente più registrazione+login automatici in sequenza):
  - `register`: manda solo `Register`, stampa l'esito, termina.
  - `login`: manda solo `Login`; se riesce, entra in sessione interattiva.
- Indirizzo:porta del server configurabile come argomento opzionale (default `127.0.0.1:8080`).
- In sessione interattiva: un task legge da stdin e manda `ChatToServer`; un task separato riceve e stampa `Broadcast`/`Direct`/`StateChanged` dal server; un terzo task manda `PositionUpdate` ogni 30 secondi.
  - Scrittura sul socket centralizzata in un `writer_task` (un solo proprietario della `OwnedWriteHalf`, gli altri task gli mandano `ClientMessage` via `mpsc::channel`) — necessario perché più di un task deve poter scrivere.
  - Simulazione posizione: generatore pseudo-casuale (xorshift64 scritto a mano, niente dipendenza `rand`), punto di partenza fisso + piccoli spostamenti casuali; ad ogni tick, metà delle volte si muove e metà resta fermo, per poter osservare sia `Idle→Moving` sia `Moving→Idle`.

### Verificato manualmente

- Login/registrazione end-to-end, con più client connessi insieme.
- Broadcast e messaggio diretto dalla console dell'operatore, ricevuti dai client giusti; un client che manda `ChatToServer` non viene rimbalzato ad altri.
- Transizioni di stato (`Idle→Moving→Idle`, `Disconnected`) osservate da un secondo client in ascolto, incluso il caso "prima posizione mai mandata" (che prima non generava notifica).
- Coordinate fuori range scartate lato server, senza toccare lo stato.
- `register`/`login` separati: registrare senza fare login, login senza essersi mai registrati (fallisce correttamente), indirizzo del server personalizzato.

## Come si esegue

```bash
# terminale 1: server (indirizzo:porta opzionale, default 0.0.0.0:8080)
cargo run -p server
cargo run -p server -- 127.0.0.1:9090

# terminale 2: registra un utente (una tantum)
cargo run -p client -- register alice pw1

# poi login (resta connesso in chat + invio posizione)
cargo run -p client -- login alice pw1
cargo run -p client -- login alice pw1 127.0.0.1:9090   # indirizzo custom
```

In chat: testo libero → mandato al server (`ChatToServer`); `/quit` → esce. Sul terminale del server: testo libero → broadcast a tutti; `/msg <utente> <testo>` → diretto a un utente.

Test automatici:
```bash
cargo test --workspace
```

Analisi statica e formattazione (nessun warning/diff atteso):
```bash
cargo clippy --workspace --all-targets
cargo fmt --check
```

## Decisioni già prese

| Decisione | Scelta | Perché |
|---|---|---|
| Formato serializzazione | JSON (`serde_json`) | Leggibile/debuggabile; isolato in `framing`, sostituibile con `bincode` senza toccare client/server |
| Framing TCP | Length-prefixed (4 byte + payload) | TCP non ha confini di messaggio |
| I/O | Asincrono con `tokio` | Un task per connessione client, non thread OS |
| Comunicazione broadcast/diretta | Decisa dal server (console operatore), non da un client che sceglie un destinatario | La consegna descrive solo client→server e server→client; non è richiesta chat utente-a-utente |
| Orologio per le transizioni di stato | Orologio del server (`Clock` iniettabile), non il `timestamp` del client | Un client con l'ora sbagliata non deve poter falsare gli stati che gli altri vedono |
| Simulazione posizione | Generatore pseudo-casuale (xorshift a mano) | Nessuna dipendenza `rand`, in linea con l'attenzione alla dimensione dell'eseguibile |

## Cosa manca / cosa cambierà (roadmap)

Vedi `ORGANIZZAZIONE.md` per il piano completo. In sintesi, restano tre aree indipendenti:

- **Storico posizioni e analisi movimento** (tragitto, velocità media, durata movimento/pause, su giorno/settimana/mese) — non iniziato, proprietà di Viorel.
- **Requisiti non funzionali**: log CPU ogni 2 minuti, build/misure su una seconda piattaforma, dimensione eseguibili — non iniziato, proprietà di Alice.
- **Lato server**: sessioni robuste contro doppio login con lo stesso username, hashing password, persistenza atomica dello storage utenti, comandi aggiuntivi alla console (`/all`, `/to`, `/stats`, `/users`) — proprietà di Francesco.

**Decisioni ancora aperte, da discutere come gruppo:**
- Sicurezza del trasporto (TLS?) — al momento tutto viaggia in chiaro, incluse le password nel messaggio `Login`.
- Se/quando passare da JSON a un formato binario (`bincode`) per prestazioni.
- Gestione riconnessioni/timeout lato client (oggi, se la connessione cade, il client stampa un errore e i task terminano: nessun retry automatico).
- Comportamento della console server quando lo stdin non è un terminale interattivo (vedi limite noto sopra).
