# georuggine

Applicazione client-server in Rust per la geolocalizzazione di una flotta di veicoli: gli utenti si registrano, fanno login, condividono la propria posizione (simulata) ogni 30 secondi e scambiano messaggi di testo con il server. Il server tiene traccia dello stato di ogni utente (`Disconnected`/`Idle`/`Moving`), mantiene uno storico delle posizioni con statistiche di movimento, e può rispondere in broadcast o a un singolo utente.

Organizzazione del lavoro di gruppo, timeline e responsabilità: vedi `ORGANIZZAZIONE.md`.

## Struttura del workspace

Cargo workspace con tre crate, per non duplicare i tipi del protocollo tra client e server:
```
georuggine/
├── Cargo.toml       # [workspace] — root, elenca i tre membri
├── common/          # tipi condivisi: protocollo messaggi + framing su TCP
├── server/
│   ├── src/
│   │   ├── main.rs      # ciclo principale, console operatore, connessioni
│   │   ├── session.rs   # registro sessioni (login duplicati, canali chiusi)
│   │   ├── state.rs     # macchina a stati live (Idle/Moving/Disconnected)
│   │   ├── users.rs     # storage utenti (registrazione/login)
│   │   ├── cpu_log.rs   # log periodico del consumo CPU del processo
│   │   └── tracking/    # storico posizioni: modello, calcoli, repository, periodi, servizio
│   └── tests/
│       └── e2e_login.rs # test end-to-end: avviano il server vero, parlano via TCP
└── client/          # binario client (si connette, manda/riceve messaggi, si riconnette da solo)
```

`server` e `client` dipendono entrambi da `common` via path locale: se il protocollo cambia, si cambia in un solo posto.

## Stato attuale — cosa c'è

### `common` — protocollo e framing (✅ implementato)

- **`ClientMessage`**: `Login`, `Register`, `PositionUpdate { lat, lon, timestamp }`, `ChatToServer { text }`.
- **`ServerMessage`**: `LoginOk`, `LoginErr`, `RegisterOk`, `RegisterErr`, `Broadcast { from, text }`, `Direct { from, text }`, `StateChanged { username, new_state }`.
- **`UserState`**: `Disconnected`, `Idle`, `Moving`.
- **Modulo `framing`**: serializzazione JSON + framing *length-prefixed* su TCP (4 byte di lunghezza big-endian + payload), asincrono. Cap `MAX_MESSAGE_SIZE` (1 MiB) contro payload dichiarati enormi.

Comunicazione: solo client→server (`ChatToServer`, ricevuto e basta, mai rimbalzato ad altri utenti) e server→client in broadcast o diretto (decisi dal server, non da un client che indica un destinatario — vedi console operatore sotto).

### `server` (🟡 funzionante, non production-ready)

- Accetta connessioni su un indirizzo configurabile (default `0.0.0.0:8080`, sovrascrivibile: `cargo run -p server -- <indirizzo:porta> [file_utenti]`), un task `tokio::spawn` per client.
- **Autenticazione** (`users.rs`, ✅): `UserStore` con username → password **in chiaro** (scelta deliberata: vedi limitazioni sotto), persistito su file JSON. Username duplicato → `RegisterErr`; credenziali errate → `LoginErr`. Username e password validati (non vuoti, max 64 caratteri, username solo `[A-Za-z0-9_-]`). Un file utenti **corrotto ferma l'avvio** invece di ripartire con un database vuoto. Il salvataggio è **atomico** (scrittura su file temporaneo + `rename`): un crash a metà scrittura non corrompe il file.
- **Registro sessioni** (`session.rs`, ✅): ogni connessione riceve un `SessionId` univoco alla accept. Un secondo login con lo stesso username viene **rifiutato** (vince chi è entrato per primo); logout e pulizia dei canali chiusi rimuovono una voce solo se l'id combacia. Ogni client ha un `writer_task` dedicato che possiede la propria `OwnedWriteHalf`.
- **Macchina a stati live** (`state.rs`, ✅): traccia per utente ultima posizione nota e stato (`Disconnected`/`Idle`/`Moving`).
  - Transizioni: prima posizione mai ricevuta → `Idle`; coordinate cambiate → `Moving`; coordinate ferme da ≥3 minuti → `Idle`; disconnessione → `Disconnected`. Notifica `StateChanged` agli altri client solo quando lo stato cambia davvero.
  - **Orologio del server**, non timestamp del client: le transizioni usano un trait `Clock` (`SystemClock` in produzione, un orologio simulato nei test).
  - **Validazione coordinate**: `lat`/`lon` fuori range (o `NaN`/infinito) vengono scartate, loggate, e non toccano lo stato.
- **Storico posizioni e analisi movimento** (`tracking/`, ✅): ogni posizione valida ricevuta viene registrata in un repository in memoria (retention di default, pruning automatico). Calcolo distanze con formula di Haversine, segmentazione movimento/pausa, velocità media, durata movimento/pause. Query per **giorno/settimana/mese** risolte sul fuso `Europe/Rome` (settimana da lunedì, cambio ora legale gestito correttamente), oltre a un intervallo `custom` arbitrario. Esposto alla console operatore col comando `/stats`.
- **Log CPU** (`cpu_log.rs`, ✅): task periodico (ogni 2 minuti) che scrive il consumo CPU del processo server su `cpu_usage.log`, tramite la crate `sysinfo`.
- **Console operatore** (`admin_console` in `main.rs`, ✅): legge righe da stdin del *processo server*. Testo libero → broadcast a tutti i client connessi; `/msg <utente> <testo>` → diretto a un singolo utente; `/stats <utente> <day|week|month|custom start_utc end_utc>` → statistiche di movimento; `/users` → elenco connessi. È l'unica sorgente di `Broadcast`/`Direct`.
  - **Limite noto**: se lo stdin del processo non è un terminale interattivo, la console legge EOF subito e il server termina.

### `client` (🟡 interattivo, funzionante)

- Due comandi separati: `register` (manda solo `Register`, stampa l'esito, termina) e `login` (manda solo `Login`; se riesce, entra in sessione interattiva).
- Indirizzo:porta del server configurabile come argomento opzionale (default `127.0.0.1:8080`).
- In sessione interattiva: un task legge da stdin e manda `ChatToServer`; un task riceve e stampa `Broadcast`/`Direct`/`StateChanged`; un terzo manda `PositionUpdate` ogni 30 secondi. Scrittura sul socket centralizzata in un `writer_task`.
- **Riconnessione automatica**: se la connessione cade, il client ritenta con backoff esponenziale fino a un numero massimo di tentativi, invece di terminare subito.
- Simulazione posizione: generatore pseudo-casuale (xorshift64 scritto a mano, niente dipendenza `rand`).

### Verificato manualmente

- Login/registrazione end-to-end, con più client connessi insieme.
- Broadcast e messaggio diretto dalla console dell'operatore, ricevuti dai client giusti.
- Transizioni di stato osservate da un secondo client in ascolto, incluso "prima posizione mai mandata".
- Coordinate fuori range scartate lato server, senza toccare lo stato.
- `register`/`login` separati, indirizzo del server personalizzato.

## Come si esegue

```bash
# terminale 1: server (indirizzo:porta e file utenti opzionali)
cargo run -p server
cargo run -p server -- 127.0.0.1:9090

# terminale 2: registra un utente (una tantum)
cargo run -p client -- register alice pw1

# poi login (resta connesso in chat + invio posizione)
cargo run -p client -- login alice pw1
cargo run -p client -- login alice pw1 127.0.0.1:9090   # indirizzo custom
```

In chat: testo libero → mandato al server; `/quit` → esce. Sul terminale del server: testo libero → broadcast; `/msg <utente> <testo>` → diretto; `/stats <utente> day` (o `week`/`month`/`custom <start_utc> <end_utc>`) → statistiche di movimento; `/users` → connessi.

Test automatici (unitari + end-to-end):
```bash
cargo test --workspace
```

Analisi statica e formattazione (nessun warning/diff atteso):
```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

## Continuous Integration

Ogni push o PR verso `dev`/`main` passa da un workflow GitHub Actions (`.github/workflows/ci.yaml`) che verifica, in ordine: formattazione, analisi statica senza warning, test automatici (unitari + end-to-end), build in release. Una PR non è mergiabile finché questi controlli non sono verdi.

## Decisioni già prese

| Decisione | Scelta | Perché |
|---|---|---|
| Formato serializzazione | JSON (`serde_json`) | Leggibile/debuggabile; isolato in `framing`, sostituibile con `bincode` senza toccare client/server |
| Framing TCP | Length-prefixed (4 byte + payload) | TCP non ha confini di messaggio |
| I/O | Asincrono con `tokio` | Un task per connessione client, non thread OS |
| Comunicazione broadcast/diretta | Decisa dal server (console operatore), non da un client che sceglie un destinatario | La consegna descrive solo client→server e server→client |
| Orologio per le transizioni di stato | Orologio del server (`Clock` iniettabile), non il `timestamp` del client | Un client con l'ora sbagliata non deve poter falsare gli stati che gli altri vedono |
| Simulazione posizione | Generatore pseudo-casuale (xorshift a mano) | Nessuna dipendenza `rand` |
| Fuso orario per giorno/settimana/mese | `Europe/Rome`, settimana da lunedì | Scelta esplicita richiesta da `ORGANIZZAZIONE.md`; gestisce anche il cambio ora legale |
| Lettura CPU del processo | Crate `sysinfo` | Multipiattaforma senza codice ad hoc per Windows/Linux |

## Cosa manca / cosa cambierà (roadmap)

Vedi `ORGANIZZAZIONE.md` per il piano completo.

- **Requisiti non funzionali** (Alice): log CPU ✅ fatto; **dimensione degli eseguibili non ancora misurata**; build/esecuzione su una seconda piattaforma verificata di fatto (Windows in locale, Linux via CI) ma non ancora scritta come prova formale; tabella dei risultati, scenario demo e report finale ancora da fare.
- **Lato server** (Francesco): `/stats` e `/msg` implementati; il broadcast usa testo libero invece di un comando `/all` esplicito (equivalente funzionalmente, nome diverso da quello originariamente previsto).

**Decisioni ancora aperte, da discutere come gruppo:**
- Se/quando passare da JSON a un formato binario (`bincode`) per prestazioni.
- Comportamento della console server quando lo stdin non è un terminale interattivo (vedi limite noto sopra).