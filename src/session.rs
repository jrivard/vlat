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

//! Session save / restart.
//!
//! Every run periodically snapshots its settings and per-target summary stats
//! to a JSON file under the XDG state directory.  Unnamed sessions rotate (the
//! 10 most recent are kept); named sessions persist until deleted.  Saving
//! also deletes any other unnamed session with an identical configuration, so
//! re-running the same targets/settings updates one auto-save in place rather
//! than piling up near-duplicates.
//!
//! Settings are stored as CLI token groups (e.g. `["--interval","1000ms"]`) so
//! restart can splice them into argv ahead of the user's own arguments and
//! re-use the normal parsing pipeline - which is also what makes "CLI flags
//! override the session" fall out naturally: groups whose flag the user passed
//! explicitly are dropped before splicing.
//!
//! Raw probe history is deliberately not persisted: restarting starts probing
//! fresh with the saved settings and targets.  The per-target summary
//! aggregates exist only so the picker's "summary & exit" action can print the
//! same end-of-session summary a quitting run would.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::cli::{Args, ColumnVis, ExtraStat, BaseStat, TlsVersionArg};
use crate::constants::SESSION_MAX_UNNAMED;
use crate::output::{epoch_ms_to_iso, iso_timestamp, iso_to_epoch_ms};
use crate::state::TargetState;

pub const SESSION_VERSION: u32 = 2;

// ── file format ──────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
pub struct SessionFile {
    pub version:           u32,
    pub name:              Option<String>,
    /// ISO 8601 UTC, e.g. "2026-07-03T18:12:54.123Z".
    pub started_at:        String,
    /// ISO 8601 UTC, e.g. "2026-07-03T18:12:54.123Z".
    pub saved_at:          String,
    /// Original target specs, exactly as given on the command line.
    pub targets:           Vec<String>,
    /// Settings as CLI token groups, e.g. [["--interval","1000ms"],["-4"]].
    pub args:              Vec<Vec<String>>,
    /// Runtime column state - not expressible as CLI tokens, applied post-parse.
    pub extra_stats:       Vec<String>,
    pub hidden_base_stats: Vec<String>,
    pub column_vis:        SavedColumnVis,
    pub target_data:       Vec<TargetData>,
}

impl SessionFile {
    /// Parsed `saved_at`, in unix-epoch ms. Falls back to `epoch_ms()` (now) if unparseable.
    pub fn saved_at_ms(&self) -> u64 {
        iso_to_epoch_ms(&self.saved_at).unwrap_or_else(epoch_ms)
    }

    /// Parsed `started_at`, in unix-epoch ms. Falls back to `epoch_ms()` (now) if unparseable.
    pub fn started_at_ms(&self) -> u64 {
        iso_to_epoch_ms(&self.started_at).unwrap_or_else(epoch_ms)
    }
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct SavedColumnVis {
    pub mode:    Option<bool>,
    pub name:    Option<bool>,
    pub port:    Option<bool>,
    pub addr:    Option<bool>,
    pub resolve: Option<bool>,
}

/// Per-target lifetime summary aggregates - just enough to print the
/// end-of-session summary table.  Raw sample history is not persisted.
#[derive(Serialize, Deserialize, Clone)]
pub struct TargetData {
    pub label:              String,
    #[serde(default)]
    pub custom_label:       bool,
    /// Mode label as shown in the UI, e.g. "tcp" or "tcp:8443".
    #[serde(default)]
    pub mode_label:         String,
    /// Exec command for exec targets (empty otherwise).
    #[serde(default)]
    pub exec_cmd:           String,
    /// Last resolved IP address, if any.
    #[serde(default)]
    pub addr:               Option<String>,
    pub total_sent:         u64,
    pub drops:              u32,
    pub dups:               u32,
    pub latency_sum:        f64,
    pub latency_sq_sum:     f64,
    pub jitter_sum:         f64,
    pub jitter_count:       u64,
    pub lifetime_min:       f64,
    pub lifetime_max:       f64,
    pub max_drop_streak:    u32,
    pub ip_changes:         u32,
    pub srtt:               f64,
}

// ── runtime context ──────────────────────────────────────────────────────────

/// Everything app::run needs to save the session.
pub struct SessionCtx {
    pub enabled:       bool,
    pub name:          Option<String>,
    pub path:          PathBuf,
    pub started_at_ms: u64,
}

/// Runtime display state that lives outside `Args` in the app loop.
pub struct RuntimeSettings<'a> {
    pub view: &'a str,
    pub sort: &'a str,
    pub keys: bool,
}

impl SessionCtx {
    /// Fresh (non-restart) context. `name` comes from --session-name.
    pub fn new(enabled: bool, name: Option<String>) -> Self {
        let started_at_ms = epoch_ms();
        let path = match &name {
            Some(n) => named_session_path(n),
            None    => sessions_dir().join(format!("auto-{}.json", filename_stamp(started_at_ms))),
        };
        SessionCtx { enabled, name, path, started_at_ms }
    }

    /// Context re-running a saved session. A restarted session keeps writing to
    /// its own file (same name, same slot in the rotation) unless the user
    /// renames it with --session-name.  Probing starts fresh, so `started_at`
    /// is now, not the saved session's start time.
    pub fn restarted(enabled: bool, cli_name: Option<String>, path: PathBuf, saved_name: Option<String>) -> Self {
        let name = cli_name.or_else(|| saved_name.clone());
        let path = match (&name, &saved_name) {
            (Some(n), saved) if saved.as_ref() != Some(n) => named_session_path(n),
            _ => path,
        };
        SessionCtx { enabled, name, path, started_at_ms: epoch_ms() }
    }

    /// Snapshot the current run to disk (atomic write) and rotate old
    /// unnamed sessions.  Failures are logged, never fatal.
    pub fn save(&self, args: &Args, rt: &RuntimeSettings, states: &[TargetState], mode_labels: &[String]) {
        if !self.enabled { return; }
        let file = SessionFile {
            version:           SESSION_VERSION,
            name:              self.name.clone(),
            started_at:        epoch_ms_to_iso(self.started_at_ms),
            saved_at:          iso_timestamp(),
            targets:           args.targets.clone(),
            args:              settings_tokens(args, rt),
            extra_stats:       args.extra_stats.iter().map(|s| stat_cli_name(s).to_string()).collect(),
            hidden_base_stats: args.hidden_base_stats.iter().map(|s| base_stat_name(s).to_string()).collect(),
            column_vis:        SavedColumnVis {
                mode:    args.column_vis.mode,
                name:    args.column_vis.name,
                port:    args.column_vis.port,
                addr:    args.column_vis.addr,
                resolve: args.column_vis.resolve,
            },
            target_data:       states.iter().zip(mode_labels.iter())
                                   .map(|(s, m)| target_data_from(s, m))
                                   .collect(),
        };
        if let Err(e) = write_atomic(&self.path, &file) {
            crate::logfile::write(&format!("session: save failed: {}", e));
            return;
        }
        if self.name.is_none() {
            dedupe_unnamed(&self.path, &file);
        }
        rotate_unnamed();
    }
}

// ── locations ────────────────────────────────────────────────────────────────

/// $VLAT_SESSION_DIR, else $XDG_STATE_HOME/vlat/sessions,
/// else ~/.local/state/vlat/sessions.
pub fn sessions_dir() -> PathBuf {
    if let Ok(p) = std::env::var("VLAT_SESSION_DIR") {
        return PathBuf::from(p);
    }
    crate::paths::xdg_state_dir().join("vlat").join("sessions")
}

fn named_session_path(name: &str) -> PathBuf {
    sessions_dir().join(format!("named-{}.json", slug(name)))
}

/// Filename-safe form of a session name.
fn slug(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    while out.contains("--") { out = out.replace("--", "-"); }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "unnamed".to_string() } else { out }
}

pub fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Filesystem-safe ISO 8601 basic-format stamp for unnamed session filenames,
/// e.g. "20260703T181254123Z" (still lexicographically sortable).
fn filename_stamp(ms: u64) -> String {
    epoch_ms_to_iso(ms).replace(['-', ':', '.'], "")
}

// ── save side ────────────────────────────────────────────────────────────────

fn write_atomic(path: &Path, file: &SessionFile) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create '{}': {}", dir.display(), e))?;
    let json = serde_json::to_vec(file).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| format!("write '{}': {}", tmp.display(), e))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename to '{}': {}", path.display(), e))
}

/// True if two saved sessions amount to the same run: same targets and same
/// settings.  Two sessions like this are redundant clutter in the picker -
/// keeping both teaches the user nothing a single entry doesn't.
fn same_config(a: &SessionFile, b: &SessionFile) -> bool {
    a.targets == b.targets
        && a.args == b.args
        && a.extra_stats == b.extra_stats
        && a.hidden_base_stats == b.hidden_base_stats
        && a.column_vis == b.column_vis
}

/// Delete other auto-saved sessions whose configuration matches `current`, so
/// each unique config appears at most once among the automatic saves.  Named
/// sessions are never touched.
fn dedupe_unnamed(current_path: &Path, current: &SessionFile) {
    let Ok(entries) = std::fs::read_dir(sessions_dir()) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        if p == current_path { continue; }
        let is_auto = p.file_name().and_then(|f| f.to_str())
            .is_some_and(|f| f.starts_with("auto-") && f.ends_with(".json"));
        if !is_auto { continue; }
        let Ok(other) = load_session(&p) else { continue };
        if same_config(&other, current) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Keep the SESSION_MAX_UNNAMED newest auto-saved sessions; named ones are exempt.
fn rotate_unnamed() {
    let Ok(entries) = std::fs::read_dir(sessions_dir()) else { return };
    let mut unnamed: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let fname = p.file_name()?.to_str()?;
            if !fname.starts_with("auto-") || !fname.ends_with(".json") { return None; }
            let mtime = e.metadata().ok()?
                .modified().ok()?
                .duration_since(UNIX_EPOCH).ok()?
                .as_millis() as u64;
            Some((mtime, p))
        })
        .collect();
    if unnamed.len() <= SESSION_MAX_UNNAMED { return; }
    unnamed.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
    for (_, p) in unnamed.drain(SESSION_MAX_UNNAMED..) {
        let _ = std::fs::remove_file(p);
    }
}

fn target_data_from(s: &TargetState, mode_label: &str) -> TargetData {
    TargetData {
        label:              s.label.clone(),
        custom_label:       s.custom_label,
        mode_label:         mode_label.to_string(),
        exec_cmd:           s.exec_cmd.clone(),
        addr:               s.current_ip.map(|ip| ip.to_string()),
        total_sent:         s.total_sent,
        drops:              s.drops,
        dups:               s.dups,
        latency_sum:        s.latency_sum,
        latency_sq_sum:     s.latency_sq_sum,
        jitter_sum:         s.jitter_sum,
        jitter_count:       s.jitter_count,
        lifetime_min:       s.lifetime_min,
        lifetime_max:       s.lifetime_max,
        max_drop_streak:    s.max_drop_streak,
        ip_changes:         s.ip_changes,
        srtt:               s.srtt,
    }
}

/// Serialize the current settings as CLI token groups.  Runtime-toggled
/// settings (view/theme/sort/keys/window) reflect their live values, so a
/// restarted session picks up the display state the user left off with, not
/// where they started. Values still at their clap default are omitted, so a
/// restarted/copied command line shows only what the user actually changed.
/// --count is deliberately not saved: restored totals already meet the limit,
/// so a restarted count-run would exit immediately.
fn settings_tokens(args: &Args, rt: &RuntimeSettings) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    macro_rules! flag {
        ($name:literal, $on:expr) => { if $on { out.push(vec![$name.to_string()]) } };
    }
    macro_rules! val {
        ($name:literal, $v:expr) => { out.push(vec![$name.to_string(), $v]) };
    }
    macro_rules! val_ne {
        ($name:literal, $cur:expr, $default:expr, $v:expr) => {
            if $cur != $default { out.push(vec![$name.to_string(), $v]) }
        };
    }
    macro_rules! opt {
        ($name:literal, $v:expr) => { if let Some(v) = $v { out.push(vec![$name.to_string(), v.to_string()]) } };
    }

    val_ne!("--view", rt.view, "single", rt.view.to_string());
    // Theme::colorful() is named "colorful" internally but "default" on the CLI.
    let theme = if args.theme.name == "colorful" { "default".to_string() } else { args.theme.name.to_string() };
    val_ne!("--theme", theme, "default", theme.clone());
    val_ne!("--sort", rt.sort, "none", rt.sort.to_string());
    flag!("--reverse-sort", args.reverse_sort);
    val_ne!("--keys", rt.keys, true, rt.keys.to_string());
    val_ne!("--interval", args.interval, 1000, format!("{}ms", args.interval));
    val_ne!("--timeout", args.timeout, 10.0, format!("{}s", args.timeout));
    val_ne!("--window", args.window, 0, format!("{}s", args.window));
    val_ne!("--graph-interval", args.graph_interval, 1000, format!("{}ms", args.graph_interval));
    val_ne!("--history-rows", args.history_rows, crate::constants::SINGLE_HISTORY_ROWS, args.history_rows.to_string());
    opt!("--span", args.span.map(|s| format!("{}s", s)));
    opt!("--max-range", args.max_range);
    if let Some(m) = &args.mode {
        val!("--mode", format!("{:?}", m).to_lowercase());
    }
    flag!("--ascii", args.ascii);
    flag!("-4", args.ipv4);
    flag!("-6", args.ipv6);
    opt!("--bind", args.bind_addr.as_ref());
    val_ne!("--resolve-interval", args.resolve_interval, 300, format!("{}s", args.resolve_interval));
    opt!("--dns-server", args.dns_server.as_ref());
    flag!("--no-dns-refresh", args.no_dns_refresh);
    val_ne!("--tcp-port", args.tcp_port, crate::constants::DEFAULT_TCP_PORT, args.tcp_port.to_string());
    val_ne!("--udp-port", args.udp_port, crate::constants::DEFAULT_UDP_PORT, args.udp_port.to_string());
    val_ne!("--http-path", args.http_path, "/", args.http_path.clone());
    val_ne!("--dns-query", args.dns_query, crate::constants::DEFAULT_DNS_QUERY, args.dns_query.clone());
    flag!("--tls-no-verify", args.tls_no_verify);
    opt!("--tls-cert", args.tls_cert.as_ref());
    match args.tls_version {
        TlsVersionArg::Any => {}
        TlsVersionArg::V12 => val!("--tls-version", "1.2".to_string()),
        TlsVersionArg::V13 => val!("--tls-version", "1.3".to_string()),
    }
    opt!("--exec-cmd", args.exec_cmd.as_ref());
    opt!("--output", args.output.as_ref());
    if let Some(f) = &args.output_format {
        val!("--output-format", format!("{:?}", f).to_lowercase());
    }
    opt!("--debug-log", args.debug_log.as_ref());
    flag!("--alert", args.alert);
    opt!("--warn-rtt", args.warn_rtt);
    flag!("--no-icmp-warn", args.no_icmp_warn);
    flag!("--allow-elevated-exec", args.allow_elevated_exec);
    out
}

// ── load / restart side ──────────────────────────────────────────────────────

pub fn load_session(path: &Path) -> Result<SessionFile, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read '{}': {}", path.display(), e))?;
    let file: SessionFile = serde_json::from_str(&content)
        .map_err(|e| format!("cannot parse '{}': {}", path.display(), e))?;
    if file.version > SESSION_VERSION {
        return Err(format!(
            "'{}' was saved by a newer vlat (session version {})",
            path.display(), file.version
        ));
    }
    Ok(file)
}

/// All readable sessions, newest first.  Unparseable files are skipped.
pub fn list_sessions() -> Vec<(PathBuf, SessionFile)> {
    let Ok(entries) = std::fs::read_dir(sessions_dir()) else { return Vec::new() };
    let mut out: Vec<(PathBuf, SessionFile)> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") { return None; }
            let s = load_session(&p).ok()?;
            Some((p, s))
        })
        .collect();
    out.sort_by(|a, b| b.1.saved_at_ms().cmp(&a.1.saved_at_ms()));
    out
}

/// Find a saved session by its name (exact match, then slug match).
pub fn find_by_name(name: &str) -> Option<(PathBuf, SessionFile)> {
    let sessions = list_sessions();
    sessions.iter()
        .find(|(_, s)| s.name.as_deref() == Some(name))
        .or_else(|| sessions.iter().find(|(_, s)| s.name.as_deref().map(slug).as_deref() == Some(&slug(name))))
        .cloned()
}

pub fn named_sessions() -> Vec<String> {
    list_sessions().into_iter().filter_map(|(_, s)| s.name).collect()
}

/// Build the argv tokens to splice in ahead of the user's own arguments:
/// saved settings (minus any group whose flag the user passed explicitly)
/// followed by the saved target specs.
pub fn restart_argv(session: &SessionFile, cmd: &clap::Command, user_set: &HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for group in &session.args {
        let Some(first) = group.first() else { continue };
        match token_arg_id(cmd, first) {
            Some(id) if user_set.contains(&id) => continue, // CLI wins
            Some(_) => out.extend(group.iter().cloned()),
            None => continue, // flag unknown to this vlat build - skip
        }
    }
    // Same "CLI wins" rule as every other saved setting: if the user typed
    // their own target(s) alongside --restart, those replace the saved list
    // rather than merging with it (clap would otherwise just concatenate
    // the two positional lists into one).
    if !user_set.contains("targets") {
        out.extend(session.targets.iter().cloned());
    }
    out
}

fn token_arg_id(cmd: &clap::Command, token: &str) -> Option<String> {
    if let Some(long) = token.strip_prefix("--") {
        cmd.get_arguments()
            .find(|a| a.get_long() == Some(long))
            .map(|a| a.get_id().to_string())
    } else if let Some(short) = token.strip_prefix('-').and_then(|s| s.chars().next()) {
        cmd.get_arguments()
            .find(|a| a.get_short() == Some(short))
            .map(|a| a.get_id().to_string())
    } else {
        None
    }
}

/// Apply the saved column state after arg parsing.  Skipped entirely when the
/// user passed --columns explicitly (their choice wins).
pub fn apply_saved_columns(args: &mut Args, session: &SessionFile, user_set_columns: bool) {
    if user_set_columns { return; }
    args.extra_stats = session.extra_stats.iter().filter_map(|n| stat_from_name(n)).collect();
    args.hidden_base_stats = session.hidden_base_stats.iter().filter_map(|n| base_stat_from_name(n)).collect();
    args.column_vis = ColumnVis {
        mode:    session.column_vis.mode,
        name:    session.column_vis.name,
        port:    session.column_vis.port,
        addr:    session.column_vis.addr,
        resolve: session.column_vis.resolve,
    };
}

// ── session picker (run when vlat starts with no targets) ───────────────────

/// Compact age string: "3m ago", "5h ago", "2d ago".
fn age_str(saved_at_ms: u64) -> String {
    let secs = epoch_ms().saturating_sub(saved_at_ms) / 1000;
    if secs < 60          { format!("{}s ago", secs) }
    else if secs < 3_600  { format!("{}m ago", secs / 60) }
    else if secs < 86_400 { format!("{}h ago", secs / 3_600) }
    else                  { format!("{}d ago", secs / 86_400) }
}

/// What the user picked a session for.
pub enum PickerAction {
    /// Re-run the session (saved settings + targets, fresh probe data).
    Restart,
    /// Print the end-of-session summary and exit.
    Summary,
}

/// The full CLI invocation equivalent to this saved session.
pub fn restart_command(s: &SessionFile) -> String {
    let mut tokens: Vec<String> = vec!["vlat".to_string()];
    for group in &s.args { tokens.extend(group.iter().cloned()); }
    tokens.extend(s.targets.iter().cloned());
    tokens.iter().map(|t| shell_quote(t)).collect::<Vec<_>>().join(" ")
}

/// Quote a token for a POSIX shell if it contains anything unsafe.
fn shell_quote(t: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c);
    if !t.is_empty() && t.chars().all(safe) {
        t.to_string()
    } else {
        format!("'{}'", t.replace('\'', "'\\''"))
    }
}

/// Copy `text` to the system clipboard via the OSC 52 escape sequence.
/// Supported by most modern terminals (xterm, kitty, WezTerm, iTerm2,
/// Windows Terminal, tmux with set-clipboard); silently ignored elsewhere.
fn copy_to_clipboard(text: &str) -> io::Result<()> {
    use std::io::Write;
    let mut out = io::stdout();
    write!(out, "{}", clipboard_osc52(text))?;
    out.flush()
}

/// Build the OSC 52 escape sequence that sets the system clipboard to `text`.
fn clipboard_osc52(text: &str) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    format!("\x1b]52;c;{}\x07", STANDARD.encode(text.as_bytes()))
}

/// Rename a saved session in place: rewrite it under the new name and remove
/// the old file if the filename changed.  Updates `entry` on success.
fn rename_session(entry: &mut (PathBuf, SessionFile), new_name: &str) -> Result<(), String> {
    let (path, sess) = entry;
    let new_path = named_session_path(new_name);
    if new_path != *path && new_path.exists() {
        return Err(format!("a session named '{}' already exists", new_name));
    }
    sess.name = Some(new_name.to_string());
    write_atomic(&new_path, sess)?;
    if new_path != *path {
        let _ = std::fs::remove_file(&path);
        *path = new_path;
    }
    Ok(())
}

/// One row of the picker's session list: a section divider, a blank spacer,
/// or a session at the given index into the picker's `sessions` vec.
enum PickerRow { Header(&'static str), Blank, Item(usize) }

/// Lay `sessions` out for display: named sessions first (in their existing
/// recency order), then unnamed ones, with a labeled divider - and a blank
/// spacer line for extra visual breathing room - between the two groups.
/// When every session is the same kind, there's nothing to separate, so it's
/// returned as a single flat list with no headers.
fn picker_rows(sessions: &[(PathBuf, SessionFile)]) -> Vec<PickerRow> {
    let named:   Vec<usize> = (0..sessions.len()).filter(|&i| sessions[i].1.name.is_some()).collect();
    let unnamed: Vec<usize> = (0..sessions.len()).filter(|&i| sessions[i].1.name.is_none()).collect();
    let mut rows = Vec::with_capacity(sessions.len() + 3);
    if !named.is_empty() && !unnamed.is_empty() {
        rows.push(PickerRow::Header("named sessions"));
        rows.extend(named.into_iter().map(PickerRow::Item));
        rows.push(PickerRow::Blank);
        rows.push(PickerRow::Header("unnamed sessions"));
        rows.extend(unnamed.into_iter().map(PickerRow::Item));
    } else {
        rows.extend((0..sessions.len()).map(PickerRow::Item));
    }
    rows
}

/// Interactive session picker on an inline ratatui viewport.
/// Returns the chosen session and action, or None if the user quit (or
/// nothing is left).
///
/// Enter/Space on a row opens a details dialog (CLI summary, condensed
/// per-target stats, and a selectable restart/summary/copy/rename/delete/back
/// menu - styled after the main app's help dialog). ↑↓/j/k move the menu
/// selection and Enter activates it; the underlying hotkeys (`s`/`c`/`r`/`d`/
/// `Esc`) still work directly too, both on the list and from inside the
/// details dialog. `r`/`d` each open their own small confirm dialog rather
/// than editing inline.
pub fn run_picker(theme_name: crate::cli::ThemeName) -> io::Result<Option<(PathBuf, SessionFile, PickerAction)>> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use crossterm::terminal;
    use ratatui::backend::CrosstermBackend;
    use ratatui::layout::{Alignment, Rect};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};
    use ratatui::{Terminal, TerminalOptions, Viewport};
    use std::time::Instant;
    use crate::cli::ThemeName;
    use crate::ui::dialogs::centered_rect;

    // Live theme state - Shift+T cycles it while the picker is open, purely
    // for how the picker itself renders (the eventual monitor run resolves
    // its own theme independently from --theme, after restart).
    let mut theme_name = theme_name;
    let mut theme = theme_name.to_theme();

    const RENAME_DIALOG_H: u16 = 7;
    const DELETE_DIALOG_H: u16 = 7;
    const HELP_DIALOG_H: u16 = 13;
    const UNNAMED: &str = "(unnamed)";

    let mut sessions = list_sessions();
    if sessions.is_empty() { return Ok(None); }

    const MAX_ROWS: usize = 10;
    const FLASH_SECS: u64 = 3;
    let rows = picker_rows(&sessions).len().min(MAX_ROWS);
    // header + scroll-up hint + rows + scroll-down hint + command line + border/title + footer
    let mut viewport_h = (rows + 6) as u16;

    let (term_w, _) = terminal::size()?;
    let dialog_w: u16 = 74.min(term_w.saturating_sub(4)).max(20);

    terminal::enable_raw_mode()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::with_options(backend, TerminalOptions {
        viewport: Viewport::Inline(viewport_h),
    })?;

    // Grow the inline viewport to fit `needed` rows. Never shrinks back -
    // same convention as `ensure_dialog_space!` in app.rs.
    macro_rules! ensure_space {
        ($needed:expr) => {
            let needed: u16 = $needed;
            if needed > viewport_h {
                let current_h = viewport_h;
                viewport_h = needed;
                drop(term);
                if current_h > 0 {
                    use std::io::Write;
                    let mut out = io::stdout();
                    write!(out, "\x1b[{}A\x1b[J", current_h)?;
                    out.flush()?;
                }
                let backend = CrosstermBackend::new(io::stdout());
                term = Terminal::with_options(backend, TerminalOptions {
                    viewport: Viewport::Inline(needed),
                })?;
            }
        }
    }

    // Default to the most recent named session, if one exists - `sessions` is
    // already newest-first, so that's just the first named entry. Falls back
    // to the most recent session overall (index 0) when there are no named
    // sessions at all.
    let mut selected: usize = sessions.iter().position(|(_, s)| s.name.is_some()).unwrap_or(0);
    let mut scroll:   usize = 0;
    let mut chosen:   Option<(PathBuf, SessionFile, PickerAction)> = None;
    // Details dialog open for `selected` (CLI summary + stats + action menu).
    let mut detail:   bool = false;
    // Index of the highlighted row in the details dialog's action menu.
    let mut detail_sel: usize = 0;
    // Some(buffer) while the dedicated rename dialog is open.
    let mut rename:   Option<String> = None;
    // True while the dedicated delete-confirm dialog is open.
    let mut confirm_delete: bool = false;
    // True while the '?' help overlay is open.
    let mut help: bool = false;
    // Transient status line: (message, is_error, shown_at).
    let mut flash:    Option<(String, bool, Instant)> = None;

    loop {
        if sessions.is_empty() { break; }
        selected = selected.min(sessions.len() - 1);
        // `scroll`/`rows` index into the display list (`prows`), not directly
        // into `sessions` - section headers occupy rows too. Recomputed every
        // iteration since renaming a session can move it between the named
        // and unnamed groups, changing whether headers are shown at all.
        let prows = picker_rows(&sessions);
        let rows = prows.len().min(MAX_ROWS);
        ensure_space!((rows as u16) + 6);
        let sel_row = prows.iter().position(|r| matches!(r, PickerRow::Item(i) if *i == selected)).unwrap_or(0);
        if sel_row < scroll { scroll = sel_row; }
        if sel_row >= scroll + rows { scroll = sel_row + 1 - rows; }
        if let Some((_, _, at)) = &flash {
            if at.elapsed().as_secs() >= FLASH_SECS { flash = None; }
        }

        // Precompute the details dialog body so its height is known before
        // drawing - the viewport must be grown (if needed) before term.draw.
        let detail_body: Option<Vec<Line<'static>>> = if detail {
            Some(build_detail_lines(&sessions[selected].1, dialog_w as usize, flash.as_ref(), detail_sel, &theme))
        } else { None };

        // +2 extra to leave room for the outer list block's own border, since
        // overlays are centered within its `inner` rect, not the full frame.
        if help {
            ensure_space!(HELP_DIALOG_H + 2);
        } else if let Some(body) = &detail_body {
            ensure_space!(body.len() as u16 + 2 + 2);
        } else if rename.is_some() {
            ensure_space!(RENAME_DIALOG_H + 2);
        } else if confirm_delete {
            ensure_space!(DELETE_DIALOG_H + 2);
        }

        term.draw(|f| {
            let area = f.area();
            let key = |s: &str| Span::styled(s.to_string(), Style::default().fg(theme.dlg_help_key));
            let lbl = |s: &str| Span::styled(s.to_string(), Style::default().fg(theme.c(theme.dlg_help_label)));
            let footer = vec![
                key(" ↑↓"),         lbl(" select  "),
                key("Enter/Space"), lbl(" details  "),
                key("s"),           lbl(" summary  "),
                key("c"),           lbl(" copy cmd  "),
                key("r"),           lbl(" rename  "),
                key("d"),           lbl(" delete  "),
                key("?"),           lbl(" help  "),
                key("q"),           lbl(" quit "),
            ];
            let block = Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(" vlat - saved sessions ",
                    Style::default().fg(theme.dlg_help_title).add_modifier(Modifier::BOLD)))
                .title_bottom(Line::from(footer));
            let inner = block.inner(area);
            f.render_widget(block, area);

            let any_named = sessions.iter().any(|(_, s)| s.name.is_some());
            let name_w = if any_named {
                sessions.iter()
                    .map(|(_, s)| s.name.as_ref().map(|n| n.chars().count()).unwrap_or(UNNAMED.chars().count()))
                    .max()
                    .unwrap_or(0)
                    .min(20)
            } else { 0 };

            // Selected-row bar: same black-on-accent convention as the
            // theme/sort pickers elsewhere in the app.
            let sel_fg = Style::default().fg(Color::Black).add_modifier(Modifier::BOLD);
            let dim    = Style::default().fg(theme.c(theme.dlg_help_label));
            let faint  = dim.add_modifier(Modifier::DIM);

            // Column header - the row format below otherwise has to be
            // learned by inference.
            let mut header_spans = vec![Span::raw("  ")];
            if name_w > 0 {
                header_spans.push(Span::styled(format!("{:<w$}", "name", w = name_w + 2), faint));
            }
            header_spans.push(Span::styled(format!("{:<18}", "date"), faint));
            header_spans.push(Span::styled(format!("{:<11}", "age"), faint));
            header_spans.push(Span::styled("targets", faint));
            f.render_widget(Paragraph::new(Line::from(header_spans)), Rect::new(inner.x, inner.y, inner.width, 1));

            // Scroll hints - reserved lines above/below the visible window
            // rather than squeezed onto the header/footer, so the list
            // doesn't jump size as you scroll through more than `rows` saved
            // sessions; blank when there's nothing more in that direction.
            let more_above = scroll;
            let more_below = prows.len().saturating_sub(scroll + rows);
            let scroll_up_line = if more_above > 0 {
                Line::from(Span::styled(format!("  ↑ {} more above", more_above), faint))
            } else { Line::raw("") };
            let scroll_down_line = if more_below > 0 {
                Line::from(Span::styled(format!("  ↓ {} more below", more_below), faint))
            } else { Line::raw("") };
            f.render_widget(Paragraph::new(scroll_up_line), Rect::new(inner.x, inner.y + 1, inner.width, 1));

            let rows_y = inner.y + 2;
            for (row, prow) in (scroll..(scroll + rows).min(prows.len())).enumerate() {
                let line_area = Rect::new(inner.x, rows_y + row as u16, inner.width, 1);
                let idx = match &prows[prow] {
                    PickerRow::Blank => continue,
                    PickerRow::Header(label) => {
                        // Named and unnamed get visually distinct headers (accent
                        // bold vs. dim), and the divider runs the full row width
                        // rather than a short "-- label --" tag, so the two
                        // groups read as clearly separate blocks, not just a
                        // labeled line among the rows.
                        let style = if *label == "named sessions" {
                            Style::default().fg(theme.dlg_help_title).add_modifier(Modifier::BOLD)
                        } else {
                            dim
                        };
                        let prefix = format!("  \u{2500}\u{2500} {} ", label);
                        let fill = (inner.width as usize).saturating_sub(prefix.chars().count());
                        let text = format!("{}{}", prefix, "\u{2500}".repeat(fill));
                        f.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), line_area);
                        continue;
                    }
                    PickerRow::Item(idx) => *idx,
                };
                let (_, s) = &sessions[idx];
                let is_sel = idx == selected;
                let marker = if is_sel { "▸ " } else { "  " };
                let saved_at_ms = s.saved_at_ms();
                let date = crate::logfile::format_epoch_secs(saved_at_ms / 1000);
                let date = date.trim_end_matches('Z');
                let date = &date[..date.len().saturating_sub(3)]; // drop :SS
                let mut spans = vec![
                    Span::styled(marker.to_string(),
                        if is_sel { sel_fg } else { Style::default().fg(Color::Reset) }),
                ];
                if name_w > 0 {
                    let (name_disp, name_style) = match &s.name {
                        Some(n) => (n.clone(), if is_sel { sel_fg } else { Style::default().fg(theme.dlg_help_title) }),
                        None    => (UNNAMED.to_string(), if is_sel { sel_fg } else { dim }),
                    };
                    spans.push(Span::styled(format!("{:<w$}  ", name_disp, w = name_w), name_style));
                }
                spans.push(Span::styled(format!("{}  ", date), if is_sel { sel_fg } else { dim }));
                spans.push(Span::styled(format!("{:>9}  ", age_str(saved_at_ms)), if is_sel { sel_fg } else { dim }));
                let used: usize = spans.iter().map(|sp| sp.content.chars().count()).sum();
                let avail = (inner.width as usize).saturating_sub(used);
                let mut targets = s.targets.join("  ");
                if targets.chars().count() > avail {
                    targets = targets.chars().take(avail.saturating_sub(1)).collect::<String>() + "…";
                }
                let targets_w = targets.chars().count();
                spans.push(Span::styled(targets, if is_sel { sel_fg } else { Style::default().fg(Color::Gray) }));

                let line = if is_sel {
                    // Pad to the full row width so the bar's background covers
                    // the whole row, not just its text - a real selection bar.
                    let pad = (inner.width as usize).saturating_sub(used + targets_w);
                    spans.push(Span::raw(" ".repeat(pad)));
                    Line::from(spans).style(Style::default().bg(theme.dlg_help_title))
                } else {
                    Line::from(spans)
                };
                f.render_widget(Paragraph::new(line), line_area);
            }
            f.render_widget(Paragraph::new(scroll_down_line), Rect::new(inner.x, rows_y + rows as u16, inner.width, 1));

            // Bottom line: transient status, or the selected session's full CLI command.
            let bottom = Rect::new(inner.x, rows_y + rows as u16 + 1, inner.width, 1);
            let line = if let Some((msg, is_err, _)) = &flash {
                Span::styled(format!(" {}", msg),
                    Style::default().fg(if *is_err { Color::Red } else { Color::Green })).into()
            } else {
                let cmd = restart_command(&sessions[selected].1);
                let avail = (inner.width as usize).saturating_sub(3);
                let cmd = if cmd.chars().count() > avail {
                    cmd.chars().take(avail.saturating_sub(1)).collect::<String>() + "…"
                } else { cmd };
                Line::from(vec![
                    Span::styled(" $ ", dim),
                    Span::styled(cmd, dim),
                ])
            };
            f.render_widget(Paragraph::new(line), bottom);

            // ── Overlays ─────────────────────────────────────────────────
            // Rename/delete take priority when stacked on top of an open
            // details dialog (`detail` stays true underneath so it reappears
            // once the rename/delete overlay closes).
            if help {
                let dialog_area = centered_rect(54.min(dialog_w), HELP_DIALOG_H, inner);
                f.render_widget(Clear, dialog_area);
                f.render_widget(
                    Paragraph::new(build_help_lines(&theme))
                        .block(Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(theme.dlg_help_title))
                            .title(Span::styled(" help ",
                                Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD)))),
                    dialog_area,
                );
            } else if let Some(buf) = &rename {
                let hint = dim;
                let body = vec![
                    Line::raw(""),
                    Line::from(Span::styled("  Enter save   Esc cancel", hint)),
                    Line::raw(""),
                    Line::from(vec![
                        Span::styled("  name: ", hint),
                        Span::styled(buf.clone(), Style::default().fg(theme.dlg_help_key)),
                        Span::styled("▏", hint),
                    ]),
                    Line::raw(""),
                ];
                let dialog_area = centered_rect(54.min(dialog_w), RENAME_DIALOG_H, inner);
                f.render_widget(Clear, dialog_area);
                f.render_widget(
                    Paragraph::new(body)
                        .block(Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(theme.dlg_help_title))
                            .title(Span::styled(" rename session ",
                                Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD)))),
                    dialog_area,
                );
            } else if confirm_delete {
                let (_, s) = &sessions[selected];
                let label = s.name.clone().unwrap_or_else(|| s.targets.join("  "));
                let hint = dim;
                let body = vec![
                    Line::raw(""),
                    Line::from(vec![
                        Span::styled("  delete '", Style::default().fg(theme.dlg_warning)),
                        Span::styled(trunc_label(&label, 40), Style::default().fg(theme.dlg_help_key).add_modifier(Modifier::BOLD)),
                        Span::styled("'?", Style::default().fg(theme.dlg_warning)),
                    ]),
                    Line::raw(""),
                    Line::from(Span::styled("  y confirm   n/Esc cancel", hint)),
                    Line::raw(""),
                ];
                let dialog_area = centered_rect(54.min(dialog_w), DELETE_DIALOG_H, inner);
                f.render_widget(Clear, dialog_area);
                f.render_widget(
                    Paragraph::new(body)
                        .block(Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(theme.dlg_warning))
                            .title(Span::styled(" delete session ",
                                Style::default().fg(Color::Black).bg(theme.dlg_warning).add_modifier(Modifier::BOLD)))),
                    dialog_area,
                );
            } else if let Some(body) = &detail_body {
                let title_txt = sessions[selected].1.name.clone()
                    .unwrap_or_else(|| sessions[selected].1.targets.join(", "));
                let dialog_area = centered_rect(dialog_w, body.len() as u16 + 2, inner);
                f.render_widget(Clear, dialog_area);
                f.render_widget(
                    Paragraph::new(body.clone())
                        .block(Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(theme.dlg_help_title))
                            .title(Span::styled(format!(" {} ", trunc_label(&title_txt, 40)),
                                Style::default().fg(Color::Black).bg(theme.dlg_help_title).add_modifier(Modifier::BOLD))))
                        .alignment(Alignment::Left),
                    dialog_area,
                );
            }
        })?;

        // Poll so a transient status message expires without a keypress.
        if !event::poll(std::time::Duration::from_millis(250))? { continue; }
        match event::read()? {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if let Some(buf) = &mut rename {
                    match k.code {
                        KeyCode::Enter => {
                            let new_name = buf.trim().to_string();
                            rename = None;
                            if !new_name.is_empty() {
                                flash = Some(match rename_session(&mut sessions[selected], &new_name) {
                                    Ok(())   => (format!("renamed to '{}'", new_name), false, Instant::now()),
                                    Err(msg) => (msg, true, Instant::now()),
                                });
                            }
                        }
                        KeyCode::Esc => rename = None,
                        KeyCode::Backspace => { buf.pop(); }
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => rename = None,
                        KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => buf.push(c),
                        _ => {}
                    }
                    continue;
                }
                // Shift-T cycles the picker's own display theme, from any of
                // its dialogs - the rename text box is the only exclusion,
                // since 'T' must stay typeable there (handled above already).
                if k.code == KeyCode::Char('T') {
                    theme_name = ThemeName::cycle_next(theme.name);
                    theme = theme_name.to_theme();
                    continue;
                }
                if help {
                    match k.code {
                        KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Esc => help = false,
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => help = false,
                        _ => {}
                    }
                    continue;
                }
                if confirm_delete {
                    match k.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            let (path, _) = &sessions[selected];
                            let _ = std::fs::remove_file(path);
                            sessions.remove(selected);
                            confirm_delete = false;
                            detail = false;
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => confirm_delete = false,
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => confirm_delete = false,
                        _ => {}
                    }
                    continue;
                }
                // Selecting a details-menu row and pressing Enter re-dispatches
                // as that row's own hotkey, so both paths share one
                // implementation. detail_sel 0 (restart) needs no remap - Enter
                // already means restart.
                let code = if detail && k.code == KeyCode::Enter {
                    match detail_sel {
                        1 => KeyCode::Char('s'),
                        2 => KeyCode::Char('c'),
                        3 => KeyCode::Char('r'),
                        4 => KeyCode::Char('d'),
                        5 => KeyCode::Esc,
                        _ => KeyCode::Enter,
                    }
                } else {
                    k.code
                };
                match code {
                    KeyCode::Up   | KeyCode::Char('k') if detail => { detail_sel = detail_sel.saturating_sub(1); }
                    KeyCode::Down | KeyCode::Char('j') if detail => {
                        if detail_sel + 1 < DETAIL_MENU.len() { detail_sel += 1; }
                    }
                    // Step to the previous/next session in display order,
                    // skipping over section headers (they're not selectable).
                    KeyCode::Up   | KeyCode::Char('k') if !detail => {
                        if sel_row > 0 {
                            if let Some(PickerRow::Item(i)) = prows[..sel_row].iter().rev()
                                .find(|r| matches!(r, PickerRow::Item(_))) { selected = *i; }
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') if !detail => {
                        if let Some(PickerRow::Item(i)) = prows[sel_row + 1..].iter()
                            .find(|r| matches!(r, PickerRow::Item(_))) { selected = *i; }
                    }
                    KeyCode::Enter if detail => {
                        let (path, sess) = sessions[selected].clone();
                        chosen = Some((path, sess, PickerAction::Restart));
                        break;
                    }
                    KeyCode::Enter | KeyCode::Char(' ') if !detail => { detail = true; detail_sel = 0; }
                    KeyCode::Char('s') => {
                        let (path, sess) = sessions[selected].clone();
                        chosen = Some((path, sess, PickerAction::Summary));
                        break;
                    }
                    KeyCode::Char('r') => {
                        rename = Some(sessions[selected].1.name.clone().unwrap_or_default());
                    }
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Char('c') => {
                        let ok = copy_to_clipboard(&restart_command(&sessions[selected].1)).is_ok();
                        flash = Some(if ok {
                            ("command copied to clipboard (OSC 52)".to_string(), false, Instant::now())
                        } else {
                            ("clipboard copy failed".to_string(), true, Instant::now())
                        });
                    }
                    KeyCode::Char('d') | KeyCode::Delete => { confirm_delete = true; }
                    KeyCode::Char('?') => { help = true; }
                    KeyCode::Char('q') | KeyCode::Esc if detail => { detail = false; }
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // If the loop above broke because the last session was deleted from
    // within the picker (the only way `sessions` ends up empty here, since
    // an initially-empty list returns before the loop starts), say so -
    // otherwise the picker just vanishes with no explanation.
    let emptied_by_delete = sessions.is_empty();

    // Clear the picker viewport so the app (or shell prompt) starts clean.
    term.clear()?;
    drop(term);
    terminal::disable_raw_mode()?;
    if emptied_by_delete {
        eprintln!("vlat: no saved sessions remain");
    }
    Ok(chosen)
}

/// Truncate `s` to at most `w` chars, appending an ellipsis when cut.
fn trunc_label(s: &str, w: usize) -> String {
    if s.chars().count() <= w { s.to_string() } else { s.chars().take(w.saturating_sub(1)).collect::<String>() + "…" }
}

/// Greedy word-wrap of `s` to `width` columns. Always returns at least one
/// (possibly empty) line.
fn wrap_text(s: &str, width: usize) -> Vec<String> {
    if width == 0 { return vec![s.to_string()]; }
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in s.split(' ') {
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur.chars().count() + 1 + word.chars().count() <= width {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
        }
    }
    lines.push(cur);
    lines
}

/// One condensed stats line for a saved target: avg RTT, loss %, avg jitter, samples sent.
fn condensed_stat_line(d: &TargetData) -> String {
    let avg    = if d.total_sent > 0 { d.latency_sum / d.total_sent as f64 } else { 0.0 };
    let loss   = if d.total_sent > 0 { d.drops as f64 / d.total_sent as f64 * 100.0 } else { 0.0 };
    let jitter = if d.jitter_count > 0 { d.jitter_sum / d.jitter_count as f64 } else { 0.0 };
    format!("{:<16} avg {:>7.1}ms  loss {:>5.1}%  jitter {:>6.2}ms  n {}",
        trunc_label(&d.label, 16), avg, loss, jitter, d.total_sent)
}

/// Builds the full-key-reference body for the '?' help overlay.
fn build_help_lines(theme: &crate::ui::Theme) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};

    let key   = Style::default().fg(theme.dlg_help_key).add_modifier(Modifier::BOLD);
    let label = Style::default().fg(theme.c(theme.dlg_help_label));

    let entries: &[(&str, &str)] = &[
        ("↑/↓, j/k",    "move selection"),
        ("Enter/Space",  "open session details"),
        ("s",            "print summary & exit"),
        ("c",            "copy restart command to clipboard"),
        ("r",            "rename session"),
        ("d, Delete",    "delete session (confirmation required)"),
        ("Shift+T",      "cycle theme"),
        ("q, Esc",       "quit / close dialog"),
        ("Ctrl+C",       "quit immediately"),
    ];
    let mut body: Vec<Line<'static>> = vec![Line::raw("")];
    for (k, desc) in entries {
        body.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{:<13}", k), key),
            Span::styled(*desc, label),
        ]));
    }
    body.push(Line::raw(""));
    body
}

/// The details dialog's action menu: (hotkey label, description). Order
/// fixes the on-screen row order and the arrow-key selection index used in
/// `run_picker`.
const DETAIL_MENU: &[(&str, &str)] = &[
    ("Enter", "restart"),
    ("s",     "summary & exit"),
    ("c",     "copy cmd"),
    ("r",     "rename"),
    ("d",     "delete"),
    ("Esc",   "back"),
];

/// Builds the details dialog body: CLI summary, condensed per-target stats,
/// and the selectable action menu (styled after the main app's help dialog).
/// `sel` is the currently highlighted menu row.
fn build_detail_lines(sess: &SessionFile, dialog_w: usize, flash: Option<&(String, bool, std::time::Instant)>, sel: usize, theme: &crate::ui::Theme) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

    const MAX_STAT_ROWS: usize = 8;

    let key   = Style::default().fg(theme.dlg_help_key).add_modifier(Modifier::BOLD);
    let label = Style::default().fg(theme.c(theme.dlg_help_label));
    let hint  = Style::default().fg(theme.c(theme.dlg_help_label)).add_modifier(Modifier::DIM);

    // Section divider - same "── label ──" convention the sort/theme pickers
    // use elsewhere in the app (see ui/dialogs.rs SORT_GROUPS), so the dense
    // cli/stats/menu blocks read as distinct sections instead of one wall of
    // text.
    let sep = |label: &str| format!(" \u{2500}\u{2500} {} ", label);

    let mut body: Vec<Line<'static>> = vec![Line::raw("")];

    // CLI summary
    body.push(Line::from(Span::styled(sep("cli"), hint)));
    let cmd = restart_command(sess);
    let wrap_w = dialog_w.saturating_sub(4).max(10);
    let cmd_lines = wrap_text(&cmd, wrap_w);
    for line in &cmd_lines {
        body.push(Line::from(Span::styled(format!("  {}", line), key)));
    }
    body.push(Line::raw(""));

    // Stats summary
    body.push(Line::from(Span::styled(sep("stats"), hint)));
    if sess.target_data.is_empty() {
        body.push(Line::from(Span::styled("  no recorded probe data", hint)));
    } else {
        for d in sess.target_data.iter().take(MAX_STAT_ROWS) {
            body.push(Line::from(Span::styled(format!("  {}", condensed_stat_line(d)), label)));
        }
        if sess.target_data.len() > MAX_STAT_ROWS {
            body.push(Line::from(Span::styled(
                format!("  ... +{} more (full table: 's')", sess.target_data.len() - MAX_STAT_ROWS), hint)));
        }
    }
    body.push(Line::raw(""));

    // Menu - full-row selection bar padded to the dialog's content width, the
    // same convention the theme/sort pickers use elsewhere in the app (see
    // ui/dialogs.rs), extended so the bar covers the whole row like the main
    // session list above rather than stopping at the text.
    body.push(Line::from(Span::styled(sep("actions"), hint)));
    let sel_fg = Style::default().fg(Color::Black).add_modifier(Modifier::BOLD);
    let sel_bg = Style::default().bg(theme.dlg_help_title);
    let content_w = dialog_w.saturating_sub(2); // account for the dialog's own border
    for (i, (k, desc)) in DETAIL_MENU.iter().enumerate() {
        if i == sel {
            let text = format!("▸ {:<6}{}", k, desc);
            let pad = content_w.saturating_sub(text.chars().count());
            body.push(Line::from(vec![
                Span::styled(text, sel_fg),
                Span::raw(" ".repeat(pad)),
            ]).style(sel_bg));
        } else {
            body.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{:<6}", k), key),
                Span::styled(*desc, label),
            ]));
        }
    }

    if let Some((msg, is_err, _)) = flash {
        body.push(Line::raw(""));
        body.push(Line::from(Span::styled(format!(" {}", msg),
            Style::default().fg(if *is_err { Color::Red } else { Color::Green }))));
    }

    body
}

// ── column-name mapping ──────────────────────────────────────────────────────

fn stat_cli_name(stat: &ExtraStat) -> &'static str {
    match stat {
        ExtraStat::Mtr    => "mtr",    ExtraStat::Std    => "std",
        ExtraStat::P01    => "p01",    ExtraStat::P10    => "p10",
        ExtraStat::P50    => "p50",    ExtraStat::P95    => "p95",
        ExtraStat::P99    => "p99",    ExtraStat::Cv     => "cv",
        ExtraStat::Srtt   => "srtt",   ExtraStat::Streak => "streak",
        ExtraStat::Last   => "last",
        ExtraStat::Recent => "recent", ExtraStat::Bar    => "bar",
        _ => "?",
    }
}

fn stat_from_name(name: &str) -> Option<ExtraStat> {
    Some(match name {
        "mtr" => ExtraStat::Mtr,       "std" => ExtraStat::Std,
        "p01" => ExtraStat::P01,       "p10" => ExtraStat::P10,
        "p50" => ExtraStat::P50,       "p95" => ExtraStat::P95,
        "p99" => ExtraStat::P99,       "cv"  => ExtraStat::Cv,
        "srtt" => ExtraStat::Srtt,     "streak" => ExtraStat::Streak,
        "last" => ExtraStat::Last,
        "recent" => ExtraStat::Recent, "bar" => ExtraStat::Bar,
        _ => return None,
    })
}

fn base_stat_name(stat: &BaseStat) -> &'static str {
    match stat {
        BaseStat::Avg    => "avg",
        BaseStat::Range  => "range",
        BaseStat::Jitter => "jitter",
        BaseStat::Drops  => "drops",
    }
}

fn base_stat_from_name(name: &str) -> Option<BaseStat> {
    Some(match name {
        "avg"    => BaseStat::Avg,
        "range"  => BaseStat::Range,
        "jitter" => BaseStat::Jitter,
        "drops"  => BaseStat::Drops,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All-default settings should produce no CLI tokens at all - only
    /// deviations from clap's defaults belong in a restarted/copied command.
    #[test]
    fn settings_tokens_omits_defaults() {
        use clap::Parser;
        let args = Args::parse_from(["vlat", "example.net"]);
        let rt = RuntimeSettings { view: "single", sort: "none", keys: true };
        assert!(settings_tokens(&args, &rt).is_empty());
    }

    /// Non-default values are still emitted, defaults among them are not.
    #[test]
    fn settings_tokens_includes_only_changed_values() {
        use clap::Parser;
        let args = Args::parse_from(["vlat", "--tcp-port", "8080", "example.net"]);
        let rt = RuntimeSettings { view: "graph", sort: "none", keys: true };
        let tokens = settings_tokens(&args, &rt);
        assert!(tokens.contains(&vec!["--view".to_string(), "graph".to_string()]));
        assert!(tokens.contains(&vec!["--tcp-port".to_string(), "8080".to_string()]));
        assert!(!tokens.iter().any(|g| g[0] == "--sort"));
        assert!(!tokens.iter().any(|g| g[0] == "--udp-port"));
        assert!(!tokens.iter().any(|g| g[0] == "--keys"));
    }

    /// Re-running the same targets/settings must collapse to a single
    /// auto-save instead of piling up one file per run.
    #[test]
    fn save_dedupes_matching_unnamed_config() {
        use clap::Parser;
        use std::sync::Mutex;
        // VLAT_SESSION_DIR is process-global; keep this test's mutation of it
        // from racing other tests that might read it.
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap();

        let dir = std::env::temp_dir().join(format!("vlat-test-dedupe-{}", epoch_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("VLAT_SESSION_DIR", &dir); }

        let args = Args::parse_from(["vlat", "example.net"]);
        let rt = RuntimeSettings { view: "list", sort: "none", keys: false };

        let ctx1 = SessionCtx::new(true, None);
        ctx1.save(&args, &rt, &[], &[]);

        std::thread::sleep(std::time::Duration::from_millis(5));
        let ctx2 = SessionCtx::new(true, None);
        assert_ne!(ctx1.path, ctx2.path, "test setup must produce two distinct auto-save paths");
        ctx2.save(&args, &rt, &[], &[]);

        let autos: Vec<_> = std::fs::read_dir(&dir).unwrap()
            .flatten()
            .filter(|e| e.file_name().to_str().is_some_and(|f| f.starts_with("auto-")))
            .collect();
        assert_eq!(autos.len(), 1, "identical config should collapse to a single auto-save");
        assert_eq!(autos[0].path(), ctx2.path, "the surviving file should be the most recent save");

        unsafe { std::env::remove_var("VLAT_SESSION_DIR"); }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Restarting with targets typed on the CLI must use only those targets -
    /// clap concatenates positional lists, so without this rule the saved
    /// targets would silently merge into whatever the user just typed.
    #[test]
    fn restart_argv_lets_cli_targets_replace_saved_ones() {
        use clap::CommandFactory;
        let sess = SessionFile {
            version:           SESSION_VERSION,
            name:              None,
            started_at:        epoch_ms_to_iso(1_000),
            saved_at:          epoch_ms_to_iso(2_000),
            targets:           vec!["old-host.example".to_string()],
            args:              vec![],
            extra_stats:       vec![],
            hidden_base_stats: vec![],
            column_vis:        SavedColumnVis::default(),
            target_data:       vec![],
        };
        let cmd = Args::command();

        // No CLI targets typed: the saved targets carry over as before.
        let argv = restart_argv(&sess, &cmd, &HashSet::new());
        assert_eq!(argv, vec!["old-host.example".to_string()]);

        // User typed their own target on the restart command line: it must
        // win outright, not merge with the saved list.
        let mut user_set = HashSet::new();
        user_set.insert("targets".to_string());
        let argv = restart_argv(&sess, &cmd, &user_set);
        assert!(argv.is_empty(), "saved targets must not be re-added when the CLI supplies its own");
    }

    #[test]
    fn slug_basic() {
        assert_eq!(slug("Home Lab"), "home-lab");
        assert_eq!(slug("a//b??c"), "a-b-c");
        assert_eq!(slug("---"), "unnamed");
    }

    fn make_state_with_data() -> TargetState {
        let mut s = TargetState::new("t".to_string());
        for i in 0..5 {
            s.record_sent(i);
            s.record_result(i, Ok(10.0 + i as f64), 86_400, false);
        }
        s.record_sent(5);
        s.record_result(5, Err(()), 86_400, false);
        s.flush_to_graph(100);
        s
    }

    #[test]
    fn session_file_json_roundtrip() {
        let src = make_state_with_data();
        let file = SessionFile {
            version: SESSION_VERSION,
            name: Some("test".to_string()),
            started_at: epoch_ms_to_iso(1_000),
            saved_at: epoch_ms_to_iso(2_000),
            targets: vec!["example.net:tcp:443".to_string()],
            args: vec![vec!["--interval".to_string(), "500ms".to_string()], vec!["-4".to_string()]],
            extra_stats: vec!["recent".to_string(), "bar".to_string()],
            hidden_base_stats: vec![],
            column_vis: SavedColumnVis::default(),
            target_data: vec![target_data_from(&src, "tcp:443")],
        };
        let json = serde_json::to_string(&file).unwrap();
        let back: SessionFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.targets, file.targets);
        assert_eq!(back.args, file.args);
        assert_eq!(back.started_at, "1970-01-01T00:00:01.000Z");
        assert_eq!(back.saved_at_ms(), 2_000);
        assert_eq!(back.target_data[0].total_sent, src.total_sent);
        assert_eq!(back.target_data[0].drops, src.drops);
        assert_eq!(back.target_data[0].mode_label, "tcp:443");
        // f64::MAX/MIN sentinels must survive the JSON roundtrip
        let empty = TargetState::new("e".to_string());
        let ed = target_data_from(&empty, "icmp");
        let j = serde_json::to_string(&ed).unwrap();
        let eb: TargetData = serde_json::from_str(&j).unwrap();
        assert_eq!(eb.lifetime_min, f64::MAX);
        assert_eq!(eb.lifetime_max, f64::MIN);
    }

    #[test]
    fn target_data_missing_new_fields_defaults() {
        // A minimal (v1-style) record without the identity fields must load.
        let j = r#"{"label":"t","total_sent":3,"drops":1,"dups":0,
            "latency_sum":30.0,"latency_sq_sum":300.0,"jitter_sum":0.0,
            "jitter_count":0,"lifetime_min":9.0,"lifetime_max":11.0,
            "max_drop_streak":1,"ip_changes":0,"srtt":10.0,
            "history":[],"window":[]}"#;
        let d: TargetData = serde_json::from_str(j).unwrap();
        assert_eq!(d.total_sent, 3);
        assert_eq!(d.mode_label, "");
        assert_eq!(d.exec_cmd, "");
        assert_eq!(d.addr, None);
        assert!(!d.custom_label);
    }

    #[test]
    fn restart_command_quotes_and_joins() {
        let file = SessionFile {
            version: SESSION_VERSION,
            name: None,
            started_at: epoch_ms_to_iso(0),
            saved_at: epoch_ms_to_iso(0),
            targets: vec!["example.net:tcp:443".to_string(), "svc,exec=./a b.sh".to_string()],
            args: vec![vec!["--interval".to_string(), "500ms".to_string()], vec!["-4".to_string()]],
            extra_stats: vec![],
            hidden_base_stats: vec![],
            column_vis: SavedColumnVis::default(),
            target_data: vec![],
        };
        assert_eq!(
            restart_command(&file),
            "vlat --interval 500ms -4 example.net:tcp:443 'svc,exec=./a b.sh'"
        );
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("plain-token"), "plain-token");
        assert_eq!(shell_quote("has space"), "'has space'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");
    }
}
