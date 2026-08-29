//! Console dell'operatore: analisi dei comandi digitati sullo stdin del
//! server e formattazione leggibile delle statistiche.
//!
//! Sta in un modulo separato da `main.rs` per due motivi. Il primo è che il
//! parsing e la formattazione sono funzioni pure: qui si possono provare con
//! test normali, mentre dentro `admin_console` servirebbe uno stdin finto.
//! Il secondo è che la consegna chiede che il report mostri "il tragitto
//! percorso, la velocità media, la durata complessiva del movimento e la
//! durata delle pause": quella presentazione è codice a sé, non due righe
//! dentro il ciclo di lettura.

use chrono::DateTime;
use chrono_tz::Europe::Rome;
use server::tracking::MovementReport;

/// Testo di aiuto della console, stampato all'avvio e con `/help`.
pub const CONSOLE_HELP: &str = "\
Comandi della console operatore:
  /all <testo>                  manda <testo> a tutti i client connessi
  /to <utente> <testo>          manda <testo> al solo <utente>
  /stats <utente> <periodo>     statistiche di movimento di <utente>
                                periodo: day | week | month
                                         custom <start_utc> <end_utc>
  /users                        elenco degli utenti connessi
  /help                         mostra questo elenco";

/// Quanti punti del tragitto stampare per esteso prima di riassumere.
/// Con un campione ogni 30 secondi una giornata intera ne produce migliaia:
/// riversarli tutti sul terminale renderebbe illeggibile il resto del report.
const MAX_ROUTE_POINTS: usize = 8;

/// Comando riconosciuto sulla riga di console.
///
/// Il testo libero **non** è più un broadcast implicito: prima lo era, e un
/// comando scritto male (`/user` invece di `/users`) finiva spedito a tutti
/// i client invece di segnalare l'errore. Ora ogni invio è esplicito.
#[derive(Debug, PartialEq)]
pub enum ConsoleCommand<'a> {
    /// `/all <testo>`
    Broadcast(&'a str),
    /// `/to <utente> <testo>`
    Direct { to: &'a str, text: &'a str },
    /// `/stats <utente> <periodo>`: gli argomenti grezzi, interpretati altrove.
    Stats(&'a str),
    /// `/users`
    Users,
    /// `/help`
    Help,
    /// Riga riconosciuta ma usata male: si stampa questo messaggio.
    Usage(&'static str),
}

/// Interpreta una riga della console. `None` per le righe vuote, che vanno
/// semplicemente ignorate.
pub fn parse_console_line(line: &str) -> Option<ConsoleCommand<'_>> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    // Un comando senza argomenti e lo stesso comando con argomenti si
    // distinguono qui: `/all` da solo è un errore d'uso, `/all ciao` no.
    let (command, rest) = match line.split_once(char::is_whitespace) {
        Some((command, rest)) => (command, rest.trim()),
        None => (line, ""),
    };

    let parsed = match command {
        "/all" if !rest.is_empty() => ConsoleCommand::Broadcast(rest),
        "/all" => ConsoleCommand::Usage("Uso: /all <testo>"),

        "/to" => match rest.split_once(char::is_whitespace) {
            Some((to, text)) if !to.is_empty() && !text.trim().is_empty() => {
                ConsoleCommand::Direct {
                    to,
                    text: text.trim(),
                }
            }
            _ => ConsoleCommand::Usage("Uso: /to <utente> <testo>"),
        },

        "/stats" if !rest.is_empty() => ConsoleCommand::Stats(rest),
        "/stats" => ConsoleCommand::Usage(crate::STATS_USAGE),

        "/users" if rest.is_empty() => ConsoleCommand::Users,
        "/users" => ConsoleCommand::Usage("Uso: /users (senza argomenti)"),

        "/help" => ConsoleCommand::Help,

        // Qualunque altra cosa: né un comando noto né un invio implicito.
        _ => ConsoleCommand::Usage(
            "comando non riconosciuto: usa /all per scrivere a tutti, /help per l'elenco",
        ),
    };

    Some(parsed)
}

/// Rende leggibile una durata in secondi: `3h 05m 12s`, `18m 30s`, `45s`.
/// Le unità superiori compaiono solo se diverse da zero, così un valore
/// piccolo non viene sepolto da `0h 00m`.
pub fn format_duration(seconds: i64) -> String {
    if seconds <= 0 {
        return "0s".to_string();
    }

    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;

    if hours > 0 {
        format!("{hours}h {minutes:02}m {secs:02}s")
    } else if minutes > 0 {
        format!("{minutes}m {secs:02}s")
    } else {
        format!("{secs}s")
    }
}

/// Metri sotto il chilometro, chilometri sopra: `847 m`, `2.34 km`.
pub fn format_distance(meters: f64) -> String {
    if !meters.is_finite() {
        return "n/d".to_string();
    }
    if meters.abs() < 1000.0 {
        format!("{meters:.0} m")
    } else {
        format!("{:.2} km", meters / 1000.0)
    }
}

/// Velocità in km/h: per una flotta di veicoli è l'unità che si legge a
/// colpo d'occhio, i m/s del modello no.
pub fn format_speed(meters_per_second: f64) -> String {
    if !meters_per_second.is_finite() {
        return "n/d".to_string();
    }
    format!("{:.1} km/h", meters_per_second * 3.6)
}

/// Istante UTC reso nel fuso di Roma, come i periodi di `/stats`: mostrare
/// UTC costringerebbe l'operatore a fare la conversione a mente.
pub fn format_instant(utc_seconds: i64) -> String {
    match DateTime::from_timestamp(utc_seconds, 0) {
        Some(instant) => instant
            .with_timezone(&Rome)
            .format("%d/%m/%Y %H:%M:%S")
            .to_string(),
        None => format!("timestamp non valido ({utc_seconds})"),
    }
}

/// Il tragitto percorso, richiesto esplicitamente dalla consegna.
/// Oltre `MAX_ROUTE_POINTS` si mostrano i primi e gli ultimi con il conteggio
/// di quelli omessi in mezzo, per non allagare il terminale.
fn format_route(report: &MovementReport) -> String {
    let points = &report.route;
    if points.is_empty() {
        return "  (nessun punto registrato)".to_string();
    }

    let render = |index: usize| {
        let point = points[index];
        format!(
            "    {:>4}. {:.5}, {:.5}",
            index + 1,
            point.lat(),
            point.lon()
        )
    };

    if points.len() <= MAX_ROUTE_POINTS {
        return (0..points.len()).map(render).collect::<Vec<_>>().join("\n");
    }

    let head = MAX_ROUTE_POINTS / 2;
    let tail = points.len() - head;
    let mut righe: Vec<String> = (0..head).map(render).collect();
    righe.push(format!("         … {} punti intermedi omessi", tail - head));
    righe.extend((tail..points.len()).map(render));
    righe.join("\n")
}

/// Report di movimento in forma leggibile.
///
/// Sostituisce il `{report:#?}` della prima versione: il dump `Debug`
/// mostrava i campi grezzi (secondi, m/s, timestamp Unix) e lasciava
/// all'operatore le conversioni.
pub fn format_report(report: &MovementReport) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "Statistiche di {} — dal {} al {}\n",
        report.username,
        format_instant(report.period_start),
        format_instant(report.period_end)
    ));

    match (report.coverage_start, report.coverage_end) {
        (Some(start), Some(end)) => out.push_str(&format!(
            "  Campioni ricevuti:    {} (dal {} al {})\n",
            report.sample_count,
            format_instant(start),
            format_instant(end)
        )),
        _ => out.push_str("  Campioni ricevuti:    nessuno nel periodo\n"),
    }

    out.push_str(&format!(
        "  Tragitto percorso:    {}\n",
        format_distance(report.total_distance_m)
    ));
    out.push_str(&format!(
        "  Durata movimento:     {}\n",
        format_duration(report.moving_seconds)
    ));
    out.push_str(&format!(
        "  Durata pause:         {}\n",
        format_duration(report.paused_seconds)
    ));
    out.push_str(&format!(
        "  Tempo osservato:      {}\n",
        format_duration(report.observed_seconds)
    ));
    out.push_str(&format!(
        "  Tempo non osservato:  {}  (client non connesso o campioni persi)\n",
        format_duration(report.unobserved_seconds)
    ));

    // Due velocità perché rispondono a domande diverse: la prima dice quanto
    // va veloce il veicolo quando si muove, la seconda quanto rende
    // sull'intero periodo osservato, pause comprese.
    out.push_str(&format!(
        "  Velocità in movimento: {}\n",
        format_speed(report.average_moving_speed_m_s)
    ));
    out.push_str(&format!(
        "  Velocità complessiva:  {}\n",
        format_speed(report.average_overall_speed_m_s)
    ));

    out.push_str(&format!("  Punti del tragitto ({}):\n", report.route.len()));
    out.push_str(&format_route(report));

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use server::tracking::GeoPoint;

    fn report_di_prova(route: Vec<GeoPoint>, sample_count: usize) -> MovementReport {
        MovementReport {
            username: "bob".to_string(),
            period_start: 1_800_000_000,
            period_end: 1_800_086_400,
            coverage_start: Some(1_800_000_060),
            coverage_end: Some(1_800_003_660),
            route,
            total_distance_m: 2_340.0,
            elapsed_seconds: 86_400,
            observed_seconds: 3_600,
            moving_seconds: 1_110,
            paused_seconds: 2_490,
            unobserved_seconds: 82_800,
            average_moving_speed_m_s: 2.108,
            average_overall_speed_m_s: 0.65,
            sample_count,
        }
    }

    fn punto(lat: f64, lon: f64) -> GeoPoint {
        GeoPoint::try_new(lat, lon).unwrap()
    }

    #[test]
    fn all_richiede_un_testo() {
        assert_eq!(
            parse_console_line("/all ciao a tutti"),
            Some(ConsoleCommand::Broadcast("ciao a tutti"))
        );
        assert!(matches!(
            parse_console_line("/all"),
            Some(ConsoleCommand::Usage(_))
        ));
        assert!(matches!(
            parse_console_line("/all    "),
            Some(ConsoleCommand::Usage(_))
        ));
    }

    #[test]
    fn to_separa_destinatario_e_testo() {
        assert_eq!(
            parse_console_line("/to carol ciao come stai"),
            Some(ConsoleCommand::Direct {
                to: "carol",
                text: "ciao come stai"
            })
        );
        // Destinatario senza testo: errore d'uso, non un messaggio vuoto.
        assert!(matches!(
            parse_console_line("/to carol"),
            Some(ConsoleCommand::Usage(_))
        ));
        assert!(matches!(
            parse_console_line("/to carol    "),
            Some(ConsoleCommand::Usage(_))
        ));
    }

    #[test]
    fn users_e_help_non_vogliono_argomenti() {
        assert_eq!(parse_console_line("/users"), Some(ConsoleCommand::Users));
        assert_eq!(
            parse_console_line("  /users  "),
            Some(ConsoleCommand::Users)
        );
        assert!(matches!(
            parse_console_line("/users bob"),
            Some(ConsoleCommand::Usage(_))
        ));
        assert_eq!(parse_console_line("/help"), Some(ConsoleCommand::Help));
    }

    #[test]
    fn stats_passa_gli_argomenti_grezzi() {
        assert_eq!(
            parse_console_line("/stats bob day"),
            Some(ConsoleCommand::Stats("bob day"))
        );
        assert!(matches!(
            parse_console_line("/stats"),
            Some(ConsoleCommand::Usage(_))
        ));
    }

    #[test]
    fn il_testo_libero_non_e_piu_un_broadcast_implicito() {
        // Il caso che questa regola esiste per prevenire: un comando scritto
        // male non deve finire spedito a tutti i client.
        assert!(matches!(
            parse_console_line("/user"),
            Some(ConsoleCommand::Usage(_))
        ));
        assert!(matches!(
            parse_console_line("ciao a tutti"),
            Some(ConsoleCommand::Usage(_))
        ));
    }

    #[test]
    fn le_righe_vuote_vengono_ignorate() {
        assert_eq!(parse_console_line(""), None);
        assert_eq!(parse_console_line("   "), None);
        assert_eq!(parse_console_line("\t"), None);
    }

    #[test]
    fn durate_leggibili() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(-5), "0s");
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(90), "1m 30s");
        assert_eq!(format_duration(3_600), "1h 00m 00s");
        assert_eq!(format_duration(11_112), "3h 05m 12s");
    }

    #[test]
    fn distanze_e_velocita_leggibili() {
        assert_eq!(format_distance(0.0), "0 m");
        assert_eq!(format_distance(847.3), "847 m");
        assert_eq!(format_distance(2_340.0), "2.34 km");
        assert_eq!(format_distance(f64::NAN), "n/d");

        // 10 m/s = 36 km/h
        assert_eq!(format_speed(10.0), "36.0 km/h");
        assert_eq!(format_speed(f64::INFINITY), "n/d");
    }

    #[test]
    fn istanti_nel_fuso_di_roma() {
        // 1_800_000_000 = 2027-01-15 08:00:00 UTC, cioè le 09:00 a Roma
        // (ora solare: gennaio).
        let reso = format_instant(1_800_000_000);
        assert!(reso.contains("09:00:00"), "reso: {reso}");
        assert!(reso.starts_with("15/01/2027"), "reso: {reso}");
    }

    #[test]
    fn il_report_mostra_tutto_cio_che_la_consegna_chiede() {
        let report = report_di_prova(vec![punto(45.07, 7.68), punto(45.08, 7.69)], 42);
        let reso = format_report(&report);

        assert!(reso.contains("Statistiche di bob"));
        assert!(reso.contains("Tragitto percorso:    2.34 km"));
        assert!(reso.contains("Durata movimento:     18m 30s"));
        assert!(reso.contains("Durata pause:         41m 30s"));
        assert!(reso.contains("Velocità in movimento: 7.6 km/h"));
        assert!(reso.contains("45.07000, 7.68000"));
        // Nessun campo grezzo del dump Debug.
        assert!(!reso.contains("average_moving_speed_m_s"));
    }

    #[test]
    fn un_tragitto_lungo_viene_riassunto() {
        let route: Vec<GeoPoint> = (0..50)
            .map(|i| punto(45.0 + i as f64 * 0.001, 7.68))
            .collect();
        let reso = format_report(&report_di_prova(route, 50));

        assert!(reso.contains("Punti del tragitto (50)"));
        assert!(reso.contains("punti intermedi omessi"));
        // Primo e ultimo restano visibili.
        assert!(reso.contains("45.00000, 7.68000"));
        assert!(reso.contains("45.04900, 7.68000"));
    }

    #[test]
    fn un_periodo_senza_campioni_lo_dice() {
        let mut report = report_di_prova(Vec::new(), 0);
        report.coverage_start = None;
        report.coverage_end = None;
        let reso = format_report(&report);

        assert!(reso.contains("nessuno nel periodo"));
        assert!(reso.contains("nessun punto registrato"));
    }
}
