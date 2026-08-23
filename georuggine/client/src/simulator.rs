//! Simulatore del movimento del veicolo — **indipendente dalla rete**.
//!
//! Modulo separato apposta da `main.rs` (vedi ORGANIZZAZIONE.md: "Andrea
//! mantiene la proprietà di [...] simulatore", elencato distintamente da
//! "client"): questo file non conosce `ClientMessage`, socket, canali o
//! `tokio`. Espone solo "genera la prossima posizione simulata"; chi fa
//! rete (`main.rs`) decide come impacchettarla in un messaggio e spedirla.
//!
//! Essendo pura logica sincrona, si presta a test unitari veri (`#[test]`,
//! non `#[tokio::test]`): niente qui dentro tocca I/O.

/// Punto di partenza di default per il simulatore (Torino, sede del PoliTO):
/// solo una base plausibile da cui far muovere il veicolo simulato.
pub const DEFAULT_START_LAT: f64 = 45.0628;
pub const DEFAULT_START_LON: f64 = 7.6620;

/// Genera posizioni simulate per un veicolo: ad ogni tick, con probabilità
/// 1/2 si sposta di un piccolo offset casuale (simula "in movimento"),
/// altrimenti resta identica (simula "fermo"). Necessario per poter
/// osservare in pratica sia la transizione Idle->Moving (cambio coordinate)
/// sia quella Moving->Idle (coordinate ferme abbastanza a lungo), gestite
/// lato server (vedi `server::state`).
pub struct Simulator {
    lat: f64,
    lon: f64,
    rng: SimpleRng,
}

impl Simulator {
    pub fn new(seed: u64, start_lat: f64, start_lon: f64) -> Self {
        Self {
            lat: start_lat,
            lon: start_lon,
            rng: SimpleRng::new(seed),
        }
    }

    /// Crea un simulatore per `username`, con seed derivato da username +
    /// istante di avvio (così più istanze non generano la stessa identica
    /// sequenza), a partire dal punto di partenza di default.
    pub fn for_user(username: &str) -> Self {
        Self::new(seed_from(username), DEFAULT_START_LAT, DEFAULT_START_LON)
    }

    /// Avanza di un tick e ritorna la nuova posizione (lat, lon).
    pub fn next_position(&mut self) -> (f64, f64) {
        if self.rng.next_u64().is_multiple_of(2) {
            self.lat += self.rng.next_offset() * 0.001; // ~100 m per passo, a 45° di latitudine
            self.lon += self.rng.next_offset() * 0.001;
        }
        (self.lat, self.lon)
    }
}

/// Seed diverso per ogni client (dipende da username + istante di avvio),
/// così più istanze non generano la stessa identica sequenza di posizioni.
fn seed_from(username: &str) -> u64 {
    // FNV-1a: hash semplice, sufficiente qui (non serve crittografico).
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in username.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    hash ^ now
}

/// Generatore pseudo-casuale minimale (xorshift64). Evita di aggiungere la
/// dipendenza `rand` solo per simulare il movimento — coerente con
/// l'attenzione alla dimensione dell'eseguibile richiesta dalla consegna.
struct SimpleRng(u64);

impl SimpleRng {
    fn new(seed: u64) -> Self {
        // Seed 0 renderebbe xorshift degenere (resta sempre 0).
        Self(if seed == 0 { 0xdead_beef } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Float pseudo-casuale in [-1.0, 1.0).
    fn next_offset(&mut self) -> f64 {
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64; // [0,1)
        unit * 2.0 - 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stesso_seed_stessa_sequenza() {
        let mut a = Simulator::new(42, 0.0, 0.0);
        let mut b = Simulator::new(42, 0.0, 0.0);
        for _ in 0..20 {
            assert_eq!(a.next_position(), b.next_position());
        }
    }

    #[test]
    fn seed_diverso_prima_o_poi_diverge() {
        let mut a = Simulator::new(1, 0.0, 0.0);
        let mut b = Simulator::new(2, 0.0, 0.0);
        let diverge = (0..20).any(|_| a.next_position() != b.next_position());
        assert!(
            diverge,
            "due seed diversi non dovrebbero produrre sempre la stessa sequenza"
        );
    }

    #[test]
    fn seed_zero_non_degenera() {
        // xorshift con stato 0 resterebbe bloccato su 0 per sempre:
        // Simulator::new deve evitarlo (vedi SimpleRng::new).
        let mut sim = Simulator::new(0, 10.0, 10.0);
        let positions: Vec<(f64, f64)> = (0..10).map(|_| sim.next_position()).collect();
        assert!(positions.iter().any(|&p| p != (10.0, 10.0)));
    }

    #[test]
    fn spostamento_per_tick_resta_piccolo() {
        // Il punto è simulare un veicolo, non un teletrasporto: ogni tick
        // sposta al più di ~0.001 gradi (~100 m) su ciascun asse.
        let mut sim = Simulator::new(123, 0.0, 0.0);
        let mut prev = (0.0, 0.0);
        for _ in 0..50 {
            let next = sim.next_position();
            assert!((next.0 - prev.0).abs() <= 0.001);
            assert!((next.1 - prev.1).abs() <= 0.001);
            prev = next;
        }
    }
}
