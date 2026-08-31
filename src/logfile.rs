use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use chrono::{DateTime, Utc};

static LOG: OnceLock<Mutex<BufWriter<File>>> = OnceLock::new();

pub fn init(path: &str) -> std::io::Result<()> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    LOG.set(Mutex::new(BufWriter::new(file)))
        .map_err(|_| std::io::Error::other("log already initialized"))?;
    write("vlat started, open for bidness");
    Ok(())
}

pub fn write(msg: &str) {
    if let Some(lock) = LOG.get() {
        if let Ok(mut w) = lock.lock() {
            let _ = writeln!(w, "{} {}", timestamp(), msg);
            let _ = w.flush();
        }
    }
}

fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_epoch_secs(secs)
}

/// Format a unix timestamp as ISO 8601 UTC, e.g. "2026-07-03T18:12:54Z".
pub fn format_epoch_secs(secs: u64) -> String {
    let dt = DateTime::<Utc>::from_timestamp(secs as i64, 0).unwrap_or_default();
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
