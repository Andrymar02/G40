# Georuggine — Manuale utente

Georuggine è un sistema di geolocalizzazione per una flotta di veicoli. Ci sono
**due figure**, con due programmi distinti:

- il **camionista**, che usa il **client**: si registra, accede, la sua posizione
  viene inviata al server e può scambiare messaggi di testo con l'azienda;
- l'**amministratore dell'azienda**, che usa la **console del server**: controlla i
  veicoli, interroga lo storico dei movimenti e invia messaggi ai camionisti.

Questo manuale spiega come si usano entrambi. Per come è fatto il sistema sotto il
cofano si veda `MANUALE_PROGETTISTA.md`; per una sequenza di prova completa,
`DEMO.md`.

---

## 1. Installazione

### 1.1 Prerequisiti

- **Sistema operativo**: Windows, Linux o macOS. Il progetto è stato verificato su
  **Windows 11** e **Linux x86-64**.
- **Toolchain Rust** recente (edition 2024): si installa con `rustup` da
  <https://rustup.rs>. Fornisce `cargo`, usato per compilare ed eseguire.
- **Nessun'altra dipendenza a runtime**: gli eseguibili prodotti sono
  autosufficienti (non serve un database, un runtime esterno o librerie di
  sistema aggiuntive).

### 1.2 Compilazione

Dalla cartella `georuggine/` del repository:

```bash
cargo build --workspace --release
```

Gli eseguibili ottimizzati vengono creati in `georuggine/target/release/`:

| File | Programma |
|---|---|
| `server` / `server.exe` | console dell'amministratore |
| `client` / `client.exe` | applicazione del camionista |

Dimensioni indicative: `server` ~1,4–2,3 MB, `client` ~0,9–1,4 MB a seconda della
piattaforma.

### 1.3 Esecuzione

Si può lanciare l'eseguibile direttamente (`./target/release/server`,
`./target/release/client ...`) oppure tramite cargo, che compila se serve:

```bash
cargo run -p server --release -- <argomenti del server>
cargo run -p client --release -- <argomenti del client>
```

Nel resto del manuale si usa la forma `cargo run`.

---

## 2. Guida per il camionista (client)

Il client si usa da riga di comando:

```
client <register|login> <username> <password> [indirizzo:porta]
```

`indirizzo:porta` è opzionale: senza, si usa `127.0.0.1:8080`.

### 2.1 Registrazione (una sola volta)

```bash
cargo run -p client --release -- register mrossi camion-2024
```

- Esito positivo: `Registrazione riuscita per mrossi. Ora puoi fare login.` Il
  programma termina: la registrazione **non** effettua l'accesso automatico.
- Esito negativo: `Registrazione fallita: <motivo>` (per esempio username già in
  uso, oppure credenziali non valide).

**Vincoli su username e password**

| Campo | Regola |
|---|---|
| username | non vuoto, massimo 64 caratteri, solo lettere, cifre, `_` e `-` |
| password | da 4 a 64 caratteri |

### 2.2 Accesso

```bash
cargo run -p client --release -- login mrossi camion-2024
# con un server non locale:
cargo run -p client --release -- login mrossi camion-2024 192.168.1.10:8080
```

All'accesso riuscito compare:

```
Login riuscito come mrossi.
Connesso. Scrivi un testo e premi invio per mandarlo al server,
'/quit' per uscire. Il server può rispondere in broadcast o in diretto.
La posizione (simulata) viene inviata automaticamente ogni 30s.
```

Se le credenziali sono sbagliate: `Login fallito: <motivo>`. Un secondo accesso
con lo **stesso** username mentre il primo è ancora connesso viene rifiutato:
resta valido chi è entrato per primo.

### 2.3 Durante la sessione

- **Invio della posizione**: automatico, ogni 30 secondi, senza fare nulla. Il
  primo campione parte dopo il primo intervallo, non subito.
- **Inviare un messaggio all'azienda**: scrivere il testo e premere invio. Le
  righe vuote sono ignorate.
- **Ricevere messaggi dall'azienda**:
  - messaggio a tutti (broadcast): `[mittente] testo`
  - messaggio solo per te: `[mittente -> te] testo`
- **Uscire**: `/quit` (oppure chiudere lo standard input, Ctrl+D / Ctrl+Z).

### 2.4 Caduta della connessione

Se la connessione si interrompe per un motivo non voluto, il client **riprova da
solo**:

```
Connessione persa (<motivo>). Riprovo tra 1s (tentativo 1/5)...
```

L'attesa raddoppia a ogni tentativo. Dopo 5 tentativi falliti:

```
Connessione persa (<motivo>). Esauriti i 5 tentativi di riconnessione: esco.
```

Un `/quit` esplicito o un login fallito **non** fanno scattare la riconnessione.

---

## 3. Guida per l'amministratore (console del server)

### 3.1 Avvio del server

```bash
cargo run -p server --release
```

Argomenti opzionali, in quest'ordine:

```
server [indirizzo:porta] [file_utenti] [file_storico]
```

| Argomento | Default | A cosa serve |
|---|---|---|
| `indirizzo:porta` | `0.0.0.0:8080` | interfaccia e porta di ascolto |
| `file_utenti` | `users.json` | dove sono salvate le credenziali |
| `file_storico` | `tracking_history.json` | dove è salvato lo storico delle posizioni |

All'avvio il server stampa l'intestazione con l'elenco dei comandi. Se un file
utenti o di storico è presente ma illeggibile, il server **si ferma** invece di
ripartire con dati vuoti (che alla prima scrittura cancellerebbero quelli esistenti).

I comandi si digitano **sullo standard input del processo server**.

### 3.2 Comandi

| Comando | Effetto |
|---|---|
| `/users` | elenco degli utenti connessi con lo stato corrente |
| `/all <testo>` | invia `<testo>` a tutti i client connessi |
| `/to <utente> <testo>` | invia `<testo>` al solo `<utente>` |
| `/stats <utente> <periodo>` | report di movimento di `<utente>` (vedi sezione 3.4) |
| `/help` | ristampa l'elenco dei comandi |

I messaggi sono **testo libero scritto dall'amministratore**, per esempio:

```
/all Segnalazione: traffico intenso tra Turate e Lomazzo
/to mrossi Rientra in sede a fine turno
```

Il testo che non inizia con `/` non viene inviato a nessuno: ogni invio è
esplicito, così un comando scritto male (`/user` invece di `/users`) segnala
l'errore invece di finire a tutti i client.

### 3.3 Stato dei veicoli — `/users`

```
connessi (2):
  mrossi               Moving
  bverdi               Idle
```

Stati possibili:

| Stato | Significato |
|---|---|
| `in attesa della prima posizione` | connesso ma non ha ancora inviato coordinate |
| `Idle` | fermo: le coordinate non cambiano |
| `Moving` | in movimento: le coordinate sono cambiate |
| `Disconnected` | non connesso |

Il passaggio da `Moving` a `Idle` avviene dopo **3 minuti** di coordinate
invariate. Se nessuno è connesso: `nessun utente connesso`.

### 3.4 Analisi dei movimenti — `/stats`

```
/stats <utente> day
/stats <utente> week
/stats <utente> month
/stats <utente> custom <inizio_utc> <fine_utc>
```

- `day` / `week` / `month`: giorno, settimana (da lunedì) o mese **corrente**, sul
  fuso orario `Europe/Rome` (il cambio di ora legale è gestito).
- `custom`: intervallo arbitrario, con estremi in secondi UTC.

Esempio di report:

```
Statistiche di mrossi — dal 04/09/2026 00:00 al 05/09/2026 00:00
  Campioni ricevuti:    128 (dal 04/09/2026 08:12 al 04/09/2026 17:40)
  Tragitto percorso:    23.41 km
  Durata movimento:     3h 05m 12s
  Durata pause:         41m 30s
  Tempo osservato:      3h 46m 42s
  Tempo non osservato:  5h 41m 18s  (client non connesso o campioni persi)
  Velocità in movimento: 42.6 km/h
  Velocità complessiva:  7.6 km/h
  Punti del tragitto (128):
       1. 45.06280, 7.66200
       2. 45.06315, 7.66241
       …
```

Come si leggono i campi:

| Campo | Significato |
|---|---|
| **Campioni ricevuti** | quante posizioni sono arrivate nel periodo, e l'intervallo effettivo coperto |
| **Tragitto percorso** | distanza totale percorsa (formula di Haversine), in m o km |
| **Durata movimento** | tempo totale trascorso in stato di movimento |
| **Durata pause** | tempo totale di sosta *a veicolo connesso* (lo stato "non connesso" non conta come pausa) |
| **Tempo osservato** | somma dei tratti in cui il server aveva dati continui |
| **Tempo non osservato** | resto del periodo: client spento o campioni persi |
| **Velocità in movimento** | media calcolata solo sui tratti in movimento (esclude le pause) |
| **Velocità complessiva** | media sull'intero tempo osservato, pause incluse |
| **Punti del tragitto** | le posizioni in ordine cronologico; oltre 8 punti se ne mostrano i primi e gli ultimi con il conteggio di quelli omessi |

Se nel periodo non ci sono dati: `Campioni ricevuti: nessuno nel periodo` e le
grandezze sono a zero.

### 3.5 Output del server e file prodotti

Di default il server stampa **solo** l'intestazione d'avvio, le risposte ai
comandi e i propri errori: registrazioni, login, singole posizioni e cambi di
stato non compaiono. Per la traccia completa di ogni evento:

```bash
GEORUGGINE_VERBOSE=1 cargo run -p server --release
```

(accetta `1`, `true`, `yes`, `si`, `on`).

Nella cartella di lavoro il server crea e mantiene:

| File | Contenuto |
|---|---|
| `users.json` | credenziali registrate |
| `tracking_history.json` | storico delle sessioni e dei campioni di posizione |
| `cpu_usage.log` | una riga ogni 2 minuti con il tempo di CPU del processo server |

**Al riavvio del server** utenti e storico restano disponibili: `/stats` continua a
riportare i dati raccolti prima. Lo stato dei veicoli riparte invece da
`Disconnected`, perché all'avvio non ci sono connessioni.

---

## 4. Prova rapida (tempi accorciati)

I tempi della specifica (posizione ogni 30 s, ritorno a `Idle` dopo 3 minuti)
rendono lunga una verifica a mano. Solo per le prove si possono accorciare con
delle variabili d'ambiente, senza cambiare i valori di default:

| Variabile | La legge | Default | Accorcia |
|---|---|---|---|
| `GEORUGGINE_POSITION_INTERVAL` | client | 30 s | invio della posizione |
| `GEORUGGINE_IDLE_THRESHOLD` | server | 180 s | ritorno a `Idle` |
| `GEORUGGINE_CLIENT_TIMEOUT` | server | 90 s | timeout di inattività della connessione |

Ogni override attivo viene annunciato all'avvio del processo. Esempio:

```bash
# terminale del server
GEORUGGINE_IDLE_THRESHOLD=10 cargo run -p server --release

# terminale del client
GEORUGGINE_POSITION_INTERVAL=2 cargo run -p client --release -- login mrossi camion-2024
```

Sequenza di dimostrazione completa: `DEMO.md`.

---

## 5. Problemi frequenti

| Sintomo | Causa / rimedio |
|---|---|
| `Registrazione fallita: username già in uso` | scegliere un altro username, oppure fare direttamente `login` se è il proprio |
| `Login fallito: credenziali non valide` | username o password errati |
| Il secondo `login` con lo stesso utente non entra | l'utente è già connesso da un'altra sessione; disconnettere la prima |
| `/stats` dice "nessun utente connesso" o campioni a zero | il client non ha ancora inviato posizioni (il primo campione arriva dopo un intervallo) o il periodo scelto è vuoto |
| Il server termina subito all'avvio | è stato lanciato senza uno standard input interattivo (es. in background): la console legge la fine dell'input e chiude |
| Il server non parte e segnala un file da controllare | `users.json` o `tracking_history.json` è corrotto: rimuoverlo solo se si accetta di perdere quei dati |
