# Scenario di demo — Georuggine

Durata ~10 minuti. Tre terminali: **S** (server) e due client **A** / **B**.
Si usano i tempi accorciati (`common/src/timing.rs`) per non aspettare 30 s a
campione e 3 minuti per il ritorno a `Idle`; i default della consegna restano
invariati, ogni override è annunciato all'avvio.

Tutti i comandi si lanciano dalla cartella `georuggine/`.

---

## 1. Avvio del server e log CPU

Terminale **S**:

```bash
GEORUGGINE_IDLE_THRESHOLD=10 GEORUGGINE_CLIENT_TIMEOUT=30 cargo run -p server --release
```

- Mostrare l'intestazione d'avvio: elenco dei comandi della console e la riga che
  annuncia l'override dei tempi (`ATTENZIONE GEORUGGINE_IDLE_THRESHOLD=10s ...`).
- Mostrare che nella cartella compaiono `users.json` e `tracking_history.json`
  (storico persistito su file, sopravvive ai riavvii) e, entro due minuti,
  `cpu_usage.log` con una riga ogni 120 s: `[...] CPU process usage: 0.xx%`.
  Commentare che il consumo del processo resta sotto il decimo di punto
  percentuale (vedi `MANUALE_PROGETTISTA.md` § 4.1).

## 2. Registrazione, login, comunicazione

Terminali **A** e **B** (una volta ciascuno):

```bash
cargo run -p client --release -- register alice pw-alice
cargo run -p client --release -- register bob   pw-bob
```

Poi il login, che apre la sessione interattiva (posizione ogni 2 s con l'override):

```bash
# terminale A
GEORUGGINE_POSITION_INTERVAL=2 cargo run -p client --release -- login alice pw-alice
# terminale B
GEORUGGINE_POSITION_INTERVAL=2 cargo run -p client --release -- login bob pw-bob
```

Dalla console **S**:

- `/users` → elenco dei connessi (alice, bob).
- `/all Il server andra' in manutenzione` → arriva a **entrambi** i client.
- `/to alice Ciao Alice!` → arriva **solo** ad alice.
- Da **A** e da **B**: scrivere una riga di testo libero → il server la riceve
  (visibile con `GEORUGGINE_VERBOSE=1`, altrimenti solo lato server). Non viene
  ritrasmessa agli altri utenti: la chat è solo client→server.

## 3. Macchina a stati (osservata dalla console)

Lo stato dell'utente è gestito dal server e **non** viene trasmesso ai client: lo
si osserva dal terminale **S**.

- Con i client che inviano posizioni, la console stampa le transizioni
  (`[bob] stato -> Moving`). Il simulatore, a ogni tick, con probabilità 1/2 si
  sposta e 1/2 resta fermo: si vedono sia `Idle -> Moving` sia, dopo ~10 s di
  coordinate invariate (soglia accorciata), `Moving -> Idle`.
- `/users` mostra lo stato corrente di ciascun utente.
- Chiudere il client **B** (`/quit`): la console segnala `[bob] stato ->
  Disconnected`.

## 4. Statistiche di movimento

Lasciare girare **A** (e riaprire **B**) per 3–5 minuti, così lo storico accumula
segmenti di movimento e pause reali. Poi dalla console **S**:

- `/stats alice day` → report leggibile con le quattro grandezze chieste dalla
  consegna: **tragitto percorso**, **velocità media** (esclude i periodi da
  fermo), **durata del movimento**, **durata delle pause**. Istanti nel fuso
  `Europe/Rome`.
- `/stats alice week` e `/stats alice month` → stesso metodo di calcolo su
  finestra più ampia (qui contengono i dati di oggi): mostrano che la selezione
  per giorno / settimana / mese funziona.
- `/stats alice custom <start_utc> <end_utc>` → intervallo arbitrario.

> **Demo con storico su più giorni.** `tracking_history.json` è persistente: per
> mostrare `week`/`month` con dati di più giornate basta far girare il server per
> qualche giorno prima della demo — non serve alcun comando di popolamento
> artificiale (il vecchio `/seed` è stato rimosso perché incompatibile con lo
> storico persistente).

## 5. Persistenza al riavvio

- Fermare il server (`Ctrl+C` sul terminale **S**) e rilanciarlo con lo stesso
  comando del punto 1.
- `/stats alice day` → i dati raccolti prima del riavvio ci sono ancora.
- Un nuovo `register alice ...` → `RegisterErr` (utente già presente: anche
  `users.json` è persistito).

---

## Comandi della console operatore

| Comando | Effetto |
|---|---|
| `/all <testo>` | messaggio a tutti i client connessi |
| `/to <utente> <testo>` | messaggio a un solo utente |
| `/stats <utente> <day\|week\|month\|custom start end>` | report di movimento |
| `/users` | elenco dei connessi con lo stato |
| `/help` | elenco dei comandi |
