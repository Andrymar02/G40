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
- **Test Automatici:** La suite di test copre ora il protocollo (framing), la macchina a stati (transizioni, validazione e disconnessione tramite orologio simulato) e l'autenticazione (registrazione, login, credenziali errate e duplicati). Puoi lanciarli con `cargo test --workspace`.

Comunicazione: solo client→server (`ChatToServer`, ricevuto e basta, mai rimbalzato ad altri utenti) e server→client in broadcast o diretto (decisi dal server, non da un client che indica un destinatario — vedi console operatore sotto).

### `server` (🟡 funzionante, non production-ready)

- Accetta connessioni su un indirizzo configurabile (default `0.0.0.0:8080`, sovrascrivibile: `cargo run -p server -- <indirizzo:porta>`), un task `tokio::spawn` per client.
- **Autenticazione** (`users.rs`): `UserStore` con `HashMap<String, String>` (username → password **in chiaro**, scelta deliberata: vedi limitazioni sotto) persistito su `users.json`. Username duplicato → `RegisterErr`; credenziali errate → `LoginErr`. Username e password sono validati (non vuoti, max 64 caratteri, username solo `[A-Za-z0-9_-]`). Un `users.json` **corrotto ferma l'avvio** invece di far ripartire il server con un database vuoto, che alla prima registrazione sovrascriverebbe le credenziali esistenti.
- **Registro sessioni** (`session.rs`, ✅): ogni connessione riceve un `SessionId` univoco alla accept. Un secondo login con lo stesso username viene **rifiutato** (vince chi è entrato per primo); logout e pulizia dei canali chiusi rimuovono una voce solo se l'id combacia, così una connessione che termina in ritardo non può sfrattare la sessione che nel frattempo ha preso quell'username. Ogni client ha un `writer_task` dedicato che possiede la propria `OwnedWriteHalf`; i lock si tengono solo per lookup/clone del `Sender`, mai durante la scrittura in rete.
  - **Timeout di inattività (90 s)**: una connessione che non manda nulla per tre intervalli di posizione consecutivi viene chiusa e il suo username liberato. Serve per il client che sparisce a livello di rete (cavo staccato, wifi caduto, VM sospesa) o resta congelato: il processo non muore, il kernel non manda il FIN, e senza timeout la sessione resterebbe registrata a tempo indeterminato — `SO_KEEPALIVE` è spento di default e il primo sondaggio TCP arriverebbe comunque dopo due ore.
- **Macchina a stati live** (`state.rs`, ✅): traccia per utente ultima posizione nota e stato (`Disconnected`/`Idle`/`Moving`).
  - Transizioni: prima posizione mai ricevuta → `Idle`; coordinate cambiate → `Moving`; coordinate ferme da ≥3 minuti (`IDLE_THRESHOLD_SECONDS`) → `Idle`; disconnessione → `Disconnected`. Notifica `StateChanged` agli altri client solo quando lo stato cambia davvero.
  - **Orologio del server**, non timestamp del client: le transizioni usano un trait `Clock` (`SystemClock` in produzione, un orologio simulato nei test) — un client con l'orologio sbagliato non può falsare le transizioni.
  - **Validazione coordinate**: `lat`/`lon` fuori range (o `NaN`/infinito) vengono scartate, loggate, e non toccano lo stato.
  - 6 test automatici (transizioni, validazione, disconnessione) — `cargo test -p server`, nessuno aspetta 3 minuti reali grazie all'orologio simulato.
- **Console operatore** (`admin_console` in `main.rs`, parsing e formattazione in `console.rs`, ✅): legge righe da stdin del *processo server*. È l'unica sorgente di `Broadcast`/`Direct`.
  - `/all <testo>` → a tutti i client connessi; `/to <utente> <testo>` → a un solo utente; `/stats <utente> <day|week|month|custom start end>` → report di movimento; `/users` → elenco dei connessi; `/help` → elenco dei comandi.
  - **Ogni invio è esplicito**: il testo libero non è più un broadcast implicito. Prima lo era, e un comando scritto male (`/user` invece di `/users`) veniva spedito a tutti i client invece di segnalare l'errore.
  - Il report di `/stats` è formattato per la lettura (distanze in m/km, durate in `1h 05m`, velocità in km/h, istanti nel fuso di Roma) e mostra le quattro grandezze chieste dalla consegna: tragitto percorso, velocità media, durata del movimento e durata delle pause.
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
# terminale 1: server
# argomenti opzionali: <indirizzo:porta> (default 0.0.0.0:8080) e <file utenti> (default users.json)
cargo run -p server
cargo run -p server -- 127.0.0.1:9090
cargo run -p server -- 127.0.0.1:9090 /tmp/prova-utenti.json

# terminale 2: registra un utente (una tantum)
cargo run -p client -- register alice password-alice

# poi login (resta connesso in chat + invio posizione)
cargo run -p client -- login alice password-alice
cargo run -p client -- login alice password-alice 127.0.0.1:9090   # indirizzo custom
```

Vincoli sulle credenziali (vedi `users.rs`): username non vuoto, massimo 64 caratteri, soli
caratteri `[A-Za-z0-9_-]`; password fra 4 e 64 caratteri. Una password di 3 caratteri viene
rifiutata con `RegisterErr`.

**La prima posizione arriva dopo 30 secondi, non subito**: il task che le invia attende un
intervallo prima del primo campione. Fino ad allora il server non ha nulla da cui dedurre uno
stato, quindi non compare nessun `StateChanged` e `/stats` riporta zero campioni.

In chat: testo libero → mandato al server (`ChatToServer`); `/quit` → esce. Sul terminale del server: `/all <testo>` → a tutti; `/to <utente> <testo>` → a un utente; `/stats`, `/users`, `/help` (vedi sopra).

### Output del server

Di default il server stampa **solo** l'intestazione d'avvio, le risposte ai comandi della
console e i propri guasti (avvio interrotto, errore in accept). Registrazioni, login,
disconnessioni, singole posizioni, cambi di stato e messaggi degli utenti non compaiono: se
non digiti nulla, il terminale resta fermo. Prima ogni evento di rete produceva una riga, e
con due o tre client il terminale scorreva da solo.

Per riavere la traccia completa:

```bash
GEORUGGINE_VERBOSE=1 cargo run -p server
```

Accetta `1`, `true`, `yes`, `si`, `on`; `0` e `false` spengono. Quando è attivo lo annuncia
all'avvio. Per osservare lo stato senza il dettaglio si usano `/users` e `/stats`.

### Tempi accorciati per le prove manuali

I valori della consegna (posizione ogni 30 s, ritorno a `Idle` dopo 3 minuti) rendono
impraticabile la verifica a mano: un giro completo richiede quattro minuti. Tre variabili
d'ambiente li accorciano **senza cambiare i default**, definite in `common/src/timing.rs`:

| Variabile | La legge | Default | Accorcia |
|---|---|---|---|
| `GEORUGGINE_POSITION_INTERVAL` | client | 30 s | invio della posizione |
| `GEORUGGINE_IDLE_THRESHOLD` | server | 180 s | ritorno a `Idle` |
| `GEORUGGINE_CLIENT_TIMEOUT` | server | 90 s | timeout di inattività |

Ognuna la legge **un processo solo**: impostarle tutte in un terminale unico non funziona.

```bash
# terminale del server
GEORUGGINE_IDLE_THRESHOLD=6 GEORUGGINE_CLIENT_TIMEOUT=10 cargo run -p server

# terminali dei client
export GEORUGGINE_POSITION_INTERVAL=2
```

Così un giro completo con due utenti, entrambe le transizioni di stato e `/stats` popolato
sta in una ventina di secondi. Ogni override attivo viene **annunciato all'avvio**, perché
una demo con tempi accorciati non deve poter passare inosservata. Valori non numerici o zero
vengono rifiutati con un messaggio e si torna al default.

**Per la demo e per le misure del report: nessuna variabile impostata.**

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
- **Lato server** — proprietà di Francesco: ✅ tutto completato. Gestione sessioni e login simultanei (`session.rs`), persistenza atomica dello storage utenti (`users.rs`), console operatore completa con `/all`, `/to`, `/stats`, `/users`, `/help` (`console.rs`), timeout di inattività sulle connessioni.

**Decisioni già prese come gruppo:**
- **Nessun hashing delle password**: restano in chiaro, in memoria e su `users.json`. La consegna non pone requisiti di sicurezza (chiede solo account e password in fase di registrazione) e chiede invece attenzione a dimensione dell'eseguibile e consumo di CPU, che una funzione di hashing peggiorerebbe. Di conseguenza tutto viaggia in chiaro sulla rete, password del messaggio `Login` incluse: niente TLS.

**Decisioni ancora aperte, da discutere come gruppo:**
- Se/quando passare da JSON a un formato binario (`bincode`) per prestazioni.
- Gestione riconnessioni/timeout lato client (oggi, se la connessione cade, il client stampa un errore e i task terminano: nessun retry automatico).
- Comportamento della console server quando lo stdin non è un terminale interattivo (vedi limite noto sopra).
