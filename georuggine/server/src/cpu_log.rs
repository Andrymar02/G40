//! Task periodico di logging del consumo CPU del processo server.

use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::time::{Duration, interval};

const LOG_INTERVAL: Duration = Duration::from_secs(120);
const LOG_FILE: &str = "cpu_usage.log";

pub async fn run() {
    // 1. Apriamo il file in modo asincrono (corretto per Tokio)
    let mut file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG_FILE)
        .await
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[cpu_log] impossibile aprire {LOG_FILE}: {e}");
            return;
        }
    };

    let pid = Pid::from(std::process::id() as usize);

    // 2. Creiamo il "sensore" UNA SOLA VOLTA e lo teniamo vivo (fondamentale per non avere sempre 0.0%)
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing().with_cpu()),
    );

    // Primo giro "a vuoto" per dare al sensore un punto di partenza
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_cpu(),
    );

    let mut tick = interval(LOG_INTERVAL);
    tick.tick().await; // Salta lo scatto immediato (t=0)

    loop {
        tick.tick().await; // Aspetta 2 minuti

        // Questa operazione dura pochissimi microsecondi, non blocca i client TCP.
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_cpu(),
        );

        let cpu_pct = sys.process(pid).map(|p| p.cpu_usage()).unwrap_or(0.0);

        let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%z");
        let line = format!("[{now}] CPU process usage: {cpu_pct:.2}%\n");

        // 3. Scrittura su file asincrona (corretto per Tokio)
        if let Err(e) = file.write_all(line.as_bytes()).await {
            eprintln!("[cpu_log] scrittura fallita: {e}");
        }
    }
}
