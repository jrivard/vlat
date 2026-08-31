// vlat - A colorful, pretty and over-engineered yet easy to use ping monitoring utility.
// Copyright (C) 2026  Jason D. Rivard <code@jrivard.org>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use std::{
    fs::File,
    io::{self, BufWriter, Write},
    time::{SystemTime, UNIX_EPOCH},
};
use chrono::{DateTime, Utc};
use csv::Writer as CsvWriter;
use serde::Serialize;
use crate::cli::{OutputFormat, PingMode};
use crate::state::TargetState;

pub fn iso_timestamp() -> String {
    epoch_ms_to_iso(now_epoch_ms())
}

pub fn now_epoch_ms() -> u64 {
    let dur = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    dur.as_secs() * 1000 + dur.subsec_millis() as u64
}

/// Format a unix-epoch millisecond timestamp as ISO 8601 UTC, e.g.
/// "2026-07-03T18:12:54.123Z".
pub fn epoch_ms_to_iso(epoch_ms: u64) -> String {
    let secs = (epoch_ms / 1000) as i64;
    let nanos = ((epoch_ms % 1000) * 1_000_000) as u32;
    let dt = DateTime::<Utc>::from_timestamp(secs, nanos).unwrap_or_default();
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Parse an ISO 8601 UTC timestamp (as produced by `epoch_ms_to_iso`) back into
/// unix-epoch milliseconds. The `.mmm` fraction is optional.
pub fn iso_to_epoch_ms(iso: &str) -> Option<u64> {
    let dt = DateTime::parse_from_rfc3339(iso).ok()?;
    u64::try_from(dt.timestamp_millis()).ok()
}

pub fn detect_format(path: &str, explicit: Option<&OutputFormat>) -> OutputFormat {
    if let Some(f) = explicit { return f.clone(); }
    let lower = path.to_lowercase();
    if lower.ends_with(".json") || lower.ends_with(".jsonl") || lower.ends_with(".ndjson") {
        OutputFormat::Json
    } else {
        OutputFormat::Csv
    }
}

fn mode_str(mode: &PingMode) -> &'static str {
    match mode {
        PingMode::Icmp  => "icmp",
        PingMode::Udp   => "udp",
        PingMode::Tcp   => "tcp",
        PingMode::Http  => "http",
        PingMode::Https => "https",
        PingMode::Dns   => "dns",
        PingMode::Tls   => "tls",
        PingMode::Ntp   => "ntp",
        PingMode::Ssh   => "ssh",
        PingMode::Smtp  => "smtp",
        PingMode::Smtps => "smtps",
        PingMode::Exec  => "exec",
        PingMode::Quic  => "quic",
    }
}

fn fmt_f64_opt(v: Option<f64>) -> String {
    match v { Some(x) => format!("{:.3}", x), None => String::new() }
}

/// Snapshot of all per-target statistics emitted alongside each probe row.
struct Stats {
    sent:            u64,
    recv:            u64,
    drops:           u64,
    dups:            u64,
    loss_pct:        f64,
    win_loss_pct:    f64,
    win_avg_ms:      Option<f64>,
    win_min_ms:      Option<f64>,
    win_max_ms:      Option<f64>,
    win_p50_ms:      Option<f64>,
    win_p95_ms:      Option<f64>,
    win_p99_ms:      Option<f64>,
    win_stddev_ms:   Option<f64>,
    win_cv_pct:      Option<f64>,
    win_jitter_ms:   Option<f64>,
    win_mtr_ms:      Option<f64>,
    srtt_ms:         Option<f64>,
    rttvar_ms:       Option<f64>,
    cur_drop_streak: u32,
    max_drop_streak: u32,
}

impl Stats {
    fn from_state(s: &TargetState) -> Self {
        let has_window = !s.window.is_empty();
        let has_srtt   = s.srtt > 0.0;
        Stats {
            sent:            s.total_sent,
            recv:            s.total_sent.saturating_sub(s.drops as u64),
            drops:           s.drops as u64,
            dups:            s.dups as u64,
            loss_pct:        s.life_loss_pct(),
            win_loss_pct:    s.win_loss_pct(),
            win_avg_ms:      if has_window { Some(s.win_avg()) }    else { None },
            win_min_ms:      if has_window { Some(s.win_min()) }    else { None },
            win_max_ms:      if has_window { Some(s.win_max()) }    else { None },
            win_p50_ms:      if has_window { Some(s.win_median()) } else { None },
            win_p95_ms:      if has_window { Some(s.win_p95()) }    else { None },
            win_p99_ms:      if has_window { Some(s.win_p99()) }    else { None },
            win_stddev_ms:   if has_window { Some(s.win_stddev()) } else { None },
            win_cv_pct:      if has_window { Some(s.win_cv()) }     else { None },
            win_jitter_ms:   if !s.jitter_window.is_empty() { Some(s.win_jitter_avg()) } else { None },
            win_mtr_ms:      s.win_mtr(),
            srtt_ms:         if has_srtt { Some(s.srtt) }   else { None },
            rttvar_ms:       if has_srtt { Some(s.rttvar) } else { None },
            cur_drop_streak: s.cur_drop_streak,
            max_drop_streak: s.max_drop_streak,
        }
    }
}

pub enum OutputFile {
    // Boxed: `csv::Writer` carries its own internal write buffer, making it
    // much larger than the `BufWriter<File>` in the Json variant.
    Csv(Box<CsvWriter<File>>),
    Json(BufWriter<File>),
}

impl OutputFile {
    #[allow(clippy::too_many_arguments)]
    pub fn write_row(
        &mut self,
        state:   &TargetState,
        host:    Option<&str>,
        port:    u16,
        mode:    &PingMode,
        seq:     usize,
        dup:     bool,
        outcome: Result<f64, ()>,
    ) {
        match self {
            OutputFile::Csv(w)  => write_csv_row(w, state, host, port, mode, seq, dup, outcome),
            OutputFile::Json(w) => write_json_row(w, state, host, port, mode, seq, dup, outcome),
        }
    }
    pub fn format_name(&self) -> &'static str {
        match self { OutputFile::Csv(_) => "CSV", OutputFile::Json(_) => "JSON" }
    }
}

/// CSV counterpart of `ProbeRow`. Rates/latencies are pre-formatted to 3
/// decimal places as text (matching the historical CSV output) rather than
/// serialized as raw floats, since `csv`/`ryu` would otherwise print full
/// float precision (e.g. "23.451999999999998").
#[derive(Serialize)]
struct CsvRow<'a> {
    timestamp:       &'a str,
    label:           &'a str,
    host:            &'a str,
    ip:              &'a str,
    port:            u16,
    mode:            &'a str,
    seq:             usize,
    ok:              bool,
    dup:             bool,
    rtt_ms:          String,
    sent:            u64,
    recv:            u64,
    drops:           u64,
    dups:            u64,
    loss_pct:        String,
    win_loss_pct:    String,
    win_avg_ms:      String,
    win_min_ms:      String,
    win_max_ms:      String,
    win_p50_ms:      String,
    win_p95_ms:      String,
    win_p99_ms:      String,
    win_stddev_ms:   String,
    win_cv_pct:      String,
    win_jitter_ms:   String,
    win_mtr_ms:      String,
    srtt_ms:         String,
    rttvar_ms:       String,
    cur_drop_streak: u32,
    max_drop_streak: u32,
}

/// Open (or create) the CSV output file. The header row is derived
/// automatically from `CsvRow`'s field names on the first `serialize()`
/// call, and only when the file didn't already exist.
pub fn open_csv(path: &str) -> io::Result<CsvWriter<File>> {
    let is_new = !std::path::Path::new(path).exists();
    let file   = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    Ok(csv::WriterBuilder::new().has_headers(is_new).from_writer(file))
}

#[allow(clippy::too_many_arguments)]
pub fn write_csv_row(
    w:       &mut CsvWriter<File>,
    state:   &TargetState,
    host:    Option<&str>,
    port:    u16,
    mode:    &PingMode,
    seq:     usize,
    dup:     bool,
    outcome: Result<f64, ()>,
) {
    let ts      = iso_timestamp();
    let label   = &state.label;
    let label_s = if let Some(pos) = label.find(" (") { &label[..pos] } else { label };
    let ip_s    = state.current_ip.map(|a| a.to_string()).unwrap_or_default();
    let rtt_s   = match outcome { Ok(ms) => format!("{:.3}", ms), Err(()) => String::new() };

    let st = Stats::from_state(state);
    let row = CsvRow {
        timestamp:       &ts,
        label:           label_s,
        host:            host.unwrap_or(""),
        ip:              &ip_s,
        port,
        mode:            mode_str(mode),
        seq,
        ok:              outcome.is_ok(),
        dup,
        rtt_ms:          rtt_s,
        sent:            st.sent,
        recv:            st.recv,
        drops:           st.drops,
        dups:            st.dups,
        loss_pct:        format!("{:.3}", st.loss_pct),
        win_loss_pct:    format!("{:.3}", st.win_loss_pct),
        win_avg_ms:      fmt_f64_opt(st.win_avg_ms),
        win_min_ms:      fmt_f64_opt(st.win_min_ms),
        win_max_ms:      fmt_f64_opt(st.win_max_ms),
        win_p50_ms:      fmt_f64_opt(st.win_p50_ms),
        win_p95_ms:      fmt_f64_opt(st.win_p95_ms),
        win_p99_ms:      fmt_f64_opt(st.win_p99_ms),
        win_stddev_ms:   fmt_f64_opt(st.win_stddev_ms),
        win_cv_pct:      fmt_f64_opt(st.win_cv_pct),
        win_jitter_ms:   fmt_f64_opt(st.win_jitter_ms),
        win_mtr_ms:      fmt_f64_opt(st.win_mtr_ms),
        srtt_ms:         fmt_f64_opt(st.srtt_ms),
        rttvar_ms:       fmt_f64_opt(st.rttvar_ms),
        cur_drop_streak: st.cur_drop_streak,
        max_drop_streak: st.max_drop_streak,
    };
    let _ = w.serialize(&row);
    let _ = w.flush();
}

pub fn open_json(path: &str) -> io::Result<BufWriter<File>> {
    let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    Ok(BufWriter::new(file))
}

#[derive(Serialize)]
struct ProbeRow<'a> {
    timestamp:       &'a str,
    label:           &'a str,
    host:            &'a str,
    ip:              &'a str,
    port:            u16,
    mode:            &'a str,
    seq:             usize,
    ok:              bool,
    dup:             bool,
    rtt_ms:          Option<f64>,
    sent:            u64,
    recv:            u64,
    drops:           u64,
    dups:            u64,
    loss_pct:        f64,
    win_loss_pct:    f64,
    win_avg_ms:      Option<f64>,
    win_min_ms:      Option<f64>,
    win_max_ms:      Option<f64>,
    win_p50_ms:      Option<f64>,
    win_p95_ms:      Option<f64>,
    win_p99_ms:      Option<f64>,
    win_stddev_ms:   Option<f64>,
    win_cv_pct:      Option<f64>,
    win_jitter_ms:   Option<f64>,
    win_mtr_ms:      Option<f64>,
    srtt_ms:         Option<f64>,
    rttvar_ms:       Option<f64>,
    cur_drop_streak: u32,
    max_drop_streak: u32,
}

#[allow(clippy::too_many_arguments)]
pub fn write_json_row(
    w:       &mut BufWriter<File>,
    state:   &TargetState,
    host:    Option<&str>,
    port:    u16,
    mode:    &PingMode,
    seq:     usize,
    dup:     bool,
    outcome: Result<f64, ()>,
) {
    let ts    = iso_timestamp();
    let label = &state.label;
    let label_raw = if let Some(pos) = label.find(" (") { &label[..pos] } else { label };
    let ip_s  = state.current_ip.map(|a| a.to_string()).unwrap_or_default();
    let st    = Stats::from_state(state);

    let row = ProbeRow {
        timestamp:       &ts,
        label:           label_raw,
        host:            host.unwrap_or(""),
        ip:              &ip_s,
        port,
        mode:            mode_str(mode),
        seq,
        ok:              outcome.is_ok(),
        dup,
        rtt_ms:          outcome.ok(),
        sent:            st.sent,
        recv:            st.recv,
        drops:           st.drops,
        dups:            st.dups,
        loss_pct:        st.loss_pct,
        win_loss_pct:    st.win_loss_pct,
        win_avg_ms:      st.win_avg_ms,
        win_min_ms:      st.win_min_ms,
        win_max_ms:      st.win_max_ms,
        win_p50_ms:      st.win_p50_ms,
        win_p95_ms:      st.win_p95_ms,
        win_p99_ms:      st.win_p99_ms,
        win_stddev_ms:   st.win_stddev_ms,
        win_cv_pct:      st.win_cv_pct,
        win_jitter_ms:   st.win_jitter_ms,
        win_mtr_ms:      st.win_mtr_ms,
        srtt_ms:         st.srtt_ms,
        rttvar_ms:       st.rttvar_ms,
        cur_drop_streak: st.cur_drop_streak,
        max_drop_streak: st.max_drop_streak,
    };

    if let Ok(line) = serde_json::to_string(&row) {
        let _ = writeln!(w, "{}", line);
        let _ = w.flush();
    }
}

// ── --summary-json: a periodically-rewritten "current state" snapshot ──────
// Unlike write_csv_row/write_json_row (which append one line per probe event),
// this overwrites a single file with the whole run's summary stats each time
// it's called, so a dashboard or monitoring script can just read the latest
// state instead of tailing a growing log.

fn round3(v: f64) -> f64 { (v * 1000.0).round() / 1000.0 }

/// Strip the auto-appended " (ip)" suffix from a label, matching the
/// convention used for the `label` field in write_csv_row/write_json_row.
fn display_label(label: &str) -> &str {
    if let Some(pos) = label.find(" (") { &label[..pos] } else { label }
}

#[derive(Serialize)]
struct SummaryTarget<'a> {
    label:           &'a str,
    mode:            &'a str,
    address:         String,
    deleted:         bool,
    sent:            u64,
    recv:            u64,
    drops:           u64,
    dups:            u64,
    loss_pct:        f64,
    avg_ms:          Option<f64>,
    jitter_ms:       Option<f64>,
    min_ms:          Option<f64>,
    max_ms:          Option<f64>,
    stddev_ms:       Option<f64>,
    mtr_ms:          Option<f64>,
    max_drop_streak: u32,
}

#[derive(Serialize)]
struct SummaryTotals {
    sent:     u64,
    recv:     u64,
    drops:    u64,
    dups:     u64,
    loss_pct: f64,
}

#[derive(Serialize)]
struct Summary<'a> {
    timestamp: String,
    targets:   Vec<SummaryTarget<'a>>,
    totals:    SummaryTotals,
}

fn summary_target<'a>(s: &'a TargetState, mode: &'a str, deleted: bool) -> SummaryTarget<'a> {
    // Same "no real data yet" / "100% loss" distinction as the exit-summary
    // table: a target with zero probes sent hasn't produced a real avg/min/max,
    // so those fields come back null rather than a misleading 0.0.
    let no_data  = s.total_sent == 0;
    let all_lost = s.total_sent > 0 && s.drops as u64 == s.total_sent;
    let skip_ms  = no_data || all_lost;
    let mode_proto = mode.split(':').next().unwrap_or(mode);
    SummaryTarget {
        label:           display_label(&s.label),
        mode:            mode_proto,
        address:         s.current_ip.map(|a| a.to_string()).unwrap_or_default(),
        deleted,
        sent:            s.total_sent,
        recv:            s.total_sent.saturating_sub(s.drops as u64),
        drops:           s.drops as u64,
        dups:            s.dups as u64,
        loss_pct:        round3(s.life_loss_pct()),
        avg_ms:          if skip_ms { None } else { Some(round3(s.avg_latency())) },
        jitter_ms:       if skip_ms { None } else { Some(round3(s.avg_jitter())) },
        min_ms:          if skip_ms { None } else { Some(round3(s.life_min())) },
        max_ms:          if skip_ms { None } else { Some(round3(s.life_max())) },
        stddev_ms:       if skip_ms { None } else { Some(round3(s.life_stddev())) },
        mtr_ms:          s.win_mtr().map(round3),
        max_drop_streak: s.max_drop_streak,
    }
}

/// Write the current summary-stats snapshot to `path`, atomically (write to a
/// sibling `.tmp` file, then rename over the target) so a reader never sees a
/// half-written file even if it polls mid-write.
pub fn write_summary_json_snapshot(
    path:                &str,
    states:              &[TargetState],
    mode_labels:         &[String],
    deleted:             &[TargetState],
    deleted_mode_labels: &[String],
) -> io::Result<()> {
    let targets: Vec<SummaryTarget> = states.iter().zip(mode_labels.iter())
        .map(|(s, m)| summary_target(s, m, false))
        .chain(
            deleted.iter().zip(deleted_mode_labels.iter())
                .map(|(s, m)| summary_target(s, m, true))
        )
        .collect();

    let totals = SummaryTotals {
        sent:     targets.iter().map(|t| t.sent).sum(),
        recv:     targets.iter().map(|t| t.recv).sum(),
        drops:    targets.iter().map(|t| t.drops).sum(),
        dups:     targets.iter().map(|t| t.dups).sum(),
        loss_pct: {
            let sent: u64  = targets.iter().map(|t| t.sent).sum();
            let drops: u64 = targets.iter().map(|t| t.drops).sum();
            if sent == 0 { 0.0 } else { round3(drops as f64 / sent as f64 * 100.0) }
        },
    };

    let summary = Summary { timestamp: iso_timestamp(), targets, totals };
    let json = serde_json::to_string_pretty(&summary)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    let tmp_path = format!("{path}.tmp");
    std::fs::write(&tmp_path, json.as_bytes())?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::OutputFormat;

    // --- detect_format ---

    #[test]
    fn detect_csv_extension() {
        assert_eq!(detect_format("results.csv", None), OutputFormat::Csv);
    }

    #[test]
    fn detect_json_extension() {
        assert_eq!(detect_format("results.json", None), OutputFormat::Json);
    }

    #[test]
    fn detect_jsonl_extension() {
        assert_eq!(detect_format("results.jsonl", None), OutputFormat::Json);
    }

    #[test]
    fn detect_ndjson_extension() {
        assert_eq!(detect_format("results.ndjson", None), OutputFormat::Json);
    }

    #[test]
    fn detect_unknown_extension_defaults_to_csv() {
        assert_eq!(detect_format("results.txt", None), OutputFormat::Csv);
    }

    #[test]
    fn detect_no_extension_defaults_to_csv() {
        assert_eq!(detect_format("results", None), OutputFormat::Csv);
    }

    #[test]
    fn detect_case_insensitive_json() {
        assert_eq!(detect_format("results.JSON", None), OutputFormat::Json);
    }

    #[test]
    fn detect_case_insensitive_jsonl() {
        assert_eq!(detect_format("results.JSONL", None), OutputFormat::Json);
    }

    #[test]
    fn detect_explicit_overrides_extension() {
        assert_eq!(detect_format("results.csv", Some(&OutputFormat::Json)), OutputFormat::Json);
    }

    #[test]
    fn detect_explicit_csv_overrides_json_extension() {
        assert_eq!(detect_format("results.json", Some(&OutputFormat::Csv)), OutputFormat::Csv);
    }

    // --- iso_timestamp ---

    #[test]
    fn iso_timestamp_has_correct_length() {
        let ts = iso_timestamp();
        assert_eq!(ts.len(), 24, "expected 24-char timestamp, got: {ts}");
    }

    #[test]
    fn iso_timestamp_ends_with_z() {
        let ts = iso_timestamp();
        assert!(ts.ends_with('Z'), "should end with Z: {ts}");
    }

    #[test]
    fn iso_timestamp_has_correct_separators() {
        let ts = iso_timestamp();
        assert_eq!(&ts[4..5],   "-", "year-month sep in: {ts}");
        assert_eq!(&ts[7..8],   "-", "month-day sep in: {ts}");
        assert_eq!(&ts[10..11], "T", "date-time sep in: {ts}");
        assert_eq!(&ts[13..14], ":", "hour-minute sep in: {ts}");
        assert_eq!(&ts[16..17], ":", "minute-second sep in: {ts}");
        assert_eq!(&ts[19..20], ".", "second-ms sep in: {ts}");
    }

    #[test]
    fn iso_timestamp_fields_are_numeric() {
        let ts = iso_timestamp();
        // YYYY-MM-DDTHH:MM:SS.mmmZ
        for (label, part) in [
            ("year",   &ts[0..4]),
            ("month",  &ts[5..7]),
            ("day",    &ts[8..10]),
            ("hour",   &ts[11..13]),
            ("minute", &ts[14..16]),
            ("second", &ts[17..19]),
            ("millis", &ts[20..23]),
        ] {
            assert!(part.parse::<u64>().is_ok(), "{label} '{part}' is not numeric in: {ts}");
        }
    }

    #[test]
    fn iso_timestamp_year_is_plausible() {
        let ts = iso_timestamp();
        let year: u64 = ts[0..4].parse().unwrap();
        assert!(year >= 2024, "year {year} too old in: {ts}");
        assert!(year <= 2100, "year {year} too far in future in: {ts}");
    }

    #[test]
    fn iso_timestamp_month_is_valid() {
        let ts = iso_timestamp();
        let month: u64 = ts[5..7].parse().unwrap();
        assert!((1..=12).contains(&month), "month {month} out of range in: {ts}");
    }

    #[test]
    fn iso_timestamp_day_is_valid() {
        let ts = iso_timestamp();
        let day: u64 = ts[8..10].parse().unwrap();
        assert!((1..=31).contains(&day), "day {day} out of range in: {ts}");
    }

    #[test]
    fn iso_timestamp_hour_is_valid() {
        let ts = iso_timestamp();
        let hour: u64 = ts[11..13].parse().unwrap();
        assert!(hour <= 23, "hour {hour} out of range in: {ts}");
    }

    #[test]
    fn iso_timestamp_millis_is_valid() {
        let ts = iso_timestamp();
        let ms: u64 = ts[20..23].parse().unwrap();
        assert!(ms <= 999, "millis {ms} out of range in: {ts}");
    }

    // --- epoch_ms_to_iso / iso_to_epoch_ms ---

    #[test]
    fn epoch_ms_iso_roundtrip() {
        for ms in [0u64, 1, 1_000, 1_719_999_999_999, now_epoch_ms()] {
            let iso = epoch_ms_to_iso(ms);
            assert_eq!(iso_to_epoch_ms(&iso), Some(ms), "roundtrip failed for {ms} -> {iso}");
        }
    }

    #[test]
    fn epoch_ms_to_iso_known_value() {
        // 2021-01-01T00:00:00.000Z
        assert_eq!(epoch_ms_to_iso(1_609_459_200_000), "2021-01-01T00:00:00.000Z");
    }

    #[test]
    fn iso_to_epoch_ms_rejects_garbage() {
        assert_eq!(iso_to_epoch_ms("not-a-timestamp"), None);
        assert_eq!(iso_to_epoch_ms("2021-01-01T00:00:00.000"), None); // missing Z
        assert_eq!(iso_to_epoch_ms("2021-13-01T00:00:00.000Z"), None); // bad month
    }

    #[test]
    fn iso_to_epoch_ms_accepts_missing_millis() {
        assert_eq!(iso_to_epoch_ms("2021-01-01T00:00:00Z"), Some(1_609_459_200_000));
    }
}
