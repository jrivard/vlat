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
    collections::{HashMap, VecDeque},
    io,
    process,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use crossterm::{
    cursor,
    event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal,
};
use futures::StreamExt;
use ratatui::{
    backend::CrosstermBackend,
    Terminal, TerminalOptions, Viewport,
};
use tokio::{sync::{mpsc, watch}, time};

/// Discard any keystrokes still queued in the console's input buffer (e.g. the
/// Enter that launched vlat) before the raw-mode event loop starts reading -
/// otherwise they surface as the app's first "keypress" immediately on start.
#[cfg(target_os = "windows")]
fn flush_console_input() {
    use windows_sys::Win32::System::Console::{FlushConsoleInputBuffer, GetStdHandle, STD_INPUT_HANDLE};
    unsafe {
        let h = GetStdHandle(STD_INPUT_HANDLE);
        let _ = FlushConsoleInputBuffer(h);
    }
}

#[cfg(not(target_os = "windows"))]
fn flush_console_input() {}

use crate::{
    cli::{Args, OutputFormat, PingMode, SortMode, ThemeName, ViewMode},
    constants::{DIALOG_ROWS, FAST_TICK_MS, FREEZE_NOTICE_SECS, HELP_DISMISS_SECS, SCALE_DECREASE_HOLD_SECS, SORT_NOTICE_SECS, THEME_NOTICE_SECS, VIEW_NOTICE_SECS, UI_TICK_MS, WARNING_DISMISS_SECS},
    output::{detect_format, open_csv, open_json, write_summary_json_snapshot, OutputFile},
    probe::{setup_ping_mode, spawn_demo_task, spawn_probe_task, IcmpDupTracker, TlsConfig, TlsVerify, TlsVersion},
    resolver::{parse_and_resolve, parse_target_spec, expand_target_specs, fire_resolve, build_dns_resolver, resolve_ip, DnsResolver},
    state::TargetState,
    types::{InitResolveResult, ProbeResult, ProbeStarted, ResolveResult},
    ui::{compute_col_widths, compute_scale, draw_bars, draw_bubble, draw_ekg, draw_fullscreen_multi_ui, draw_fullscreen_ui, draw_cards, draw_pong, draw_radar, draw_list_ui, draw_single_ui, draw_scatter, draw_worm, explain_max_scroll, help_dialog_ideal_height, help_menu_items, min_size, pong_left_margin, render_ambient_logo, single_history_avail, AxisField, AxisMetric, BarsState, BubbleState, ColWidthsStabilizer, DialogMode, EkgState, HelpItem, HelpSubMenu, LogoAnim, CardsState, PongState, RadarState, ScatterState, WormState, ViewCtx, HELP_SORTS, HELP_THEMES, HELP_VIEWS, VIEW_DISPLAY_ORDER, VIEW_PICKER_ORDER},
    ui::dialogs::{item_section, nav_skip},
};

fn term_size() -> (u16, u16) {
    terminal::size().unwrap_or((80, 24))
}

/// CLI name of the current view, for session saving.
fn view_cli_name(view: FullscreenView) -> &'static str {
    match view {
        FullscreenView::List    => "list",
        FullscreenView::Single  => "single",
        FullscreenView::Graph   => "graph",
        FullscreenView::Worm    => "worm",
        FullscreenView::Radar   => "radar",
        FullscreenView::Ekg     => "ekg",
        FullscreenView::Bars    => "bars",
        FullscreenView::Cards   => "cards",
        FullscreenView::Bubble  => "bubble",
        FullscreenView::Scatter => "scatter",
        FullscreenView::Pong    => "pong",
    }
}

fn view_idx(view: FullscreenView) -> usize {
    let help_idx = match view {
        FullscreenView::List    => 0,
        FullscreenView::Single  => 10,
        FullscreenView::Graph   => 1,
        FullscreenView::Worm    => 2,
        FullscreenView::Radar   => 3,
        FullscreenView::Ekg     => 4,
        FullscreenView::Bars    => 5,
        FullscreenView::Cards   => 6,
        FullscreenView::Bubble  => 7,
        FullscreenView::Scatter => 8,
        FullscreenView::Pong    => 9,
    };
    VIEW_PICKER_ORDER.iter().position(|&i| i == help_idx).unwrap_or(0)
}

fn sort_idx(mode: &SortMode) -> usize {
    match mode {
        SortMode::None   => 0,
        SortMode::Name   => 1,
        SortMode::Avg    => 2,
        SortMode::Loss   => 3,
        SortMode::Jitter => 4,
        SortMode::Mtr    => 5,
        SortMode::Std    => 6,
        SortMode::P01    => 7,
        SortMode::P10    => 8,
        SortMode::P50    => 9,
        SortMode::P95    => 10,
        SortMode::P99    => 11,
        SortMode::Cv     => 12,
        SortMode::Srtt   => 13,
        SortMode::Streak => 14,
        SortMode::Last   => 15,
    }
}

fn sort_mode_at_idx(idx: usize) -> SortMode {
    match idx {
        0  => SortMode::None,
        1  => SortMode::Name,
        2  => SortMode::Avg,
        3  => SortMode::Loss,
        4  => SortMode::Jitter,
        5  => SortMode::Mtr,
        6  => SortMode::Std,
        7  => SortMode::P01,
        8  => SortMode::P10,
        9  => SortMode::P50,
        10 => SortMode::P95,
        11 => SortMode::P99,
        12 => SortMode::Cv,
        13 => SortMode::Srtt,
        14 => SortMode::Streak,
        _  => SortMode::Last,
    }
}

#[cfg(unix)]
fn is_elevated() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(unix))]
fn is_elevated() -> bool { false }

#[derive(Clone, Copy, PartialEq)]
#[allow(dead_code)]
enum FullscreenView { List, Single, Graph, Worm, Radar, Ekg, Bars, Cards, Pong, Bubble, Scatter }

impl FullscreenView {
    fn notice_label(&self) -> (&'static str, &'static str) {
        match self {
            FullscreenView::List    => ("list",    "one line per target, no graph"),
            FullscreenView::Single  => ("single",  "detailed view for one target"),
            FullscreenView::Graph   => ("graph",   "fullscreen area graph"),
            FullscreenView::Worm    => ("worm",    "worm screensaver"),
            FullscreenView::Radar   => ("radar",   "radar sweep"),
            FullscreenView::Ekg     => ("ekg",     "EKG monitor"),
            FullscreenView::Bars    => ("bars",     "vertical bar chart"),
            FullscreenView::Cards   => ("cards",   "grid of per-target panels"),
            FullscreenView::Pong    => ("pong",    "pong"),
            FullscreenView::Bubble  => ("bubble",  "floating latency bubbles"),
            FullscreenView::Scatter => ("scatter", "scatter plot: avg RTT vs loss (a: pick axes)"),
        }
    }
}


fn eff_interval(ov: Option<u64>, global: u64) -> u64 { ov.unwrap_or(global) }
fn eff_timeout(ov: Option<f64>, global: f64) -> f64 { ov.unwrap_or(global) }
fn eff_resolve(no_dns_refresh: bool, ov: Option<u64>, global: u64) -> u64 {
    if no_dns_refresh { 0 } else { ov.unwrap_or(global) }
}

fn mode_label_str(mode: &PingMode, port: u16, args: &Args) -> String {
    mode_label_str_vis(mode, port, args, args.column_vis.port)
}

/// Like `mode_label_str` but with an explicit port-column visibility, so the
/// automatic rule can be evaluated regardless of the current override.
fn mode_label_str_vis(mode: &PingMode, port: u16, args: &Args, port_vis: Option<bool>) -> String {
    // Port suffix: auto (None) = only when the port differs from the mode default;
    // forced on/off via --columns port / the 'x' dialog. icmp and exec have no port.
    let with_port = |name: &str, is_default: bool| -> String {
        match port_vis {
            Some(false) => name.to_string(),
            Some(true)  => format!("{}:{}", name, port),
            None        => if is_default { name.to_string() } else { format!("{}:{}", name, port) },
        }
    };
    match mode {
        PingMode::Icmp  => "icmp".to_string(),
        PingMode::Udp   => with_port("udp",   port == args.udp_port),
        PingMode::Tcp   => with_port("tcp",   port == args.tcp_port),
        PingMode::Http  => with_port("http",  port == 80),
        PingMode::Https => with_port("https", port == 443),
        PingMode::Dns   => with_port("dns",   true),
        PingMode::Tls   => with_port("tls",   port == 443),
        PingMode::Ntp   => with_port("ntp",   port == crate::constants::DEFAULT_NTP_PORT),
        PingMode::Ssh   => with_port("ssh",   port == crate::constants::DEFAULT_SSH_PORT),
        PingMode::Smtp  => with_port("smtp",  port == crate::constants::DEFAULT_SMTP_PORT),
        PingMode::Smtps => with_port("smtps", port == crate::constants::DEFAULT_SMTPS_PORT),
        PingMode::Exec  => "exec".to_string(),
        PingMode::Quic  => with_port("quic",  port == crate::constants::DEFAULT_QUIC_PORT),
    }
}

/// Effective on/off state of the 5 identity columns (mode, name, port, addr, resolve)
/// shown in the 'x' dialog: forced overrides applied on top of the automatic rules.
fn identity_column_states(
    args: &Args,
    states: &[crate::state::TargetState],
    mode_labels: &[String],
    effective: &[(PingMode, u16)],
) -> [bool; 5] {
    let vis = &args.column_vis;
    let auto_mode = !mode_labels.iter().all(|m| m == "icmp");
    let auto_name = states.iter().any(|s| s.custom_label || s.host.parse::<std::net::IpAddr>().is_err());
    let auto_port = effective.iter()
        .any(|(m, p)| mode_label_str_vis(m, *p, args, None).contains(':'));
    let auto_resolve = states.iter().any(|s| s.ip_changes > 1);
    [
        vis.mode.unwrap_or(auto_mode),
        vis.name.unwrap_or(auto_name),
        vis.port.unwrap_or(auto_port),
        vis.addr.unwrap_or(true),
        vis.resolve.unwrap_or(auto_resolve),
    ]
}

fn writable_config_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("VLAT_CONFIG") {
        return std::path::PathBuf::from(p);
    }
    crate::paths::xdg_config_dir().join("vlat").join("defaults")
}

struct DefaultsFileData {
    view:    Option<String>,
    theme:   Option<String>,
    sort:    Option<String>,
    keys:    Option<String>,
    window:  Option<String>,
    columns: Option<String>,
}

fn read_defaults_file(path: &std::path::Path) -> DefaultsFileData {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return DefaultsFileData { view: None, theme: None, sort: None, keys: None, window: None, columns: None },
    };
    let mut d = DefaultsFileData { view: None, theme: None, sort: None, keys: None, window: None, columns: None };
    for line in content.lines() {
        let trimmed = line.trim();
        let mut parts = trimmed.splitn(2, |c: char| c.is_whitespace() || c == '=');
        let key = parts.next().unwrap_or("");
        let val = parts.next().unwrap_or("").to_string();
        match key {
            "--view"    => { d.view    = Some(val); }
            "--theme"   => { d.theme   = Some(val); }
            "--sort"    => { d.sort    = Some(val); }
            "--keys"    => { d.keys    = Some(val); }
            "--window"  => { d.window  = Some(val); }
            "--columns" => { d.columns = Some(val); }
            _ => {}
        }
    }
    d
}

#[allow(clippy::too_many_arguments)]
fn write_defaults_file(
    path:        &std::path::Path,
    view:        Option<&str>,
    theme:       &str,
    sort:        &str,
    keys:        bool,
    window:      u64,
    cols:        &str,
    save_view:   Option<bool>,
    save_theme:  Option<bool>,
    save_sort:   Option<bool>,
    save_keys:   Option<bool>,
    save_window: Option<bool>,
    save_cols:   Option<bool>,
) -> Result<(), String> {
    // Some(true) = write current value, None = leave unchanged, Some(false) = delete key
    let existing = if path.exists() {
        std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read '{}': {}", path.display(), e))?
    } else {
        String::new()
    };

    let mut new_lines: Vec<String> = Vec::new();
    let mut view_written   = false;
    let mut theme_written  = false;
    let mut sort_written   = false;
    let mut keys_written   = false;
    let mut window_written = false;
    let mut cols_written   = false;

    for ln in existing.lines() {
        let trimmed = ln.trim();
        let key = trimmed.split(|c: char| c.is_whitespace() || c == '=').next().unwrap_or("");
        macro_rules! handle_key {
            ($action:expr, $written:expr, $new_val:expr) => {
                match $action {
                    Some(true) => {
                        if !$written { new_lines.push($new_val); $written = true; }
                        // drop duplicates
                    }
                    None       => { new_lines.push(ln.to_string()); }
                    Some(false)=> { /* clear: drop the line */ }
                }
            };
        }
        match key {
            "--view"    => handle_key!(save_view,   view_written,   format!("--view={}", view.unwrap_or(""))),
            "--theme"   => handle_key!(save_theme,  theme_written,  format!("--theme={}", theme)),
            "--sort"    => handle_key!(save_sort,   sort_written,   format!("--sort={}", sort)),
            "--keys"    => handle_key!(save_keys,   keys_written,   format!("--keys={}", keys)),
            "--window"  => handle_key!(save_window, window_written, format!("--window={}", crate::cli::format_window_hms(window))),
            "--columns" => handle_key!(save_cols,   cols_written,   format!("--columns={}", cols)),
            _ => { new_lines.push(ln.to_string()); }
        }
    }

    if save_view   == Some(true) { if let Some(v) = view { if !view_written   { new_lines.push(format!("--view={}", v)); } } }
    if save_theme  == Some(true) && !theme_written  { new_lines.push(format!("--theme={}", theme)); }
    if save_sort   == Some(true) && !sort_written   { new_lines.push(format!("--sort={}", sort)); }
    if save_keys   == Some(true) && !keys_written   { new_lines.push(format!("--keys={}", keys)); }
    if save_window == Some(true) && !window_written { new_lines.push(format!("--window={}", crate::cli::format_window_hms(window))); }
    if save_cols   == Some(true) && !cols_written   { new_lines.push(format!("--columns={}", cols)); }

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create '{}': {}", parent.display(), e))?;
        }
    }

    let content = new_lines.join("\n") + "\n";
    std::fs::write(path, &content)
        .map_err(|e| format!("cannot write '{}': {}", path.display(), e))
}

fn extra_stat_cli_name(stat: &crate::cli::ExtraStat) -> &'static str {
    use crate::cli::ExtraStat;
    match stat {
        ExtraStat::Mtr    => "mtr",    ExtraStat::Std  => "std",
        ExtraStat::P01    => "p01",    ExtraStat::P10  => "p10",
        ExtraStat::P50    => "p50",    ExtraStat::P95  => "p95",
        ExtraStat::P99    => "p99",    ExtraStat::Cv   => "cv",
        ExtraStat::Srtt   => "srtt",   ExtraStat::Streak => "streak",
        ExtraStat::Last   => "last",
        ExtraStat::Recent => "recent", ExtraStat::Bar  => "bar",
        _ => "?",
    }
}

fn make_save_defaults_dialog(
    view_cli:      Option<&'static str>,
    theme_name:    &'static str,
    sort_cli:      &'static str,
    show_col_keys: bool,
    window_secs:   u64,
    extra_stats:   &[crate::cli::ExtraStat],
) -> crate::ui::dialogs::DialogMode {
    use crate::cli::ExtraStat;
    let config_path = writable_config_path();
    let f = read_defaults_file(&config_path);
    let has_view = view_cli.is_some();

    // Build CLI string for current extra_stats
    let cols_cli = if extra_stats.is_empty() {
        "none".to_string()
    } else {
        extra_stats.iter().map(extra_stat_cli_name).collect::<Vec<_>>().join(",")
    };

    // Delta from default set [recent, bar]
    const DEFAULT_COLS: &[ExtraStat] = &[ExtraStat::Recent, ExtraStat::Bar];
    let mut delta_parts: Vec<String> = Vec::new();
    for stat in DEFAULT_COLS {
        if !extra_stats.contains(stat) {
            delta_parts.push(format!("-{}", extra_stat_cli_name(stat)));
        }
    }
    for stat in extra_stats {
        if !DEFAULT_COLS.contains(stat) {
            delta_parts.push(format!("+{}", extra_stat_cli_name(stat)));
        }
    }
    let cols_delta = if delta_parts.is_empty() {
        "default".to_string()
    } else {
        delta_parts.join(" ")
    };

    crate::ui::dialogs::DialogMode::SaveDefaults {
        view_name:      view_cli,
        theme_name,
        sort_name:      sort_cli,
        config_path:    config_path.display().to_string(),
        save_view:      None,
        save_theme:     None,
        save_sort:      None,
        save_keys:      None,
        save_window:    None,
        save_cols:      None,
        cursor:         if has_view { 0 } else { 1 },
        keys_current:   show_col_keys,
        window_current: window_secs,
        cols_delta,
        cols_cli,
        file_view:      f.view,
        file_theme:     f.theme,
        file_sort:      f.sort,
        file_keys:      f.keys,
        file_window:    f.window,
        file_cols:      f.columns,
    }
}

pub async fn run(mut args: Args, session_ctx: crate::session::SessionCtx) -> Result<(), Box<dyn std::error::Error>> {
    // Expand any IP range (`192.168.1.1-20`) or CIDR (`192.168.1.0/24`) target specs
    // into their individual hosts before anything else looks at args.targets.
    args.targets = expand_target_specs(&args.targets).map_err(|e| format!("error: {}", e))?;
    if args.targets.len() > crate::constants::MAX_TARGETS {
        eprintln!("error: {} targets requested, vlat allows at most {}", args.targets.len(), crate::constants::MAX_TARGETS);
        eprintln!("  narrow the range/CIDR block or split targets across multiple runs");
        process::exit(1);
    }

    // Validate and parse the optional bind address up front
    let bind_ip: Option<std::net::IpAddr> = match &args.bind_addr {
        Some(s) => Some(s.parse().map_err(|_| format!("invalid bind address: '{}'", s))?),
        None    => None,
    };

    // Build custom DNS resolver if --dns-server was specified; fail fast on bad spec.
    let custom_dns_resolver: Option<DnsResolver> = if let Some(ref server) = args.dns_server {
        Some(build_dns_resolver(server).await
            .map_err(|e| format!("--dns-server '{}': {}", server, e))?)
    } else {
        None
    };

    // --demo runs synthetic targets (see crate::demo) instead of real ones -
    // skip the real-probe-only ICMP capability probe entirely rather than
    // pointlessly attempting (and logging a fallback for) a raw socket no
    // demo task will ever use.
    let demo_count: Option<usize> = args.demo.then(|| args.demo_count.unwrap_or(5));
    let (icmp_client_v4, icmp_client_v6, active_mode) = if demo_count.is_some() {
        (None, None, PingMode::Icmp)
    } else {
        setup_ping_mode(&args, bind_ip)
    };
    let target_count = demo_count.unwrap_or(args.targets.len());
    let icmp_available = matches!(active_mode, PingMode::Icmp);

    if demo_count.is_none() && !icmp_available && args.mode == Some(PingMode::Icmp) {
        eprintln!("error: ICMP mode requested but unavailable (requires root or CAP_NET_RAW)");
        process::exit(1);
    }

    // Reject exact duplicate target specs up front.
    {
        let mut seen = std::collections::HashSet::new();
        for spec in &args.targets {
            let key = spec.trim().to_lowercase();
            if !seen.insert(key) {
                eprintln!("error: duplicate target '{}' - use per-target overrides to probe the same host differently", spec);
                eprintln!("  e.g.  {}:tcp:80 {}:tcp:443", spec, spec);
                process::exit(1);
            }
        }
    }

    let is_multi = target_count > 1;
    let stagger_ms = if !is_multi { 0 } else { (args.interval / target_count as u64).min(100) };

    let mut effective:   Vec<(PingMode, u16)>                  = Vec::with_capacity(target_count);
    let mut mode_labels: Vec<String>                           = Vec::with_capacity(target_count);
    let mut states:      Vec<TargetState>                      = Vec::with_capacity(target_count);
    let mut ip_arcs:     Vec<Arc<Mutex<std::net::IpAddr>>>     = Vec::with_capacity(target_count);
    // Hostname per slot for periodic DNS re-resolution (None for IP literals or unresolved).
    let mut hostnames:   Vec<Option<String>>                   = Vec::with_capacity(target_count);
    // Per-target effective interval/timeout/resolve_interval (from inline overrides or global).
    let mut target_intervals:         Vec<u64>          = Vec::with_capacity(target_count);
    let mut target_timeouts:          Vec<f64>          = Vec::with_capacity(target_count);
    let mut target_resolve_intervals: Vec<u64>          = Vec::with_capacity(target_count);
    let mut http_path_arcs:           Vec<Arc<String>>  = Vec::with_capacity(target_count);
    let mut exec_cmd_arcs:            Vec<Arc<String>>  = Vec::with_capacity(target_count);

    // Targets removed at runtime - kept for the end-of-session summary.
    let deleted_states:      Vec<TargetState> = Vec::new();
    let deleted_mode_labels: Vec<String>      = Vec::new();

    let (probe_tx,   mut rx)       = mpsc::unbounded_channel::<ProbeResult>();
    let (start_tx,   mut start_rx) = mpsc::unbounded_channel::<ProbeStarted>();
    let icmp4_arc                  = icmp_client_v4.map(std::sync::Arc::new);
    let icmp6_arc                  = icmp_client_v6.map(std::sync::Arc::new);

    // ICMP duplicate detector - raw socket listeners, only active when ICMP is available.
    let dup_tracker: Option<Arc<IcmpDupTracker>> = if icmp_available {
        let t = Arc::new(IcmpDupTracker::new());
        t.spawn_listeners();
        Some(t)
    } else {
        None
    };
    let dns_query_arc              = Arc::new(args.dns_query.clone());
    let tls_config_arc             = Arc::new({
        use crate::cli::TlsVersionArg;
        let verify = if args.tls_no_verify {
            TlsVerify::Skip
        } else if let Some(ref path) = args.tls_cert {
            TlsVerify::Cert(path.clone())
        } else {
            TlsVerify::System
        };
        let version = match args.tls_version {
            TlsVersionArg::Any => TlsVersion::Any,
            TlsVersionArg::V12 => TlsVersion::V12,
            TlsVersionArg::V13 => TlsVersion::V13,
        };
        TlsConfig { verify, version }
    });

    // Preflight: if any target uses a TLS probe mode and cert validation is enabled,
    // verify the system cert store is loadable before entering the TUI.
    if matches!(tls_config_arc.verify, TlsVerify::System) {
        let has_tls_target = args.targets.iter().any(|spec| {
            parse_target_spec(spec, &args)
                .map(|p| matches!(p.mode, PingMode::Https | PingMode::Tls | PingMode::Smtps | PingMode::Quic))
                .unwrap_or(false)
        });
        if has_tls_target {
            let result = rustls_native_certs::load_native_certs();
            if result.certs.is_empty() {
                match result.errors.first() {
                    Some(e) => eprintln!("vlat: cannot load system certificate store: {}", e),
                    None    => eprintln!("vlat: system certificate store is empty"),
                }
                eprintln!("  To skip TLS certificate validation, use --tls-no-verify");
                process::exit(1);
            }
        }
    }

    // Stable task_id → state-slot mapping; per-task cancel senders.
    let mut next_task_id: usize = 0;
    let mut task_id_to_state: HashMap<usize, usize> = HashMap::new();
    let mut task_cancels: Vec<(usize, watch::Sender<bool>)> = Vec::new(); // (task_id, cancel_tx)

    // Allocate a task id and cancel channel, spawn the probe task for `slot`,
    // and register both in the bookkeeping maps.
    macro_rules! start_probe {
        ($slot:expr, $mode:expr, $port:expr, $start_delay:expr) => {{
            let slot = $slot;
            let tid  = next_task_id;
            next_task_id += 1;
            let (per_cancel_tx, per_cancel_rx) = watch::channel(false);
            spawn_probe_task(crate::probe::ProbeTask {
                task_id:     tid,
                ip_shared:   ip_arcs[slot].clone(),
                interval:    target_intervals[slot],
                timeout:     target_timeouts[slot],
                mode:        $mode,
                port:        $port,
                icmp4:       icmp4_arc.clone(),
                icmp6:       icmp6_arc.clone(),
                bind_ip,
                hostname:    hostnames[slot].as_ref().map(|h| Arc::new(h.clone())),
                dns_query:   dns_query_arc.clone(),
                tls_config:  tls_config_arc.clone(),
                http_path:   http_path_arcs[slot].clone(),
                exec_cmd:    exec_cmd_arcs[slot].clone(),
                tx:          probe_tx.clone(),
                start_tx:    start_tx.clone(),
                cancel:      per_cancel_rx,
                start_delay: $start_delay,
                dup_tracker: dup_tracker.clone(),
            });
            task_id_to_state.insert(tid, slot);
            task_cancels.push((tid, per_cancel_tx));
        }};
    }

    // Channel for initial async resolve results (multi-target only).
    let (init_resolve_tx, mut init_resolve_rx) = mpsc::unbounded_channel::<InitResolveResult>();
    // How many initial resolves are still outstanding (used as a select! guard).
    let mut init_resolves_remaining: usize = 0;

    let placeholder_ip: std::net::IpAddr = "0.0.0.0".parse().unwrap();

    if let Some(n) = demo_count {
        // Synthetic targets (see crate::demo): no DNS, no sockets, no real
        // IP/hostname. Each slot gets its own spawn_demo_task instead of
        // spawn_probe_task, sending the same ProbeStarted/ProbeResult
        // messages a real probe would over the same channels, so nothing
        // downstream of this block needs to know the data isn't real.
        for (slot, dt) in crate::demo::demo_targets(n).into_iter().enumerate() {
            let mut s = TargetState::new(dt.label.clone());
            s.host       = dt.addr.clone();
            s.current_ip = dt.addr.parse().ok();
            effective.push((PingMode::Icmp, 0));
            mode_labels.push(dt.mode_tag.to_string());
            ip_arcs.push(Arc::new(Mutex::new(s.current_ip.unwrap_or(placeholder_ip))));
            hostnames.push(None);
            target_intervals.push(args.interval);
            target_timeouts.push(args.timeout);
            target_resolve_intervals.push(0);
            http_path_arcs.push(Arc::new(String::new()));
            exec_cmd_arcs.push(Arc::new(String::new()));

            let tid = next_task_id;
            next_task_id += 1;
            let (per_cancel_tx, per_cancel_rx) = watch::channel(false);
            spawn_demo_task(
                tid, dt.profile, target_intervals[slot],
                probe_tx.clone(), start_tx.clone(), per_cancel_rx,
                Duration::from_millis(stagger_ms * slot as u64),
            );
            task_id_to_state.insert(tid, slot);
            task_cancels.push((tid, per_cancel_tx));

            states.push(s);
        }
    } else if !is_multi {
        // Single target: resolve synchronously before TUI starts (error out on failure).
        // Show a spinner on stderr while DNS is in flight so the terminal isn't blank.
        let needs_dns = {
            let p = parse_target_spec(&args.targets[0], &args).map_err(|e| format!("error: {}", e))?;
            p.host.parse::<std::net::IpAddr>().is_err() && p.mode != PingMode::Exec
        };
        let spinner_task: Option<tokio::task::JoinHandle<()>> = if needs_dns {
            let host = {
                let p = parse_target_spec(&args.targets[0], &args).map_err(|e| format!("error: {}", e))?;
                p.host.to_string()
            };
            let ascii = args.ascii;
            Some(tokio::spawn(async move {
                use std::io::Write;
                let unicode_frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let ascii_frames   = ["|", "/", "-", "\\"];
                let mut i = 0usize;
                loop {
                    let frame = if ascii { ascii_frames[i % ascii_frames.len()] }
                                else    { unicode_frames[i % unicode_frames.len()] };
                    eprint!("\r{} Resolving {}...", frame, host);
                    let _ = std::io::stderr().flush();
                    i += 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }))
        } else {
            None
        };
        let rt_result = parse_and_resolve(&args.targets[0], &args, custom_dns_resolver.as_ref()).await;
        if let Some(task) = spinner_task {
            task.abort();
            eprint!("\r\x1B[2K");
        }
        let rt = rt_result?;
        let eff_mode = match rt.mode {
            PingMode::Icmp if !icmp_available => {
                crate::logfile::write(&format!("probe: target '{}' requested ICMP but it is unavailable, using UDP", rt.label));
                PingMode::Udp
            }
            ref m => m.clone(),
        };
        let eff_port = rt.port;
        let mode_label = mode_label_str(&eff_mode, eff_port, &args);
        let exec_cmd_str = rt.exec_cmd.or_else(|| args.exec_cmd.clone()).unwrap_or_default();
        let mut s = TargetState::new(rt.label.clone());
        s.host         = rt.hostname.clone().unwrap_or_else(|| rt.ip.to_string());
        s.current_ip   = Some(rt.ip);
        s.custom_label = rt.label_is_custom;
        if eff_mode == PingMode::Exec { s.exec_cmd = exec_cmd_str.clone(); }
        effective.push((eff_mode.clone(), eff_port));
        mode_labels.push(mode_label);
        ip_arcs.push(Arc::new(Mutex::new(rt.ip)));
        hostnames.push(rt.hostname.clone());
        states.push(s);
        target_intervals.push(eff_interval(rt.interval, args.interval));
        target_timeouts.push(eff_timeout(rt.timeout, args.timeout));
        target_resolve_intervals.push(eff_resolve(args.no_dns_refresh, rt.resolve_interval, args.resolve_interval));
        http_path_arcs.push(Arc::new(rt.http_path.unwrap_or_else(|| args.http_path.clone())));
        exec_cmd_arcs.push(Arc::new(exec_cmd_str));

        start_probe!(0, eff_mode.clone(), eff_port, Duration::from_millis(0));
    } else {
        // Multi-target: parse specs, create states.  IP-literal targets are ready immediately;
        // hostname targets are grouped so one DNS query is shared across all slots with the
        // same name - both on startup and during periodic re-resolution.
        for (slot, spec) in args.targets.iter().enumerate() {
            let parsed = parse_target_spec(spec, &args)
                .map_err(|e| format!("error: {}", e))?;
            let host = parsed.host;
            let mode = parsed.mode;
            let port = parsed.port;
            let label_override = parsed.overrides.label.clone();
            let http_path_str = parsed.overrides.http_path.clone().unwrap_or_else(|| args.http_path.clone());
            let exec_cmd_str  = parsed.overrides.exec_cmd.clone().or_else(|| args.exec_cmd.clone()).unwrap_or_default();
            if mode == PingMode::Exec && exec_cmd_str.is_empty() {
                return Err(format!(
                    "exec target '{}' requires a command - use exec=<cmd> inline or --exec-cmd",
                    host
                ).into());
            }
            target_intervals.push(eff_interval(parsed.overrides.interval, args.interval));
            target_timeouts.push(eff_timeout(parsed.overrides.timeout, args.timeout));
            target_resolve_intervals.push(eff_resolve(args.no_dns_refresh, parsed.overrides.resolve_interval, args.resolve_interval));
            http_path_arcs.push(Arc::new(http_path_str));
            exec_cmd_arcs.push(Arc::new(exec_cmd_str.clone()));
            let eff_mode = match mode {
                PingMode::Icmp if !icmp_available => {
                    crate::logfile::write(&format!("probe: target '{}' requested ICMP but it is unavailable, using UDP", host));
                    PingMode::Udp
                }
                m => m,
            };
            let eff_port = port;
            let mode_label = mode_label_str(&eff_mode, eff_port, &args);
            let input_is_ip = host.parse::<std::net::IpAddr>().is_ok();
            let init_label = label_override.clone().unwrap_or_else(|| host.to_string());
            let mut s = TargetState::new(init_label);
            s.host         = host.to_string();
            s.custom_label = label_override.is_some();
            if eff_mode == PingMode::Exec { s.exec_cmd = exec_cmd_str.clone(); }
            effective.push((eff_mode.clone(), eff_port));
            mode_labels.push(mode_label);

            if input_is_ip || eff_mode == PingMode::Exec {
                // IP literal or exec target - no DNS needed; start probe task immediately.
                let ip: std::net::IpAddr = if input_is_ip {
                    host.parse().unwrap()
                } else {
                    placeholder_ip
                };
                s.current_ip = Some(ip);
                ip_arcs.push(Arc::new(Mutex::new(ip)));
                // For exec targets the host string is a label, not a real hostname,
                // but we still pass it so VLAT_HOST is set in the child environment.
                let hostname_for_probe = if eff_mode == PingMode::Exec && !input_is_ip {
                    Some(host.to_string())
                } else {
                    None
                };
                hostnames.push(hostname_for_probe.clone());
                states.push(s);

                start_probe!(slot, eff_mode.clone(), eff_port,
                             Duration::from_millis(stagger_ms * slot as u64));
            } else {
                // Hostname - defer until DNS resolves (grouped with peers sharing the same name).
                s.resolving = true;
                ip_arcs.push(Arc::new(Mutex::new(placeholder_ip)));
                hostnames.push(Some(host.to_string()));
                states.push(s);
            }
        }

        // Group hostname slots by their hostname string, then fire one DNS lookup per group.
        let mut hostname_groups: HashMap<String, Vec<usize>> = HashMap::new();
        for (slot, hostname) in hostnames.iter().enumerate() {
            if effective[slot].0 == PingMode::Exec { continue; }
            if let Some(h) = hostname {
                hostname_groups.entry(h.clone()).or_default().push(slot);
            }
        }
        for (host_str, slots) in hostname_groups {
            let tx2          = init_resolve_tx.clone();
            let prefer_v4    = args.ipv4;
            let prefer_v6    = args.ipv6;
            let dns_resolver = custom_dns_resolver.clone();
            tokio::spawn(async move {
                match resolve_ip(&host_str, prefer_v4, prefer_v6, dns_resolver.as_ref()).await {
                    Err(msg) => { let _ = tx2.send(InitResolveResult::Err { slots, msg }); }
                    Ok(ip)   => {
                        let label = format!("{} ({})", host_str, ip);
                        let _ = tx2.send(InitResolveResult::Ok { slots, ip, hostname: Some(host_str), label });
                    }
                }
            });
            init_resolves_remaining += 1;
        }
        drop(init_resolve_tx); // clones held by tasks; drop ours so channel closes when all done
    }
    // Keep probe_tx alive so rx.recv() only returns None on cancel, not on task exit.

    let has_exec_targets = effective.iter().any(|(m, _)| *m == PingMode::Exec);
    if has_exec_targets && is_elevated() && !args.allow_elevated_exec {
        eprintln!("error: exec probes cannot run with elevated privileges (running as root or with CAP_NET_RAW).");
        eprintln!("       Shell commands from your config would execute with those privileges.");
        eprintln!("       Pass --allow-elevated-exec to override (a startup warning will be shown).");
        process::exit(1);
    }

    // File output
    let mut output_file: Option<OutputFile> = if let Some(ref path) = args.output {
        let fmt = detect_format(path, args.output_format.as_ref());
        let result = match fmt {
            OutputFormat::Csv  => open_csv(path).map(|w| OutputFile::Csv(Box::new(w))),
            OutputFormat::Json => open_json(path).map(OutputFile::Json),
        };
        match result {
            Ok(f)  => {
                crate::logfile::write(&format!("output: logging to '{}'", path));
                Some(f)
            }
            Err(e) => {
                crate::logfile::write(&format!("output: cannot open file '{}': {}", path, e));
                eprintln!("Warning: cannot open output file '{}': {}", path, e);
                None
            }
        }
    } else { None };

    // DNS re-resolution
    let (resolve_tx, mut resolve_rx) = mpsc::unbounded_channel::<ResolveResult>();
    let mut resolve_last: Vec<std::time::Instant> = vec![std::time::Instant::now(); states.len()];
    // Tracks slots whose initial DNS resolution failed; cleared on first success.
    // These slots retry every 5 seconds instead of waiting the full resolve_interval.
    let mut never_resolved: Vec<bool> = vec![false; states.len()];
    let mut resolve_check = time::interval(Duration::from_secs(1));
    resolve_check.tick().await; // consume initial immediate tick

    // Startup terminal size check — before entering raw mode so a clean error
    // can be printed to stderr.  Uses initial col_widths derived from states.
    {
        use crate::cli::ViewMode;
        let start_view = if args.worm || args.view == ViewMode::Worm          { "worm"  }
                         else if args.radar || args.view == ViewMode::Radar    { "radar" }
                         else if args.ekg   || args.view == ViewMode::Ekg      { "ekg"   }
                         else if args.bars  || args.view == ViewMode::Bars     { "bars"  }
                         else if args.cards || args.view == ViewMode::Cards  { "cards" }
                         else if args.fullscreen || args.view == ViewMode::Graph { "graph" }
                         else if args.pong  || args.view == ViewMode::Pong    { "pong"  }
                         else                                                   { "list"  };
        if let Ok((term_w, term_h)) = terminal::size() {
            let init_cw = compute_col_widths(&states, args.is_window(), &args.extra_stats, &args.hidden_base_stats, args.ipv6, &args.column_vis);
            let (min_w, min_h) = min_size(start_view, states.len(), args.keys, true, &init_cw, &mode_labels, args.column_vis.mode);
            if term_w < min_w || term_h < min_h {
                eprintln!(
                    "vlat: terminal too small for {} view\n  Current: {}w × {}h\n  Needed:  {}w × {}h",
                    start_view, term_w, term_h, min_w, min_h
                );
                std::process::exit(1);
            }
        }
    }

    // Set up Ratatui terminal - starts at minimum height (content only).
    // On first dialog open we physically scroll the terminal up by printing
    // blank lines, then recreate the terminal claiming that new space.
    // This is done at most once per session so there are no blank lines
    // during normal operation.
    let n_targets    = target_count as u16;
    let content_rows = n_targets * 4;
    let dialog_rows  = DIALOG_ROWS;

    terminal::enable_raw_mode()?;
    flush_console_input();
    let (term_w, term_h) = term_size();
    let mut worm_state: Option<WormState> = if args.worm {
        let nw_cell_w = (term_w / 80).clamp(1, 4);
        Some(WormState::with_metric(target_count, term_w / nw_cell_w, term_h, args.worm_metric.to_axis_metric()))
    } else {
        None
    };
    let mut radar_state: Option<RadarState> = if args.radar {
        Some(RadarState::with_metric(target_count, args.radar_metric.to_axis_metric()))
    } else {
        None
    };
    let mut ekg_state: Option<EkgState> = if args.ekg {
        Some(EkgState::new(target_count))
    } else {
        None
    };
    let mut bars_state: Option<BarsState> = if args.bars {
        Some(BarsState::new(target_count))
    } else {
        None
    };
    let mut cards_state: Option<CardsState> = if args.cards {
        Some(CardsState::new(target_count))
    } else {
        None
    };
    let mut pong_state: Option<PongState> = if args.pong {
        Some(PongState::new(target_count, term_w, pong_left_margin(&states)))
    } else {
        None
    };
    let mut bubble_state: Option<BubbleState> = if args.bubble {
        Some(BubbleState::new(target_count, term_w, term_h))
    } else {
        None
    };
    let mut scatter_state: Option<ScatterState> = if args.scatter {
        Some(ScatterState::with_axes(args.scatter_x.to_axis_metric(), args.scatter_y.to_axis_metric()))
    } else {
        None
    };
    if args.view == ViewMode::List { args.list = true; }
    if args.view == ViewMode::Single {
        if target_count == 1 { args.single = true; } else { args.single = false; args.list = true; }
    }

    // All fullscreen modes use the alternate screen so the main buffer (and user's
    // cursor position) is preserved and restored when returning to list.
    if args.fullscreen || args.worm || args.radar || args.ekg || args.bars || args.cards || args.pong || args.bubble || args.scatter {
        execute!(io::stdout(), terminal::EnterAlternateScreen)?;
    }
    // include_hist is only true for single mode - list no longer shows scrolling history rows.
    let list_viewport_h = |n_targets: usize, col_keys: bool, hist_rows: u16, include_hist: bool| -> u16 {
        let hist: u16 = if include_hist && n_targets == 1 { hist_rows } else { 0 };
        n_targets as u16 + if col_keys { 2 } else { 0 } + hist
    };
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::with_options(backend, TerminalOptions {
        viewport: if args.fullscreen || args.worm || args.radar || args.ekg || args.bars || args.cards || args.pong || args.bubble || args.scatter {
            Viewport::Fullscreen
        } else if args.list || args.single {
            Viewport::Inline(list_viewport_h(target_count, args.keys, args.history_rows, args.single))
        } else {
            Viewport::Inline(content_rows)
        },
    })?;
    if args.fullscreen || args.worm || args.radar || args.ekg || args.bars || args.cards || args.pong || args.bubble || args.scatter { terminal.clear()?; }
    let mut viewport_h = if args.list || args.single { list_viewport_h(target_count, args.keys, args.history_rows, args.single) } else { content_rows };

    // Expand the inline viewport to at least `needed` rows.
    // Only recreates the terminal when the current viewport is too small.
    macro_rules! ensure_dialog_space {
        ($needed:expr) => {
            let needed: u16 = ($needed as u16).max(content_rows);
            if needed > viewport_h {
                let current_h = viewport_h;
                viewport_h = needed;
                drop(terminal);
                if current_h > 0 {
                    use std::io::Write;
                    let mut out = io::stdout();
                    write!(out, "\x1b[{}A\x1b[J", current_h)?;
                    out.flush()?;
                }
                let backend = CrosstermBackend::new(io::stdout());
                terminal = Terminal::with_options(backend, TerminalOptions {
                    viewport: Viewport::Inline(needed),
                })?;
                terminal.clear()?;
            }
        }
    }

    let mut last_term_w: u16 = term_w; // tracked across resizes to compute wrap factor
    let mut frozen          = false;
    let mut keep_display    = false;
    let mut show_headers    = true;
    let mut show_col_keys   = args.keys;
    let mut fullscreen_view = match args.view {
        ViewMode::Worm    => FullscreenView::Worm,
        ViewMode::Radar   => FullscreenView::Radar,
        ViewMode::Ekg     => FullscreenView::Ekg,
        ViewMode::Bars    => FullscreenView::Bars,
        ViewMode::Cards   => FullscreenView::Cards,
        ViewMode::Bubble  => FullscreenView::Bubble,
        ViewMode::Scatter => FullscreenView::Scatter,
        ViewMode::Graph   => FullscreenView::Graph,
        ViewMode::List    => FullscreenView::List,
        ViewMode::Single  => if target_count == 1 { FullscreenView::Single } else { FullscreenView::List },
        ViewMode::Pong    => FullscreenView::Pong,
    };
    let mut tick_count = 0u64;
    // Runtime fullscreen: Some(slot) = that single target fullscreen; None = normal view.
    let mut fullscreen_target: Option<usize> = None;
    // All-targets fullscreen (set at startup when --fullscreen + multi, or via 'f' key).
    let mut fullscreen_all: bool = args.fullscreen && target_count > 1;
    // True when the terminal is using the alternate screen buffer (entered at startup
    // via --fullscreen, or at runtime via 'f').
    let mut in_alternate_screen = args.fullscreen || args.worm || args.radar || args.ekg || args.bars || args.cards || args.pong || args.bubble || args.scatter;

    // Sorting state - only active in multi-target fullscreen, cycled by 's'.
    let mut sort_mode:         SortMode = args.sort.clone();
    let mut sort_order:        Vec<usize> = (0..states.len()).collect();
    let mut sort_arrows:       Vec<Option<(Instant, bool)>> = vec![None; states.len()];
    let mut last_sort:         Instant = Instant::now();
    let mut sort_mode_changed: Option<Instant> = None;
    // Sort interval: every 5 probe intervals, clamped to [2 s, 30 s],
    // but never more than 20× per window (prevents thrashing on noisy nets).
    let sort_interval_ms: u64 = (args.interval * 5)
        .clamp(2_000, 30_000)
        .max(args.window * 1_000 / 20);
    // Graph calibration wait: collect this many ms of samples before drawing the graph,
    // so the initial scale is stable and no animation cascade fires at startup/reset.
    // 5 probe intervals, clamped to [1 s, 10 s].
    let graph_wait_ms: u64 = (args.interval * 5).clamp(1_000, 10_000);

    // Reset every view's state and args flag; the enter_* macros below then
    // re-create the single view they activate.
    macro_rules! close_all_views {
        () => {
            // The enter_* macro that invoked this immediately re-creates one of
            // these states, making that particular `None` a dead store.
            #[allow(unused_assignments)]
            {
            worm_state    = None; args.worm    = false;
            radar_state   = None; args.radar   = false;
            ekg_state     = None; args.ekg     = false;
            bars_state    = None; args.bars    = false;
            cards_state   = None; args.cards   = false;
            pong_state    = None; args.pong    = false;
            bubble_state  = None; args.bubble  = false;
            scatter_state = None; args.scatter = false;
            args.list     = false;
            args.single   = false;
            }
        };
    }

    // Swap the inline viewport for a fullscreen one on the alternate screen,
    // so the previous inline view is restored on exit.
    macro_rules! enter_alt_screen {
        () => {
            let top = terminal.get_frame().area().y;
            let _ = execute!(io::stdout(), cursor::MoveTo(0, top));
            drop(terminal);
            execute!(io::stdout(), terminal::EnterAlternateScreen)?;
            in_alternate_screen = true;
            let backend = CrosstermBackend::new(io::stdout());
            terminal = Terminal::with_options(backend, TerminalOptions {
                viewport: Viewport::Fullscreen,
            })?;
            terminal.clear()?;
        };
    }

    // Switch the terminal to fullscreen for a specific target slot.
    macro_rules! enter_fullscreen {
        ($slot:expr) => {
            fullscreen_view   = FullscreenView::Graph;
            fullscreen_target = Some($slot);
            close_all_views!();
            enter_alt_screen!();
        };
    }

    // Enter a fullscreen screensaver view (worm/radar/ekg/bars/cards/bubble/scatter/pong):
    // clear all views, then activate $state with the value of $new.
    // Switching from one screensaver to another skips the terminal.clear() -
    // screensavers repaint the whole frame, so clearing would only flicker.
    macro_rules! enter_screensaver {
        ($variant:ident, $state:ident, $flag:ident, $new:expr) => {
            fullscreen_view = FullscreenView::$variant;
            let from_other_screensaver = in_alternate_screen && $state.is_none()
                && (worm_state.is_some() || radar_state.is_some() || ekg_state.is_some()
                    || bars_state.is_some() || cards_state.is_some() || bubble_state.is_some()
                    || scatter_state.is_some() || pong_state.is_some());
            fullscreen_target = None;
            fullscreen_all    = false;
            args.fullscreen   = false;
            close_all_views!();
            if !in_alternate_screen {
                enter_alt_screen!();
            } else if !from_other_screensaver {
                terminal.clear()?;
            }
            $state = Some($new);
            args.$flag = true;
        };
    }

    macro_rules! enter_worm {
        () => {
            enter_screensaver!(Worm, worm_state, worm, {
                let (w, h) = term_size();
                let cell_w = (w / 80).clamp(1, 4);
                WormState::with_metric(states.len(), w / cell_w, h, args.worm_metric.to_axis_metric())
            });
        };
    }

    macro_rules! enter_radar {
        () => { enter_screensaver!(Radar, radar_state, radar, RadarState::with_metric(states.len(), args.radar_metric.to_axis_metric())); };
    }

    macro_rules! enter_ekg {
        () => { enter_screensaver!(Ekg, ekg_state, ekg, EkgState::new(states.len())); };
    }

    macro_rules! enter_bars {
        () => { enter_screensaver!(Bars, bars_state, bars, BarsState::new(states.len())); };
    }

    macro_rules! enter_cards {
        () => { enter_screensaver!(Cards, cards_state, cards, CardsState::new(states.len())); };
    }

    macro_rules! enter_bubble {
        () => {
            enter_screensaver!(Bubble, bubble_state, bubble, {
                let (w, h) = term_size();
                BubbleState::new(states.len(), w, h)
            });
        };
    }

    macro_rules! enter_scatter {
        () => { enter_screensaver!(Scatter, scatter_state, scatter, ScatterState::with_axes(args.scatter_x.to_axis_metric(), args.scatter_y.to_axis_metric())); };
    }

    macro_rules! enter_pong {
        () => {
            enter_screensaver!(Pong, pong_state, pong, {
                let (w, _h) = term_size();
                PongState::new(states.len(), w, pong_left_margin(&states))
            });
        };
    }

    // Switch the terminal to all-targets fullscreen.
    // Uses the alternate screen so the previous inline view is restored on exit.
    macro_rules! enter_fullscreen_all {
        () => {
            fullscreen_view = FullscreenView::Graph;
            fullscreen_all  = true;
            close_all_views!();
            enter_alt_screen!();
        };
    }

    // Switch to list inline view (one row per target, no graph).
    macro_rules! enter_list {
        () => {
            fullscreen_view = FullscreenView::List;
            let was_alt      = in_alternate_screen;
            // Capture the inline viewport's top row before dropping the terminal.
            let viewport_top = terminal.get_frame().area().y;
            if was_alt {
                let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen);
                in_alternate_screen = false;
            }
            fullscreen_target = None;
            fullscreen_all    = false;
            args.fullscreen   = false;
            close_all_views!();
            args.list  = true;
            viewport_h = list_viewport_h(target_count, show_col_keys, args.history_rows, false);
            drop(terminal);
            if was_alt {
                // LeaveAlternateScreen restores the cursor to the position saved when
                // EnterAlternateScreen was called (the viewport top) - clear from there down.
                let _ = execute!(io::stdout(), terminal::Clear(terminal::ClearType::FromCursorDown));
            } else {
                // Inline → inline: jump to the viewport top and clear downward.
                let _ = execute!(io::stdout(),
                    cursor::MoveTo(0, viewport_top),
                    terminal::Clear(terminal::ClearType::FromCursorDown));
            }
            let backend = CrosstermBackend::new(io::stdout());
            terminal = Terminal::with_options(backend, TerminalOptions {
                viewport: Viewport::Inline(viewport_h),
            })?;
            terminal.clear()?;
        };
    }

    // Switch to single-target detail inline view. Only meaningful with exactly
    // one target; callers are responsible for checking that before invoking this.
    // Unlike list, single reserves rows for the scrolling per-return history.
    macro_rules! enter_single {
        () => {
            fullscreen_view = FullscreenView::Single;
            let was_alt      = in_alternate_screen;
            // Capture the inline viewport's top row before dropping the terminal.
            let viewport_top = terminal.get_frame().area().y;
            if was_alt {
                let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen);
                in_alternate_screen = false;
            }
            fullscreen_target = None;
            fullscreen_all    = false;
            args.fullscreen   = false;
            close_all_views!();
            args.single = true;
            viewport_h = list_viewport_h(target_count, show_col_keys, args.history_rows, true);
            drop(terminal);
            if was_alt {
                // LeaveAlternateScreen restores the cursor to the position saved when
                // EnterAlternateScreen was called (the viewport top) - clear from there down.
                let _ = execute!(io::stdout(), terminal::Clear(terminal::ClearType::FromCursorDown));
            } else {
                // Inline → inline: jump to the viewport top and clear downward.
                let _ = execute!(io::stdout(),
                    cursor::MoveTo(0, viewport_top),
                    terminal::Clear(terminal::ClearType::FromCursorDown));
            }
            let backend = CrosstermBackend::new(io::stdout());
            terminal = Terminal::with_options(backend, TerminalOptions {
                viewport: Viewport::Inline(viewport_h),
            })?;
            terminal.clear()?;
        };
    }

    // Switch to a view by display id: 0=list, 1=fullscreen graph, 2=worm,
    // 3=radar, 4=ekg, 5=bars, 6=cards, 7=bubble, 8=scatter, 9=pong, 10=single
    // (the HELP_VIEWS order).
    // The '1'..'9' hotkeys follow display order instead (see VIEW_DISPLAY_ORDER):
    // digit - 1 is a position in VIEW_DISPLAY_ORDER, which maps to this id. List and
    // single have no hotkey - both are only reachable via the picker dialog, and single
    // only when there's one target.
    macro_rules! enter_view_id {
        ($id:expr) => {
            match $id {
                0 => { enter_list!(); }
                1 => { if states.len() == 1 { enter_fullscreen!(0); } else { enter_fullscreen_all!(); } }
                2 => { enter_worm!(); }
                3 => { enter_radar!(); }
                4 => { enter_ekg!(); }
                5 => { enter_bars!(); }
                6 => { enter_cards!(); }
                7 => { enter_bubble!(); }
                8 => { enter_scatter!(); }
                9 => { enter_pong!(); }
                _ => { if states.len() == 1 { enter_single!(); } else { enter_list!(); } }
            }
        };
    }



    let mut dialog = if !icmp_available && args.mode.is_none() && !args.no_icmp_warn {
        DialogMode::Warning {
            dismiss_at: std::time::Instant::now() + Duration::from_secs(WARNING_DISMISS_SECS),
            message: "ICMP unavailable \u{2014} falling back to UDP.  Run 'vlat --explain' for more info".to_string(),
        }
    } else if has_exec_targets && is_elevated() && args.allow_elevated_exec {
        DialogMode::Warning {
            dismiss_at: std::time::Instant::now() + Duration::from_secs(WARNING_DISMISS_SECS),
            message: "WARNING: exec probes running as root \u{2014} shell commands inherit elevated privileges".to_string(),
        }
    } else {
        DialogMode::None
    };
    // If we're showing a startup dialog, ensure the viewport is tall enough now.
    if !matches!(dialog, DialogMode::None) {
        ensure_dialog_space!(3);
    }
    let mut fullscreen_logo = LogoAnim::new_cycling(3, 10 * 60, 30 * 60);
    let mut ui_ticker    = time::interval(Duration::from_millis(UI_TICK_MS));
    ui_ticker.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut fast_ticker  = time::interval(Duration::from_millis(FAST_TICK_MS));
    fast_ticker.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    // graph_history must hold enough samples to back the full --span display.
    // --window governs rolling stats; --span governs graph display; memory is
    // bounded by whichever is larger (--span is capped at 24h by the parser).
    // When window=0 (lifetime) use 300s as the effective graph backing window.
    let effective_window  = if args.window == 0 { 300 } else { args.window };
    let window_entries    = ((effective_window * 1000) / args.graph_interval.max(1) + 1) as usize;
    let mut graph_max_entries = window_entries.max(args.graph_span_cols());
    let mut graph_ticker = time::interval(Duration::from_millis(args.graph_interval));
    let mut stats_ticker = time::interval(Duration::from_secs(60));
    stats_ticker.tick().await;
    let mut session_ticker = time::interval(Duration::from_secs(crate::constants::SESSION_SAVE_SECS));
    session_ticker.tick().await;
    let mut summary_json_ticker = time::interval(Duration::from_secs(
        args.summary_json_interval.unwrap_or(crate::constants::SUMMARY_JSON_DEFAULT_SECS)
    ));
    summary_json_ticker.tick().await;
    let mut session_probes_sent:    u64 = 0;
    let mut session_packets_received: u64 = 0;
    let mut session_bytes_sent:     u64 = 0;
    let mut session_bytes_received: u64 = 0;
    let mut session_re_resolves:    u64 = 0;
    let mut session_ui_updates:     u64 = 0;
    let mut states_snapshot   = states.clone();
    let mut worm_snapshot     = worm_state.clone();
    let mut radar_snapshot    = radar_state.clone();
    let mut ekg_snapshot      = ekg_state.clone();
    let mut bars_snapshot     = bars_state.clone();
    let mut cards_snapshot    = cards_state.clone();
    let mut pong_snapshot     = pong_state.clone();
    let mut bubble_snapshot   = bubble_state.clone();
    let mut scatter_snapshot  = scatter_state.clone();

    macro_rules! fire_due_resolves {
        ($force:expr) => {{
            let now = std::time::Instant::now();
            // Group slots by hostname so each unique name gets one DNS query.
            let mut hostname_groups: HashMap<String, Vec<usize>> = HashMap::new();
            for i in 0..hostnames.len() {
                if effective[i].0 == PingMode::Exec { continue; }
                let Some(ref host) = hostnames[i] else { continue };
                let ri = target_resolve_intervals.get(i).copied().unwrap_or(0);
                if ri == 0 { continue; }
                hostname_groups.entry(host.clone()).or_default().push(i);
            }
            for (host, indices) in hostname_groups {
                // Fire if forced or any slot in the group is past its resolve interval.
                let any_due = $force || indices.iter().any(|&i| {
                    if states[i].resolving { return false; }
                    let eff_interval = if never_resolved[i] { 5 } else { target_resolve_intervals[i] };
                    now.duration_since(resolve_last[i]).as_secs() >= eff_interval
                });
                if !any_due { continue; }
                for &i in &indices {
                    resolve_last[i] = now;
                    states[i].resolving = true;
                    states[i].resolve_error = None;
                }
                fire_resolve(indices, host, args.ipv4, args.ipv6, resolve_tx.clone(), custom_dns_resolver.clone());
            }
        }}
    }

    // Session snapshot: settings (including runtime view/sort/keys toggles)
    // plus per-target summary stats.  Fired every SESSION_SAVE_SECS and at exit.
    macro_rules! save_session {
        () => {{
            let rt = crate::session::RuntimeSettings {
                view: view_cli_name(fullscreen_view),
                sort: sort_mode.as_str(),
                keys: show_col_keys,
            };
            session_ctx.save(&args, &rt, &states, &mode_labels);
        }}
    }

    // --summary-json: live "current state" snapshot, overwritten in place.
    // Fired once at startup, every summary_json_ticker interval, and at exit.
    macro_rules! save_summary_json {
        () => {{
            if let Some(ref path) = args.summary_json {
                if let Err(e) = write_summary_json_snapshot(path, &states, &mode_labels, &deleted_states, &deleted_mode_labels) {
                    crate::logfile::write(&format!("summary-json: write to {} failed: {}", path, e));
                }
            }
        }}
    }
    save_summary_json!();

    let mut key_stream = EventStream::new();
    let mut last_drawn_scale:      f64             = 0.0;
    let mut suppress_scale_anim:   bool            = false;
    let mut scale_decrease_since:  Option<Instant> = None;
    let mut col_widths_stabilizer = ColWidthsStabilizer::new();

    // ── centralized frame scheduling ─────────────────────────────────────────
    // Select branches never call terminal.draw() themselves; they mutate state
    // and set `needs_frame`.  One frame is then drawn at the bottom of the loop
    // iteration by draw_frame!, so every repaint - whatever triggered it - goes
    // through a single renderer with one scale and one column layout.
    let mut needs_frame = false;

    // True while anything needs the 200 ms cadence: a screensaver view, an open
    // dialog, the ambient logo, a running animation, an unresolved/waiting
    // target spinner, or a frozen display.  The fast ticker owns frame pacing
    // whenever this holds; the 1 Hz ticker only requests frames when it doesn't.
    macro_rules! fast_mode_active {
        () => {
            worm_state.is_some()
                || radar_state.is_some()
                || ekg_state.is_some() || bars_state.is_some() || cards_state.is_some()
                || pong_state.is_some() || bubble_state.is_some() || scatter_state.is_some()
                || !matches!(dialog, DialogMode::None)
                || fullscreen_logo.is_active()
                || states.iter().any(|s| s.graph_anim_start.is_some())
                || states.iter().any(|s| s.bar_anim_start.is_some())
                || states.iter().any(|s| s.current_ip.is_none() || s.waiting)
                || frozen
        }
    }

    // Single source of truth for the chart scale.  While a scale animation is
    // running the scale is locked to the animation target so consecutive
    // frames (and all views) agree; otherwise it is computed from the data.
    macro_rules! current_scale {
        ($states:expr) => {{
            let global_p95 = $states.iter().filter(|s| !s.waiting)
                .map(|s| s.win_p95()).fold(f64::MIN, f64::max);
            $states.iter()
                .find(|s| !s.waiting && s.scale_anim.is_some() && s.scale_anim_new > 0.0)
                .map(|s| s.scale_anim_new)
                .unwrap_or_else(|| compute_scale(&args, global_p95))
        }}
    }

    // Set true only for the final exit frame so draw_frame! stashes the displayed buffer
    // into captured_buf (used at teardown to replay the view and measure its height).
    // Declared before the macro so the macro body can resolve them.
    let mut capture_frame = false;
    let mut captured_buf: Option<ratatui::buffer::Buffer> = None;

    // The one renderer.  Picks live or frozen state, computes scale + column
    // widths once, arms the scale animation when the scale changed, and
    // dispatches to whichever view is active.
    macro_rules! draw_frame {
        () => {{
            let shared_scale = if frozen {
                current_scale!(&states_snapshot)
            } else {
                let s = current_scale!(&states);
                if suppress_scale_anim {
                    last_drawn_scale    = s;
                    suppress_scale_anim = false;
                    scale_decrease_since = None;
                } else {
                    // Always derive the true data-required scale (ignoring any
                    // animation lock) so trigger_scale_anim can detect when new
                    // probe data exceeds the current in-flight animation target.
                    let global_p95 = states.iter().filter(|st| !st.waiting)
                        .map(|st| st.win_p95()).fold(f64::MIN, f64::max);
                    let true_s = compute_scale(&args, global_p95);
                    let effective_s = if last_drawn_scale > 0.0 && true_s < last_drawn_scale {
                        if scale_decrease_since.is_none() {
                            scale_decrease_since = Some(Instant::now());
                        }
                        if scale_decrease_since.map_or(false, |t| t.elapsed().as_secs_f64() >= SCALE_DECREASE_HOLD_SECS) {
                            scale_decrease_since = None;
                            true_s
                        } else {
                            last_drawn_scale
                        }
                    } else {
                        scale_decrease_since = None;
                        true_s
                    };
                    trigger_scale_anim(&mut states, effective_s, &mut last_drawn_scale);
                }
                s
            };
            let disp: &[TargetState] = if frozen { &states_snapshot[..] } else { &states[..] };
            let col_widths = col_widths_stabilizer.apply(compute_col_widths(disp, args.is_window(), &args.extra_stats, &args.hidden_base_stats, args.ipv6, &args.column_vis));
            let log_fmt    = output_file.as_ref().map(|f| f.format_name()).unwrap_or("");
            session_ui_updates += 1;
            let ctx = ViewCtx {
                args:              &args,
                mode_labels:       &mode_labels,
                col_widths:        &col_widths,
                shared_scale,
                log_fmt,
                tick:              tick_count,
                dialog:            &dialog,
                sort_order:        &sort_order,
                sort_arrows:       &sort_arrows,
                sort_mode:         &sort_mode,
                sort_mode_changed,
                frozen,
                show_headers,
                show_col_keys,
            };
            // When frozen, draw from the freeze-time snapshot; otherwise the live state.
            macro_rules! live_or_snap {
                ($state:ident, $snap:ident, $as:ident) => {
                    if frozen { $snap.$as() } else { None }.or($state.$as()).unwrap()
                };
            }
            // Render one view frame; `logo` overlays the ambient cycling logo.
            macro_rules! view_frame {
                ($draw:ident, $vs:expr) => {
                    terminal.draw(|f| { $draw(f, disp, $vs, &ctx); })?
                };
                ($draw:ident, $vs:expr, logo) => {
                    terminal.draw(|f| {
                        $draw(f, disp, $vs, &ctx);
                        render_ambient_logo(f, disp.len(), show_headers, &fullscreen_logo, &args);
                    })?
                };
            }
            // NB: this is an *expression* - each arm yields the CompletedFrame from
            // terminal.draw() so the final exit frame can be snapshotted (see capture_frame
            // below).
            let __completed = if worm_state.is_some() {
                view_frame!(draw_worm, live_or_snap!(worm_state, worm_snapshot, as_ref), logo)
            } else if radar_state.is_some() {
                view_frame!(draw_radar, live_or_snap!(radar_state, radar_snapshot, as_ref), logo)
            } else if ekg_state.is_some() {
                view_frame!(draw_ekg, live_or_snap!(ekg_state, ekg_snapshot, as_mut), logo)
            } else if bars_state.is_some() {
                view_frame!(draw_bars, live_or_snap!(bars_state, bars_snapshot, as_mut), logo)
            } else if cards_state.is_some() {
                view_frame!(draw_cards, live_or_snap!(cards_state, cards_snapshot, as_mut))
            } else if pong_state.is_some() {
                view_frame!(draw_pong, live_or_snap!(pong_state, pong_snapshot, as_mut), logo)
            } else if bubble_state.is_some() {
                view_frame!(draw_bubble, live_or_snap!(bubble_state, bubble_snapshot, as_ref), logo)
            } else if scatter_state.is_some() {
                view_frame!(draw_scatter, live_or_snap!(scatter_state, scatter_snapshot, as_ref), logo)
            } else {
                terminal.draw(|f| {
                    if fullscreen_all || (args.fullscreen && disp.len() > 1) {
                        draw_fullscreen_multi_ui(f, disp, &ctx, Some(&fullscreen_logo));
                    } else {
                        let fs_slot = if args.fullscreen && disp.len() == 1 { Some(0) } else { fullscreen_target };
                        if let Some(slot) = fs_slot.filter(|&s| s < disp.len()) {
                            draw_fullscreen_ui(f, &disp[slot], &mode_labels[slot], &ctx);
                        } else if args.single {
                            draw_single_ui(f, disp, &ctx);
                        } else {
                            draw_list_ui(f, disp, &ctx);
                        }
                    }
                })?
            };
            // Stash a copy of the frame that was just drawn, but ONLY for the final exit
            // frame (capture_frame).  It must come from the CompletedFrame returned by
            // draw() above: ratatui double-buffers and resets `current_buffer_mut()` on
            // swap, so reading the buffer back afterwards yields a *blank* frame.  That
            // long-standing bug is exactly why Ctrl-C "preserve view" produced an empty
            // screen in every fullscreen/screensaver view.
            if capture_frame {
                captured_buf = Some(__completed.buffer.clone());
            } else {
                let _ = &__completed; // borrow ends here; nothing to keep in the hot path
            }
        }}
    }

    loop {
        tokio::select! {
            // Keyboard / resize
            key_event = key_stream.next() => {
                let Some(Ok(event)) = key_event else { continue };
                // On terminal resize, clear old content and redraw immediately.
                if let Event::Resize(new_w, _) = event {
                    if in_alternate_screen || worm_state.is_some() || radar_state.is_some() || ekg_state.is_some() || bars_state.is_some() || pong_state.is_some() || bubble_state.is_some() || scatter_state.is_some() {
                        let _ = terminal.clear();
                    } else {
                        // In inline mode, when the terminal gets narrower the old rendered
                        // lines wrap in the terminal emulator, leaving orphaned duplicate
                        // lines above ratatui's next draw position.  Compute how many
                        // visual rows the old content could have wrapped into
                        // (ceil(old_width / new_width) × viewport rows), move the cursor
                        // back that far, and erase to end of screen before recreating the
                        // terminal so ratatui draws fresh from a clean position.
                        let (_, cur_row) = cursor::position().unwrap_or((0, viewport_h));
                        let w_old = last_term_w.max(1) as u32;
                        let w_new = new_w.max(1) as u32;
                        let wrap_factor = w_old.div_ceil(w_new).max(1);
                        let rows_back = ((viewport_h as u32 * wrap_factor).min(cur_row as u32)) as u16;
                        if rows_back > 0 {
                            use std::io::Write;
                            let mut out = io::stdout();
                            write!(out, "\x1b[{}A\x1b[J", rows_back)?;
                            out.flush()?;
                        }
                        drop(terminal);
                        let backend = CrosstermBackend::new(io::stdout());
                        terminal = Terminal::with_options(backend, TerminalOptions {
                            viewport: Viewport::Inline(viewport_h),
                        })?;
                    }
                    last_term_w = new_w;
                    // Repaint immediately - the resize arm continues past the
                    // scheduler at the loop bottom, so invoke the renderer here.
                    draw_frame!();
                    continue;
                }
                let Event::Key(k) = event else { continue };
                if k.kind == KeyEventKind::Release { continue; }
                let (code, mods) = (k.code, k.modifiers);
                // Ctrl-C / Ctrl-Q always quit, keeping the current view on screen with the
                // summary printed below it.  This is handled HERE - before any dialog or
                // view-specific routing - so it works identically from every view and from
                // inside every dialog/picker.  Keeping it in one place is deliberate: the
                // old per-arm handling meant a newly added dialog or fullscreen view would
                // silently swallow Ctrl-C, which is how this behaviour kept regressing.
                if mods.contains(KeyModifiers::CONTROL)
                    && matches!(code, KeyCode::Char('c') | KeyCode::Char('q'))
                {
                    keep_display = true;
                    break;
                }
                // FreezeNotice and SortNotice are auto-dismissing toasts; any key clears them
                // so the keypress is handled normally by the None arm below.
                if matches!(dialog, DialogMode::FreezeNotice { .. } | DialogMode::SortNotice { .. } | DialogMode::ThemeNotice { .. } | DialogMode::ViewNotice { .. }) {
                    dialog = DialogMode::None;
                }
                // Shift-T cycles the theme from anywhere, dialogs included - same
                // "handled here, before dialog routing" pattern as Ctrl-C above, so
                // it works identically from every dialog. The only exclusion is text
                // entry (filename / window-duration input), where 'T' must stay a
                // literal character. Remapping to Null (rather than an early
                // continue) lets the normal draw/scheduler tail below still run.
                let code = if code == KeyCode::Char('T')
                    && !matches!(dialog, DialogMode::FilenameInput { .. } | DialogMode::WindowInput { .. })
                {
                    let n = HELP_THEMES.len();
                    let ti = HELP_THEMES.iter().position(|&n2| n2 == args.theme.name).unwrap_or(0);
                    let idx = (ti + 1) % n;
                    args.theme_name = ThemeName::at_idx(idx);
                    args.theme = args.theme_name.to_theme();
                    args.theme_changed = Some(Instant::now());
                    // Only pop the notice toast when nothing else is on screen -
                    // cycling from inside another dialog shouldn't interrupt it.
                    if matches!(dialog, DialogMode::None) {
                        dialog = DialogMode::ThemeNotice {
                            dismiss_at: Instant::now() + Duration::from_secs(THEME_NOTICE_SECS),
                            theme_name: HELP_THEMES[idx],
                        };
                    }
                    KeyCode::Null
                } else {
                    code
                };
                match &mut dialog {
                        DialogMode::Warning { .. } => {
                            // Any key dismisses the warning
                            dialog = DialogMode::None;
                        }
                        DialogMode::FilenameInput { format, input } => {
                            match code {
                                KeyCode::Tab => {
                                    *format = match format {
                                        OutputFormat::Csv  => OutputFormat::Json,
                                        OutputFormat::Json => OutputFormat::Csv,
                                    };
                                }
                                KeyCode::Esc | KeyCode::Char('q') => {
                                    dialog = DialogMode::None;
                                }
                                KeyCode::Enter => {
                                    let s = input.trim().to_string();
                                    if !s.is_empty() {
                                        let mut path = s.clone();
                                        if !path.contains('.') {
                                            path.push_str(if matches!(format, OutputFormat::Json) { ".json" } else { ".csv" });
                                        }
                                        let result = match format {
                                            OutputFormat::Csv  => open_csv(&path).map(|w| OutputFile::Csv(Box::new(w))),
                                            OutputFormat::Json => open_json(&path).map(OutputFile::Json),
                                        };
                                        match result {
                                            Ok(f)  => {
                                                crate::logfile::write(&format!("output: logging started to '{}'", path));
                                                output_file = Some(f);
                                            }
                                            Err(e) => {
                                                crate::logfile::write(&format!("output: cannot open file '{}': {}", path, e));
                                            }
                                        }
                                    }
                                    dialog = DialogMode::None;
                                }
                                KeyCode::Backspace => { input.pop(); }
                                KeyCode::Char(ch) if !mods.contains(KeyModifiers::CONTROL) => {
                                    input.push(ch);
                                }
                                _ => {}
                            }
                        }
                        DialogMode::WindowInput { input } => {
                            match code {
                                KeyCode::Esc | KeyCode::Char('q') => {
                                    dialog = DialogMode::None;
                                }
                                KeyCode::Enter => {
                                    let raw = input.trim().to_string();
                                    let new_secs = if raw.is_empty() {
                                        Some(if args.window == 0 { 300 } else { 0 })
                                    } else {
                                        crate::cli::parse_window_input_secs(&raw).filter(|&s| {
                                            s == 0 || s >= crate::constants::WINDOW_MIN_SECS
                                        })
                                    };
                                    if let Some(secs) = new_secs {
                                        args.window = secs;
                                        let effective = if args.window == 0 { 300 } else { args.window };
                                        let we = ((effective * 1000) / args.graph_interval.max(1) + 1) as usize;
                                        graph_max_entries = we.max(args.graph_span_cols());
                                        for s in states.iter_mut() {
                                            s.resize_window(secs, graph_max_entries);
                                        }
                                        suppress_scale_anim = true;
                                    }
                                    dialog = DialogMode::None;
                                }
                                KeyCode::Backspace => { input.pop(); }
                                KeyCode::Char(ch) if !mods.contains(KeyModifiers::CONTROL) => {
                                    input.push(ch);
                                }
                                _ => {}
                            }
                        }
                        DialogMode::Help { dismiss_at, logo: _, cursor, sub_menu, collapsed, .. } => {
                            if let Some(ref mut sub) = sub_menu {
                                // ── Sub-menu navigation ──────────────────────────────────────────
                                match code {
                                    KeyCode::Esc | KeyCode::Char('q') => { *sub_menu = None; }
                                    KeyCode::Up | KeyCode::Char('k') => {
                                        match sub {
                                            // Bounded by VIEW_PICKER_ORDER (not HELP_VIEWS) since Enter
                                            // resolves this cursor through VIEW_PICKER_ORDER below, and
                                            // that list is shorter than HELP_VIEWS while pong is hidden.
                                            HelpSubMenu::View    { cursor: c } => { let n = VIEW_PICKER_ORDER.len();  *c = if *c == 0 { n - 1 } else { *c - 1 }; }
                                            HelpSubMenu::Sort    { cursor: c } => { let n = HELP_SORTS.len();  *c = if *c == 0 { n - 1 } else { *c - 1 }; }
                                            HelpSubMenu::Theme   { cursor: c } => { let n = HELP_THEMES.len(); *c = if *c == 0 { n - 1 } else { *c - 1 }; }
                                            HelpSubMenu::Logging { cursor: c } => { let n = if output_file.is_some() { 3 } else { 2 }; *c = if *c == 0 { n - 1 } else { *c - 1 }; }
                                        }
                                    }
                                    KeyCode::Down | KeyCode::Char('j') => {
                                        match sub {
                                            HelpSubMenu::View    { cursor: c } => { let n = VIEW_PICKER_ORDER.len();  *c = (*c + 1) % n; }
                                            HelpSubMenu::Sort    { cursor: c } => { let n = HELP_SORTS.len();  *c = (*c + 1) % n; }
                                            HelpSubMenu::Theme   { cursor: c } => { let n = HELP_THEMES.len(); *c = (*c + 1) % n; }
                                            HelpSubMenu::Logging { cursor: c } => { let n = if output_file.is_some() { 3 } else { 2 }; *c = (*c + 1) % n; }
                                        }
                                    }
                                    KeyCode::Enter => {
                                        match sub {
                                            HelpSubMenu::View { cursor: sc } => {
                                                enter_view_id!(VIEW_PICKER_ORDER.get(*sc).copied().unwrap_or(0));
                                                let (vn, vd) = fullscreen_view.notice_label();
                                                dialog = DialogMode::ViewNotice {
                                                    dismiss_at: Instant::now() + Duration::from_secs(VIEW_NOTICE_SECS),
                                                    view_name: vn, view_desc: vd,
                                                };
                                            }
                                            HelpSubMenu::Sort { cursor: sc } => {
                                                sort_mode = sort_mode_at_idx(*sc);
                                                sort_mode_changed = Some(Instant::now());
                                                if sort_mode == SortMode::None {
                                                    sort_order  = (0..states.len()).collect();
                                                    sort_arrows = vec![None; states.len()];
                                                } else {
                                                    last_sort = Instant::now() - Duration::from_millis(sort_interval_ms);
                                                }
                                                dialog = DialogMode::SortNotice {
                                                    dismiss_at: Instant::now() + Duration::from_secs(SORT_NOTICE_SECS),
                                                    sort_mode: sort_mode.clone(),
                                                };
                                            }
                                            HelpSubMenu::Theme { cursor: sc } => {
                                                let new_tn = match *sc {
                                                    0 => ThemeName::Default,
                                                    1 => ThemeName::Nord,
                                                    2 => ThemeName::Gruvbox,
                                                    3 => ThemeName::Dracula,
                                                    4 => ThemeName::Solarized,
                                                    5 => ThemeName::Okabe,
                                                    6 => ThemeName::Highcontrast,
                                                    7 => ThemeName::Phosphor,
                                                    8 => ThemeName::Retro,
                                                    _ => ThemeName::Nocolor,
                                                };
                                                args.theme_name = new_tn;
                                                args.theme = args.theme_name.to_theme();
                                                args.theme_changed = Some(Instant::now());
                                                dialog = DialogMode::ThemeNotice {
                                                    dismiss_at: Instant::now() + Duration::from_secs(THEME_NOTICE_SECS),
                                                    theme_name: HELP_THEMES[(*sc).min(HELP_THEMES.len() - 1)],
                                                };
                                            }
                                            HelpSubMenu::Logging { cursor: sc } => {
                                                if *sc == 2 || output_file.is_some() {
                                                    crate::logfile::write(&format!("output: {} logging stopped", output_file.as_ref().map(|f| f.format_name()).unwrap_or("")));
                                                    output_file = None;
                                                    *sub_menu = None;
                                                    *dismiss_at = Instant::now() + Duration::from_secs(HELP_DISMISS_SECS);
                                                } else {
                                                    let fmt = if *sc == 1 { OutputFormat::Json } else { OutputFormat::Csv };
                                                    if !in_alternate_screen && !args.fullscreen && !args.worm && !args.radar {
                                                        ensure_dialog_space!(dialog_rows);
                                                    }
                                                    dialog = DialogMode::FilenameInput { format: fmt, input: String::new() };
                                                }
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            } else {
                                // ── Main menu input ──────────────────────────────────────────────
                                let sort_available = states.len() > 1;
                                let has_headers = matches!(fullscreen_view, FullscreenView::Graph | FullscreenView::Worm | FullscreenView::Radar | FullscreenView::Ekg | FullscreenView::Bars | FullscreenView::Pong);
                                let show_axis_menu = matches!(fullscreen_view, FullscreenView::Scatter | FullscreenView::Worm | FullscreenView::Radar);
                                let items       = help_menu_items(sort_available, has_headers, show_axis_menu);
                                let item_count  = items.len();

                                match code {
                                    KeyCode::Esc | KeyCode::Char('h') | KeyCode::Char('q') => { dialog = DialogMode::None; }
                                    KeyCode::Up => {
                                        let n = item_count.max(1);
                                        let mut c = if *cursor == 0 { n - 1 } else { *cursor - 1 };
                                        while nav_skip(&items, c, collapsed) { c = if c == 0 { n - 1 } else { c - 1 }; }
                                        *cursor = c;
                                    }
                                    KeyCode::Down => {
                                        let n = item_count.max(1);
                                        let mut c = (*cursor + 1) % n;
                                        while nav_skip(&items, c, collapsed) { c = (c + 1) % n; }
                                        *cursor = c;
                                    }
                                    KeyCode::Left | KeyCode::Right => {
                                        if let Some(sec) = item_section(&items, *cursor) {
                                            let s = sec as usize;
                                            collapsed[s] = !collapsed[s];
                                            if collapsed[s] {
                                                // Move cursor to the section separator
                                                if let Some(sep_idx) = items.iter().position(|it|
                                                    matches!(it, HelpItem::Separator { sec: Some(n), .. } if *n == sec)
                                                ) {
                                                    *cursor = sep_idx;
                                                }
                                            } else {
                                                // Expanded: move past the separator to the first item
                                                let sep_pos = *cursor;
                                                if let Some(new_pos) = items.iter().enumerate().skip(sep_pos + 1)
                                                    .find(|(j, it)| !matches!(it, HelpItem::Separator { .. }) && !nav_skip(&items, *j, collapsed))
                                                    .map(|(j, _)| j)
                                                {
                                                    *cursor = new_pos;
                                                }
                                            }
                                        }
                                    }
                                    KeyCode::Enter => {
                                        if let Some(&item) = items.get(*cursor) {
                                            match item {
                                                HelpItem::ToggleHelp | HelpItem::CloseHelp => { dialog = DialogMode::None; }
                                                HelpItem::Separator { .. } => {}
                                                HelpItem::ToggleColKeys => {
                                                    show_col_keys = !show_col_keys;
                                                    *dismiss_at = Instant::now() + Duration::from_secs(HELP_DISMISS_SECS);
                                                }
                                                HelpItem::ToggleExtraStats => {
                                                    if !in_alternate_screen && !args.fullscreen && !args.worm && !args.radar {
                                                        ensure_dialog_space!(crate::ui::dialogs::STAT_TOGGLE_DIALOG_H);
                                                    }
                                                    dialog = DialogMode::StatColumnToggle {
                                                        cursor: 0,
                                                        identity: identity_column_states(&args, &states, &mode_labels, &effective),
                                                    };
                                                }
                                                HelpItem::Explain => {
                                                    if !in_alternate_screen && !args.fullscreen && !args.worm && !args.radar {
                                                        let term_h = term_size().1;
                                                        ensure_dialog_space!(term_h);
                                                    }
                                                    dialog = DialogMode::Explain { scroll: 0 };
                                                }
                                                HelpItem::ViewMenu => {
                                                    *sub_menu = Some(HelpSubMenu::View { cursor: view_idx(fullscreen_view) });
                                                }
                                                HelpItem::AxisMenu => {
                                                    if let Some(nw) = worm_state.as_ref() {
                                                        let cursor = AxisMetric::ALL.iter().position(|&m| m == nw.metric).unwrap_or(0);
                                                        if !in_alternate_screen { ensure_dialog_space!((AxisMetric::ALL.len() as u16) + 6); }
                                                        dialog = DialogMode::MetricPicker { cursor };
                                                    } else if let Some(radar) = radar_state.as_ref() {
                                                        let cursor = AxisMetric::ALL.iter().position(|&m| m == radar.metric).unwrap_or(0);
                                                        if !in_alternate_screen { ensure_dialog_space!((AxisMetric::ALL.len() as u16) + 6); }
                                                        dialog = DialogMode::MetricPicker { cursor };
                                                    } else {
                                                        let axis_cursor = scatter_state.as_ref()
                                                            .and_then(|sc| AxisMetric::ALL.iter().position(|&m| m == sc.x_axis))
                                                            .unwrap_or(0);
                                                        if !in_alternate_screen { ensure_dialog_space!((AxisMetric::ALL.len() as u16) + 9); }
                                                        dialog = DialogMode::AxisPicker { cursor: axis_cursor, field: AxisField::X };
                                                    }
                                                }
                                                HelpItem::ToggleHeaders => {
                                                    show_headers = !show_headers;
                                                    *dismiss_at = Instant::now() + Duration::from_secs(HELP_DISMISS_SECS);
                                                }
                                                HelpItem::SetWindow => {
                                                    if !in_alternate_screen && !args.fullscreen && !args.worm && !args.radar {
                                                        ensure_dialog_space!(dialog_rows);
                                                    }
                                                    dialog = DialogMode::WindowInput { input: String::new() };
                                                }
                                                HelpItem::SortMenu if sort_available => {
                                                    *sub_menu = Some(HelpSubMenu::Sort { cursor: sort_idx(&sort_mode) });
                                                }
                                                HelpItem::SortMenu => {
                                                    // N/A: single target, no-op
                                                }
                                                HelpItem::ThemeMenu => {
                                                    let theme_idx = HELP_THEMES.iter().position(|&n| n == args.theme.name).unwrap_or(0);
                                                    *sub_menu = Some(HelpSubMenu::Theme { cursor: theme_idx });
                                                }
                                                HelpItem::FreezeToggle => {
                                                    frozen = !frozen;
                                                    if frozen {
                                                        states_snapshot  = states.clone();
                                                        worm_snapshot    = worm_state.clone();
                                                        radar_snapshot   = radar_state.clone();
                                                        ekg_snapshot     = ekg_state.clone();
                                                        bars_snapshot    = bars_state.clone();
                                                        cards_snapshot   = cards_state.clone();
                                                        pong_snapshot    = pong_state.clone();
                                                        bubble_snapshot  = bubble_state.clone();
                                                        scatter_snapshot = scatter_state.clone();
                                                    }
                                                    *dismiss_at = Instant::now() + Duration::from_secs(HELP_DISMISS_SECS);
                                                }
                                                HelpItem::SaveDefaults => {
                                                    let view_cli = match fullscreen_view {
                                                        FullscreenView::List    => Some("list"),
                                                        FullscreenView::Single  => Some("single"),
                                                        FullscreenView::Graph   => Some("graph"),
                                                        FullscreenView::Worm    => Some("worm"),
                                                        FullscreenView::Radar   => Some("radar"),
                                                        FullscreenView::Ekg     => Some("ekg"),
                                                        FullscreenView::Bars    => Some("bars"),
                                                        FullscreenView::Cards   => Some("cards"),
                                                        FullscreenView::Bubble  => Some("bubble"),
                                                        FullscreenView::Scatter => Some("scatter"),
                                                        FullscreenView::Pong    => Some("pong"),
                                                    };
                                                    let sort_cli: &'static str = sort_mode.as_str();
                                                    if !in_alternate_screen && !args.fullscreen && !args.worm && !args.radar {
                                                        ensure_dialog_space!(dialog_rows);
                                                    }
                                                    dialog = make_save_defaults_dialog(view_cli, args.theme.name, sort_cli, show_col_keys, args.window, &args.extra_stats);
                                                }
                                                HelpItem::ReResolve => {
                                                    if !args.no_dns_refresh { fire_due_resolves!(true); }
                                                    *dismiss_at = Instant::now() + Duration::from_secs(HELP_DISMISS_SECS);
                                                }
                                                HelpItem::LoggingMenu => {
                                                    if output_file.is_some() {
                                                        crate::logfile::write(&format!("output: {} logging stopped", output_file.as_ref().map(|f| f.format_name()).unwrap_or("")));
                                                        output_file = None;
                                                        *dismiss_at = Instant::now() + Duration::from_secs(HELP_DISMISS_SECS);
                                                    } else {
                                                        *sub_menu = Some(HelpSubMenu::Logging { cursor: 0 });
                                                    }
                                                }
                                                HelpItem::Quit => break,
                                            }
                                        }
                                    }
                                    KeyCode::Char('e') => { dialog = DialogMode::Explain { scroll: 0 }; }
                                    KeyCode::Char('d') => {
                                        let view_cli = match fullscreen_view {
                                            FullscreenView::List    => Some("list"),
                                            FullscreenView::Single  => Some("single"),
                                            FullscreenView::Graph   => Some("graph"),
                                            FullscreenView::Worm    => Some("worm"),
                                            FullscreenView::Radar   => Some("radar"),
                                            FullscreenView::Ekg     => Some("ekg"),
                                            FullscreenView::Bars    => Some("bars"),
                                            FullscreenView::Cards   => Some("cards"),
                                            FullscreenView::Bubble  => Some("bubble"),
                                            FullscreenView::Scatter => Some("scatter"),
                                            FullscreenView::Pong    => Some("pong"),
                                        };
                                        let sort_cli: &'static str = sort_mode.as_str();
                                        dialog = make_save_defaults_dialog(view_cli, args.theme.name, sort_cli, show_col_keys, args.window, &args.extra_stats);
                                    }
                                    _ => {
                                        // Execute the action but keep help open; reset the timer.
                                        *dismiss_at = std::time::Instant::now() + Duration::from_secs(HELP_DISMISS_SECS);
                                        match code {
                                            KeyCode::Char('v') if !states.is_empty() => {
                                                // Open view sub-menu within help (feature 1)
                                                let vi = view_idx(fullscreen_view);
                                                *sub_menu = Some(HelpSubMenu::View { cursor: vi });
                                            }
                                            KeyCode::Char(' ') => {
                                                frozen = !frozen;
                                                if frozen {
                                                    states_snapshot = states.clone();
                                                    worm_snapshot   = worm_state.clone();
                                                    radar_snapshot  = radar_state.clone();
                                                    ekg_snapshot    = ekg_state.clone();
                                                    bars_snapshot   = bars_state.clone();
                                                    cards_snapshot  = cards_state.clone();
                                                    pong_snapshot   = pong_state.clone();
                                                    bubble_snapshot = bubble_state.clone();
                                                }
                                                dialog = DialogMode::FreezeNotice {
                                                    dismiss_at: Instant::now() + Duration::from_secs(FREEZE_NOTICE_SECS),
                                                    now_frozen: frozen,
                                                };
                                            }
                                            KeyCode::Char('t') => {
                                                // Open theme sub-menu within help (feature 1)
                                                let ti = HELP_THEMES.iter().position(|&n| n == args.theme.name).unwrap_or(0);
                                                *sub_menu = Some(HelpSubMenu::Theme { cursor: ti });
                                            }
                                            KeyCode::Char('s') if sort_available => {
                                                // Open sort sub-menu within help (feature 1)
                                                *sub_menu = Some(HelpSubMenu::Sort { cursor: sort_idx(&sort_mode) });
                                            }
                                            KeyCode::Char('i') if in_alternate_screen || worm_state.is_some() || radar_state.is_some() || ekg_state.is_some() || bars_state.is_some() || pong_state.is_some() || bubble_state.is_some() || scatter_state.is_some() => {
                                                show_headers = !show_headers;
                                            }
                                            KeyCode::Char('k') => { show_col_keys = !show_col_keys; }
                                            KeyCode::Char('w') => { dialog = DialogMode::WindowInput { input: String::new() }; }
                                            KeyCode::Char('r') if !args.no_dns_refresh => { fire_due_resolves!(true); }
                                            KeyCode::Char('l') => {
                                                if output_file.is_some() {
                                                    crate::logfile::write(&format!("output: {} logging stopped", output_file.as_ref().map(|f| f.format_name()).unwrap_or("")));
                                                    output_file = None;
                                                } else {
                                                    if !in_alternate_screen && !args.fullscreen && !args.worm && !args.radar {
                                                        ensure_dialog_space!(dialog_rows);
                                                    }
                                                    dialog = DialogMode::FilenameInput { format: OutputFormat::Csv, input: String::new() };
                                                }
                                            }
                                            KeyCode::Char(ch @ '1'..='9') if !states.is_empty() => {
                                                let display_c = (ch as u8 - b'1') as usize;
                                                enter_view_id!(VIEW_DISPLAY_ORDER.get(display_c).copied().unwrap_or(0));
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                        DialogMode::Explain { scroll } => {
                            let max = explain_max_scroll(args.ascii, viewport_h);
                            match code {
                                KeyCode::Esc | KeyCode::Char('e') | KeyCode::Char('q') => {
                                    dialog = DialogMode::None;
                                }
                                KeyCode::Up   | KeyCode::Char('k') => { *scroll = scroll.saturating_sub(1); }
                                KeyCode::Down | KeyCode::Char('j') => { *scroll = scroll.saturating_add(1).min(max); }
                                KeyCode::PageUp   => { *scroll = scroll.saturating_sub(10); }
                                KeyCode::PageDown => { *scroll = scroll.saturating_add(10).min(max); }
                                KeyCode::Home => { *scroll = 0; }
                                KeyCode::End  => { *scroll = max; }
                                _ => {}
                            }
                        }
                        DialogMode::SaveDefaults { view_name, theme_name, sort_name, config_path,
                                                   save_view, save_theme, save_sort,
                                                   save_keys, save_window, save_cols,
                                                   keys_current, window_current, cols_cli,
                                                   cursor, .. } => {
                            let vn = *view_name;
                            let tn = *theme_name;
                            let sn = *sort_name;
                            let has_view = vn.is_some();
                            match code {
                                KeyCode::Up | KeyCode::Char('k') => {
                                    let min_c = if has_view { 0 } else { 1 };
                                    let new_c = if *cursor <= min_c { 6 } else { *cursor - 1 };
                                    *cursor = if new_c == 0 && !has_view { 6 } else { new_c };
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    let new_c = (*cursor + 1) % 7;
                                    *cursor = if new_c == 0 && !has_view { 1 } else { new_c };
                                }
                                KeyCode::Char(' ') | KeyCode::Enter => {
                                    fn cycle(s: Option<bool>) -> Option<bool> {
                                        match s { None => Some(true), Some(true) => Some(false), Some(false) => None }
                                    }
                                    match *cursor {
                                        0 if has_view => { *save_view   = cycle(*save_view); }
                                        1             => { *save_theme  = cycle(*save_theme); }
                                        2             => { *save_sort   = cycle(*save_sort); }
                                        3             => { *save_keys   = cycle(*save_keys); }
                                        4             => { *save_window = cycle(*save_window); }
                                        5             => { *save_cols   = cycle(*save_cols); }
                                        6 => {
                                            let sv = *save_view;
                                            let st = *save_theme;
                                            let ss = *save_sort;
                                            let sk = *save_keys;
                                            let sw = *save_window;
                                            let sc = *save_cols;
                                            let kc = *keys_current;
                                            let wc = *window_current;
                                            let cc = cols_cli.clone();
                                            let cp = config_path.clone();
                                            let result = write_defaults_file(
                                                &std::path::PathBuf::from(&cp),
                                                vn, tn, sn, kc, wc, &cc,
                                                sv, st, ss, sk, sw, sc,
                                            );
                                            dialog = match result {
                                                Ok(()) => DialogMode::Warning {
                                                    dismiss_at: std::time::Instant::now() + Duration::from_secs(WARNING_DISMISS_SECS),
                                                    message: format!("defaults saved \u{2014} {}", cp),
                                                },
                                                Err(e) => DialogMode::Warning {
                                                    dismiss_at: std::time::Instant::now() + Duration::from_secs(WARNING_DISMISS_SECS),
                                                    message: format!("error saving defaults: {}", e),
                                                },
                                            };
                                        }
                                        _ => {}
                                    }
                                }
                                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('d') => {
                                    dialog = DialogMode::None;
                                }
                                _ => {}
                            }
                        }
                        DialogMode::StatColumnToggle { cursor, identity } => {
                            use crate::ui::dialogs::STAT_TOGGLE_COUNT;
                            use crate::cli::BaseStat;
                            const IDENTITY_COUNT: usize = 5; // mode, name, port, addr, resolve
                            const BASE_STATS: &[BaseStat] = &[
                                BaseStat::Avg, BaseStat::Range, BaseStat::Jitter, BaseStat::Drops,
                            ];
                            match code {
                                KeyCode::Esc | KeyCode::Char('c') | KeyCode::Char('q') => {
                                    dialog = DialogMode::None;
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    *cursor = if *cursor == 0 { STAT_TOGGLE_COUNT - 1 } else { *cursor - 1 };
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    *cursor = (*cursor + 1) % STAT_TOGGLE_COUNT;
                                }
                                KeyCode::Char(' ') | KeyCode::Enter => {
                                    let idx = *cursor;
                                    if idx < IDENTITY_COUNT {
                                        // Identity columns: toggling pins an explicit on/off
                                        // (the automatic rule no longer applies this session).
                                        let now_on = !identity[idx];
                                        identity[idx] = now_on;
                                        match idx {
                                            0 => args.column_vis.mode    = Some(now_on),
                                            1 => args.column_vis.name    = Some(now_on),
                                            2 => {
                                                args.column_vis.port = Some(now_on);
                                                // Port lives inside the mode-badge labels - rebuild them.
                                                for (i, (m, p)) in effective.iter().enumerate() {
                                                    mode_labels[i] = mode_label_str(m, *p, &args);
                                                }
                                            }
                                            3 => args.column_vis.addr    = Some(now_on),
                                            _ => args.column_vis.resolve = Some(now_on),
                                        }
                                    } else if idx < IDENTITY_COUNT + BASE_STATS.len() {
                                        let stat = BASE_STATS[idx - IDENTITY_COUNT].clone();
                                        if args.hidden_base_stats.contains(&stat) {
                                            args.hidden_base_stats.retain(|s| s != &stat);
                                        } else {
                                            args.hidden_base_stats.push(stat);
                                        }
                                    } else {
                                        let extra_idx = idx - IDENTITY_COUNT - BASE_STATS.len();
                                        let stat = crate::cli::EXTRA_STAT_ALL[extra_idx].clone();
                                        if args.extra_stats.contains(&stat) {
                                            args.extra_stats.retain(|s| s != &stat);
                                        } else {
                                            args.extra_stats.push(stat);
                                            args.extra_stats.sort_by_key(|s| {
                                                crate::cli::EXTRA_STAT_ALL.iter().position(|a| a == s).unwrap_or(usize::MAX)
                                            });
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        DialogMode::ViewPicker { cursor } => {
                            let n = VIEW_PICKER_ORDER.len();
                            macro_rules! enter_view_at {
                                ($picker_cursor:expr) => {
                                    let id = VIEW_PICKER_ORDER.get($picker_cursor).copied().unwrap_or(0);
                                    enter_view_id!(id);
                                    // The picker stays open over the list/single view; make room for it.
                                    if id == 0 || id == 9 { ensure_dialog_space!((HELP_VIEWS.len() as u16) + 9); }
                                }
                            }
                            match code {
                                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('v') => {
                                    dialog = DialogMode::None;
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    *cursor = if *cursor == 0 { n - 1 } else { *cursor - 1 };
                                    let c = *cursor;
                                    enter_view_at!(c);
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    *cursor = (*cursor + 1) % n;
                                    let c = *cursor;
                                    enter_view_at!(c);
                                }
                                KeyCode::Char(ch @ '1'..='9') => {
                                    // Digit shortcuts follow the global hotkey mapping (VIEW_DISPLAY_ORDER),
                                    // not the picker's on-screen order, so '1' still means "list" etc. here too.
                                    let display_c = (ch as usize) - ('1' as usize);
                                    let id = VIEW_DISPLAY_ORDER.get(display_c).copied().unwrap_or(0);
                                    *cursor = VIEW_PICKER_ORDER.iter().position(|&i| i == id).unwrap_or(0);
                                    enter_view_id!(id);
                                    if id == 0 || id == 9 { ensure_dialog_space!((HELP_VIEWS.len() as u16) + 9); }
                                    dialog = DialogMode::None;
                                }
                                _ => {}
                            }
                        }
                        DialogMode::SortPicker { cursor } => {
                            let n = HELP_SORTS.len();
                            macro_rules! apply_sort_at {
                                ($idx:expr) => {
                                    sort_mode = sort_mode_at_idx($idx);
                                    sort_mode_changed = Some(Instant::now());
                                    if sort_mode == SortMode::None {
                                        sort_order  = (0..states.len()).collect();
                                        sort_arrows = vec![None; states.len()];
                                    } else {
                                        last_sort = Instant::now() - Duration::from_millis(sort_interval_ms);
                                    }
                                }
                            }
                            match code {
                                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('s') => {
                                    dialog = DialogMode::None;
                                }
                                KeyCode::Char('r') => {
                                    args.reverse_sort = !args.reverse_sort;
                                    if sort_mode != SortMode::None {
                                        last_sort = Instant::now() - Duration::from_millis(sort_interval_ms);
                                    }
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    *cursor = if *cursor == 0 { n - 1 } else { *cursor - 1 };
                                    let c = *cursor;
                                    apply_sort_at!(c);
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    *cursor = (*cursor + 1) % n;
                                    let c = *cursor;
                                    apply_sort_at!(c);
                                }
                                _ => {}
                            }
                        }
                        DialogMode::ThemePicker { cursor } => {
                            match code {
                                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('t') => {
                                    dialog = DialogMode::None;
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    let n = HELP_THEMES.len();
                                    *cursor = if *cursor == 0 { n - 1 } else { *cursor - 1 };
                                    args.theme_name = ThemeName::at_idx(*cursor);
                                    args.theme = args.theme_name.to_theme();
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    let n = HELP_THEMES.len();
                                    *cursor = (*cursor + 1) % n;
                                    args.theme_name = ThemeName::at_idx(*cursor);
                                    args.theme = args.theme_name.to_theme();
                                }
                                _ => {}
                            }
                        }
                        DialogMode::AxisPicker { cursor, field } => {
                            let n = AxisMetric::ALL.len();
                            if let Some(sc) = scatter_state.as_mut() {
                                match code {
                                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('a') => {
                                        dialog = DialogMode::None;
                                    }
                                    KeyCode::Up | KeyCode::Char('k') => {
                                        *cursor = if *cursor == 0 { n - 1 } else { *cursor - 1 };
                                        let m = AxisMetric::ALL[*cursor];
                                        match field { AxisField::X => sc.set_x_axis(m), AxisField::Y => sc.set_y_axis(m) }
                                    }
                                    KeyCode::Down | KeyCode::Char('j') => {
                                        *cursor = (*cursor + 1) % n;
                                        let m = AxisMetric::ALL[*cursor];
                                        match field { AxisField::X => sc.set_x_axis(m), AxisField::Y => sc.set_y_axis(m) }
                                    }
                                    KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                                        *field = match field { AxisField::X => AxisField::Y, AxisField::Y => AxisField::X };
                                        let current = match field { AxisField::X => sc.x_axis, AxisField::Y => sc.y_axis };
                                        *cursor = AxisMetric::ALL.iter().position(|&m| m == current).unwrap_or(0);
                                    }
                                    KeyCode::Char('l') => { sc.toggle_log_x(); }
                                    _ => {}
                                }
                            } else {
                                dialog = DialogMode::None;
                            }
                        }
                        DialogMode::MetricPicker { cursor } => {
                            let n = AxisMetric::ALL.len();
                            if worm_state.is_some() || radar_state.is_some() {
                                match code {
                                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('a') => {
                                        dialog = DialogMode::None;
                                    }
                                    KeyCode::Up | KeyCode::Char('k') => {
                                        *cursor = if *cursor == 0 { n - 1 } else { *cursor - 1 };
                                        let m = AxisMetric::ALL[*cursor];
                                        if let Some(nw) = worm_state.as_mut() { nw.set_metric(m); }
                                        else if let Some(radar) = radar_state.as_mut() { radar.set_metric(m); }
                                    }
                                    KeyCode::Down | KeyCode::Char('j') => {
                                        *cursor = (*cursor + 1) % n;
                                        let m = AxisMetric::ALL[*cursor];
                                        if let Some(nw) = worm_state.as_mut() { nw.set_metric(m); }
                                        else if let Some(radar) = radar_state.as_mut() { radar.set_metric(m); }
                                    }
                                    _ => {}
                                }
                            } else {
                                dialog = DialogMode::None;
                            }
                        }
                        DialogMode::None => {
                            // q/Esc from fullscreen: quit without leaving fullscreen first
                            if (fullscreen_target.is_some() || fullscreen_all)
                                && matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                                    break;
                                }
                            match code {
                                // NB: Ctrl-C / Ctrl-Q are handled globally above (quit + keep
                                // display); only plain q/Esc need handling here.
                                KeyCode::Char('q') | KeyCode::Esc => break,
                                KeyCode::Char(' ') => {
                                    frozen = !frozen;
                                    if !in_alternate_screen { ensure_dialog_space!(3); }
                                    if frozen {
                                        states_snapshot  = states.clone();
                                        worm_snapshot    = worm_state.clone();
                                        radar_snapshot   = radar_state.clone();
                                        ekg_snapshot     = ekg_state.clone();
                                        bars_snapshot    = bars_state.clone();
                                        cards_snapshot   = cards_state.clone();
                                        pong_snapshot    = pong_state.clone();
                                        bubble_snapshot  = bubble_state.clone();
                                        scatter_snapshot = scatter_state.clone();
                                    }
                                    dialog = DialogMode::FreezeNotice {
                                        dismiss_at: Instant::now() + Duration::from_secs(FREEZE_NOTICE_SECS),
                                        now_frozen: frozen,
                                    };
                                }
                                KeyCode::Char('v') if !states.is_empty() => {
                                    let cursor = view_idx(fullscreen_view);
                                    if !in_alternate_screen { ensure_dialog_space!((HELP_VIEWS.len() as u16) + 9); }
                                    dialog = DialogMode::ViewPicker { cursor };
                                }
                                KeyCode::Char('V') if !states.is_empty() => {
                                    let n = VIEW_PICKER_ORDER.len();
                                    let next = (view_idx(fullscreen_view) + 1) % n;
                                    let id = VIEW_PICKER_ORDER.get(next).copied().unwrap_or(0);
                                    enter_view_id!(id);
                                    let (vn, vd) = fullscreen_view.notice_label();
                                    dialog = DialogMode::ViewNotice {
                                        dismiss_at: Instant::now() + Duration::from_secs(VIEW_NOTICE_SECS),
                                        view_name: vn, view_desc: vd,
                                    };
                                }
                                KeyCode::Char('h') | KeyCode::Enter => {
                                    if !in_alternate_screen && !args.fullscreen && !args.worm && !args.radar {
                                        let sn = match &sort_mode { SortMode::None => "n/a", _ => "name" };
                                        let vn = fullscreen_view.notice_label().0;
                                        let ideal_h = help_dialog_ideal_height(vn, sn);
                                        let term_h  = term_size().1;
                                        ensure_dialog_space!(ideal_h.min(term_h));
                                    }
                                    dialog = DialogMode::Help { page: 0, scroll: 0, dismiss_at: std::time::Instant::now() + Duration::from_secs(HELP_DISMISS_SECS), logo: LogoAnim::new(), cursor: 0, sub_menu: None, collapsed: [false; 3] };
                                }
                                KeyCode::Char('e') => {
                                    if !in_alternate_screen && !args.fullscreen && !args.worm && !args.radar {
                                        let term_h = term_size().1;
                                        ensure_dialog_space!(term_h);
                                    }
                                    dialog = DialogMode::Explain { scroll: 0 };
                                }

                                KeyCode::Char('s') if states.len() > 1 => {
                                    let cursor = sort_idx(&sort_mode);
                                    if !in_alternate_screen { ensure_dialog_space!((HELP_SORTS.len() as u16) + 5); }
                                    dialog = DialogMode::SortPicker { cursor };
                                }
                                KeyCode::Char('S') if states.len() > 1 => {
                                    let n = HELP_SORTS.len();
                                    let idx = (sort_idx(&sort_mode) + 1) % n;
                                    sort_mode = sort_mode_at_idx(idx);
                                    sort_mode_changed = Some(Instant::now());
                                    if sort_mode == SortMode::None {
                                        sort_order  = (0..states.len()).collect();
                                        sort_arrows = vec![None; states.len()];
                                    } else {
                                        last_sort = Instant::now() - Duration::from_millis(sort_interval_ms);
                                    }
                                    dialog = DialogMode::SortNotice {
                                        dismiss_at: Instant::now() + Duration::from_secs(SORT_NOTICE_SECS),
                                        sort_mode: sort_mode.clone(),
                                    };
                                }
                                KeyCode::Char('t') => {
                                    let cursor = HELP_THEMES.iter().position(|&n| n == args.theme.name).unwrap_or(0);
                                    if !in_alternate_screen { ensure_dialog_space!((HELP_THEMES.len() as u16) + 5); }
                                    dialog = DialogMode::ThemePicker { cursor };
                                }
                                // Shift-T is handled globally above (before dialog routing).
                                KeyCode::Char('a') if scatter_state.is_some() || worm_state.is_some() || radar_state.is_some() => {
                                    if let Some(nw) = worm_state.as_ref() {
                                        let cursor = AxisMetric::ALL.iter().position(|&m| m == nw.metric).unwrap_or(0);
                                        if !in_alternate_screen { ensure_dialog_space!((AxisMetric::ALL.len() as u16) + 6); }
                                        dialog = DialogMode::MetricPicker { cursor };
                                    } else if let Some(radar) = radar_state.as_ref() {
                                        let cursor = AxisMetric::ALL.iter().position(|&m| m == radar.metric).unwrap_or(0);
                                        if !in_alternate_screen { ensure_dialog_space!((AxisMetric::ALL.len() as u16) + 6); }
                                        dialog = DialogMode::MetricPicker { cursor };
                                    } else if let Some(sc) = scatter_state.as_ref() {
                                        let cursor = AxisMetric::ALL.iter().position(|&m| m == sc.x_axis).unwrap_or(0);
                                        if !in_alternate_screen { ensure_dialog_space!((AxisMetric::ALL.len() as u16) + 9); }
                                        dialog = DialogMode::AxisPicker { cursor, field: AxisField::X };
                                    }
                                }
                                KeyCode::Char('i') if in_alternate_screen || worm_state.is_some() || radar_state.is_some() || ekg_state.is_some() || bars_state.is_some() || pong_state.is_some() || bubble_state.is_some() => {
                                    show_headers = !show_headers;
                                }
                                KeyCode::Char('c') => {
                                    if !in_alternate_screen && !args.fullscreen && !args.worm && !args.radar {
                                        ensure_dialog_space!(crate::ui::dialogs::STAT_TOGGLE_DIALOG_H);
                                    }
                                    dialog = DialogMode::StatColumnToggle {
                                        cursor: 0,
                                        identity: identity_column_states(&args, &states, &mode_labels, &effective),
                                    };
                                }
                                KeyCode::Up | KeyCode::Down if fullscreen_view == FullscreenView::Single && target_count == 1 => {
                                    let disp: &[TargetState] = if frozen { &states_snapshot[..] } else { &states[..] };
                                    let disp_col_widths = compute_col_widths(disp, args.is_window(), &args.extra_stats, &args.hidden_base_stats, args.ipv6, &args.column_vis);
                                    let (term_w, term_h) = term_size();
                                    let full_area = ratatui::layout::Rect { x: 0, y: 0, width: term_w, height: term_h };
                                    let max_rows = (single_history_avail(full_area, disp, &args, &disp_col_widths) as u16).max(1);
                                    args.history_rows = if code == KeyCode::Up {
                                        (args.history_rows + 1).min(max_rows)
                                    } else {
                                        args.history_rows.saturating_sub(1).max(1)
                                    };
                                    // The inline viewport must be resized to make room for
                                    // (or reclaim) the scrolling history rows.
                                    if args.single && !in_alternate_screen {
                                        let needed = list_viewport_h(target_count, show_col_keys, args.history_rows, true);
                                        if needed != viewport_h {
                                            let viewport_top = terminal.get_frame().area().y;
                                            viewport_h = needed;
                                            drop(terminal);
                                            let _ = execute!(io::stdout(),
                                                cursor::MoveTo(0, viewport_top),
                                                terminal::Clear(terminal::ClearType::FromCursorDown));
                                            let backend = CrosstermBackend::new(io::stdout());
                                            terminal = Terminal::with_options(backend, TerminalOptions {
                                                viewport: Viewport::Inline(viewport_h),
                                            })?;
                                            terminal.clear()?;
                                        }
                                    }
                                }
                                KeyCode::Char('k') => {
                                    show_col_keys = !show_col_keys;
                                    // In list/single mode the inline viewport must be resized to make room
                                    // for (or reclaim) the 2-row col-key header.
                                    if (args.list || args.single) && !in_alternate_screen {
                                        let needed = target_count as u16 + if show_col_keys { 2 } else { 0 };
                                        if needed != viewport_h {
                                            let viewport_top = terminal.get_frame().area().y;
                                            viewport_h = needed;
                                            drop(terminal);
                                            let _ = execute!(io::stdout(),
                                                cursor::MoveTo(0, viewport_top),
                                                terminal::Clear(terminal::ClearType::FromCursorDown));
                                            let backend = CrosstermBackend::new(io::stdout());
                                            terminal = Terminal::with_options(backend, TerminalOptions {
                                                viewport: Viewport::Inline(viewport_h),
                                            })?;
                                            terminal.clear()?;
                                        }
                                    }
                                }
                                KeyCode::Char('w') => {
                                    if !in_alternate_screen && !args.fullscreen && !args.worm && !args.radar {
                                        ensure_dialog_space!(dialog_rows);
                                    }
                                    dialog = DialogMode::WindowInput { input: String::new() };
                                }
                                KeyCode::Char('r') if !args.no_dns_refresh => {
                                    fire_due_resolves!(true);
                                }
                                KeyCode::Char('d') => {
                                    let view_cli = match fullscreen_view {
                                        FullscreenView::List    => Some("list"),
                                        FullscreenView::Single  => Some("single"),
                                        FullscreenView::Graph   => Some("graph"),
                                        FullscreenView::Worm    => Some("worm"),
                                        FullscreenView::Radar   => Some("radar"),
                                        FullscreenView::Ekg     => Some("ekg"),
                                        FullscreenView::Bars    => Some("bars"),
                                        FullscreenView::Cards   => Some("cards"),
                                        FullscreenView::Bubble  => Some("bubble"),
                                        FullscreenView::Scatter => Some("scatter"),
                                        FullscreenView::Pong    => Some("pong"),
                                    };
                                    let sort_cli: &'static str = sort_mode.as_str();
                                    if !in_alternate_screen && !args.fullscreen && !args.worm && !args.radar {
                                        ensure_dialog_space!(dialog_rows);
                                    }
                                    dialog = make_save_defaults_dialog(view_cli, args.theme.name, sort_cli, show_col_keys, args.window, &args.extra_stats);
                                }
                                KeyCode::Char('l') => {
                                    if output_file.is_some() {
                                        crate::logfile::write(&format!("output: {} logging stopped", output_file.as_ref().map(|f| f.format_name()).unwrap_or("")));
                                        output_file = None;
                                    } else {
                                        if !in_alternate_screen && !args.fullscreen && !args.worm && !args.radar {
                                            ensure_dialog_space!(dialog_rows);
                                        }
                                        dialog = DialogMode::FilenameInput {
                                            format: OutputFormat::Csv,
                                            input: String::new(),
                                        };
                                    }
                                }
                                KeyCode::Char(ch @ '1'..='9') if !states.is_empty() => {
                                    let display_c = (ch as u8 - b'1') as usize;
                                    enter_view_id!(VIEW_DISPLAY_ORDER.get(display_c).copied().unwrap_or(0));
                                }
                                _ => {}
                            }
                        }
                        // FreezeNotice / SortNotice / etc. are pre-cleared above; these arms are
                        // unreachable but required for exhaustiveness.
                        DialogMode::FreezeNotice { .. } => {}
                        DialogMode::SortNotice { .. } => {}
                        DialogMode::ThemeNotice { .. } => {}
                        DialogMode::ViewNotice { .. } => {}
                    }
                    // Every handled keypress repaints via the centralized scheduler -
                    // this covers dialog navigation, view switches, and toggles alike.
                    needs_frame = true;
            }

            // Probe sent - advance the timeline immediately
            started = start_rx.recv() => {
                let Some(started) = started else { break };
                let Some(&slot) = task_id_to_state.get(&started.task_id) else { continue };
                states[slot].record_sent(started.seq);
                session_probes_sent += 1;
            }

            // Probe result
            msg = rx.recv() => {
                let Some(msg) = msg else { break };

                // Ignore results from tasks whose target has been deleted
                let slot = match task_id_to_state.get(&msg.task_id) {
                    Some(&s) => s,
                    None     => continue,
                };

                let was_drop    = msg.outcome.is_err();
                let was_waiting = states[slot].waiting;
                if !was_drop { session_packets_received += 1; }
                session_bytes_sent     += msg.bytes_sent;
                session_bytes_received += msg.bytes_received;
                states[slot].record_result(msg.seq, msg.outcome, args.window, msg.dup);
                // First result: start the calibration window so the graph draws at a
                // stable scale instead of cascading through early scale animations.
                // Skip calibration for drops - they don't affect the Y-axis scale, so
                // there's nothing to stabilize. This prevents a late timeout from an
                // unresponsive target from re-triggering the overlay after the graph
                // has already been shown for responsive targets.
                if was_waiting && !states[slot].waiting && !was_drop {
                    let now = Instant::now();
                    states[slot].calibrating = Some((now, now + Duration::from_millis(graph_wait_ms)));
                }

                if was_drop && args.alert { print!("\x07"); }

                if let Some(ref mut f) = output_file {
                    f.write_row(
                        &states[slot],
                        hostnames[slot].as_deref(),
                        effective[slot].1,
                        &effective[slot].0,
                        msg.seq,
                        msg.dup,
                        msg.outcome,
                    );
                }

                if let (false, Ok(rtt_ms), Some(thresh)) = (was_drop, msg.outcome, args.warn_rtt) {
                    if rtt_ms >= thresh {
                        print!("\x07");
                        states[slot].threshold_flash = 6;
                    }
                }

                if let Some(limit) = args.count {
                    if states.iter().map(|s| s.total_sent).sum::<u64>() >= limit { break; }
                }
            }

            // Initial async resolve result (multi-target startup)
            init_res = init_resolve_rx.recv(), if init_resolves_remaining > 0 => {
                init_resolves_remaining = init_resolves_remaining.saturating_sub(1);
                if let Some(init_res) = init_res {
                    match init_res {
                        InitResolveResult::Err { slots, msg } => {
                            crate::logfile::write(&format!("dns: {}", msg));
                            for slot in &slots {
                                states[*slot].resolving = false;
                                states[*slot].resolve_error = Some(msg.clone());
                                never_resolved[*slot] = true;
                            }
                        }
                        InitResolveResult::Ok { slots, ip, hostname, label } => {
                            // Apply the shared DNS result to every slot that requested it.
                            for &slot in &slots {
                                states[slot].resolving = false;
                                if !states[slot].custom_label {
                                    states[slot].label = label.clone();
                                }
                                states[slot].current_ip = Some(ip);
                                *ip_arcs[slot].lock().unwrap() = ip;
                                hostnames[slot] = hostname.clone();

                                // Spawn probe task for this now-resolved slot.
                                let (eff_mode, eff_port) = &effective[slot];
                                start_probe!(slot, eff_mode.clone(), *eff_port,
                                             Duration::from_millis(stagger_ms * slot as u64));
                            }
                        }
                    }
                }
            }

            // DNS re-resolution tick (per-target check every second)
            _ = resolve_check.tick() => {
                fire_due_resolves!(false);
            }

            // DNS re-resolution result
            res = resolve_rx.recv() => {
                if let Some(r) = res {
                    session_re_resolves += 1;
                    for &i in &r.indices {
                        states[i].resolving = false;
                        if let Some(new_ip) = r.new_ip {
                            let old_ip = states[i].current_ip;
                            if old_ip != Some(new_ip) {
                                states[i].prev_ip        = old_ip;
                                *ip_arcs[i].lock().unwrap() = new_ip;
                                states[i].current_ip     = Some(new_ip);
                                if !states[i].custom_label {
                                    states[i].label = r.new_label.clone();
                                }
                                states[i].last_scale     = 0.0;
                                states[i].trail          = VecDeque::new();
                                states[i].bar_ema        = 0.0;
                                states[i].resolve_notice = 8;
                                if let Some(old_ip) = old_ip {
                                    states[i].ip_changes += 1;
                                    if let Some(ref host) = hostnames[i] {
                                        crate::logfile::write(&format!(
                                            "dns: '{}' changed from {} to {}", host, old_ip, new_ip
                                        ));
                                    }
                                } else {
                                    // First successful resolution after an initial failure -
                                    never_resolved[i] = false;
                                    // no probe task was ever spawned for this slot, so start one now.
                                    let (eff_mode, eff_port) = &effective[i];
                                    start_probe!(i, eff_mode.clone(), *eff_port, Duration::from_millis(0));
                                }
                            }
                        } else {
                            // Re-resolution failed - restore the error so the UI shows it.
                            if let Some(ref host) = hostnames[i] {
                                states[i].resolve_error = Some(format!("cannot resolve '{}'", host));
                                crate::logfile::write(&format!("dns: re-resolve failed for '{}'", host));
                            }
                        }
                    }
                }
            }

            // Graph history tick - independent of probe rate
            _ = graph_ticker.tick(), if !frozen => {
                for s in states.iter_mut().filter(|s| !s.waiting) {
                    s.flush_to_graph(graph_max_entries);
                }
            }

            // Periodic stats log (every 60 seconds)
            _ = stats_ticker.tick() => {
                crate::logfile::write(&format!(
                    "stats: probes_sent={} packets_received={} bytes_sent={} bytes_received={} re_resolves={} ui_updates={}",
                    session_probes_sent, session_packets_received, session_bytes_sent, session_bytes_received, session_re_resolves, session_ui_updates,
                ));
            }

            // Periodic session snapshot (also fires on exit, below the loop)
            _ = session_ticker.tick(), if session_ctx.enabled => {
                save_session!();
            }

            // Periodic --summary-json snapshot (also fires at startup and exit)
            _ = summary_json_ticker.tick(), if args.summary_json.is_some() => {
                save_summary_json!();
            }

            // UI tick
            _ = ui_ticker.tick() => {
                tick_count += 1;
                // Advance fullscreen logo Waiting phase (1 Hz is enough resolution)
                fullscreen_logo.tick();
                // Auto-dismiss toast dialogs regardless of frozen state
                if let DialogMode::FreezeNotice { dismiss_at, .. } | DialogMode::SortNotice { dismiss_at, .. } | DialogMode::ThemeNotice { dismiss_at, .. } | DialogMode::ViewNotice { dismiss_at, .. } = &mut dialog {
                    if std::time::Instant::now() >= *dismiss_at {
                        dialog = DialogMode::None;
                    }
                }

                if frozen { continue; }
                // Auto-dismiss timed warning
                if let DialogMode::Warning { dismiss_at, .. } = &dialog {
                    if std::time::Instant::now() >= *dismiss_at {
                        dialog = DialogMode::None;
                    }
                }
                // Auto-dismiss help after countdown
                if let DialogMode::Help { dismiss_at, logo, .. } = &mut dialog {
                    if std::time::Instant::now() >= *dismiss_at {
                        logo.start_draw_out();
                    }
                }
                sync_and_tick_scale_anim(&mut states);

                // Expire any calibration windows that have elapsed.
                {
                    let now = Instant::now();
                    for s in states.iter_mut() {
                        if let Some((_, until)) = s.calibrating {
                            if until <= now { s.calibrating = None; }
                        }
                    }
                }

                // Periodic sort pass (only in multi-target fullscreen).
                if sort_mode != SortMode::None && states.len() > 1
                    && last_sort.elapsed().as_millis() as u64 >= sort_interval_ms
                {
                    let now = Instant::now();
                    let rev = args.reverse_sort;
                    macro_rules! bubble_swap_f64 {
                        ($i:expr, $ka:expr, $kb:expr) => {
                            let should_swap = if rev { $ka < $kb } else { $ka > $kb };
                            if should_swap {
                                let b_slot = sort_order[$i + 1];
                                let a_slot = sort_order[$i];
                                sort_order.swap($i, $i + 1);
                                sort_arrows[b_slot] = Some((now, true));
                                sort_arrows[a_slot] = Some((now, false));
                            }
                        }
                    }
                    macro_rules! bubble_swap_u32 {
                        ($i:expr, $ka:expr, $kb:expr) => {
                            let should_swap = if rev { $ka < $kb } else { $ka > $kb };
                            if should_swap {
                                let b_slot = sort_order[$i + 1];
                                let a_slot = sort_order[$i];
                                sort_order.swap($i, $i + 1);
                                sort_arrows[b_slot] = Some((now, true));
                                sort_arrows[a_slot] = Some((now, false));
                            }
                        }
                    }
                    match sort_mode {
                        SortMode::Mtr => {
                            // Bubble-sort pass: one position per interval, fastest RTT to top.
                            // Sort key = mean reliable delivery time: win_avg / (1 - loss_fraction).
                            // Targets that are 100% lost → MAX. Targets with < 3 samples stay put.
                            let n = sort_order.len();
                            let win_sort_key = |slot: usize| -> f64 {
                                states[slot].win_mtr().unwrap_or(f64::MAX)
                            };
                            for i in 0..n.saturating_sub(1) {
                                let a = sort_order[i];
                                let b = sort_order[i + 1];
                                if states[a].total_sent < 3 || states[b].total_sent < 3 { continue; }
                                if states[a].waiting || states[b].waiting { continue; }
                                let ka = win_sort_key(a);
                                let kb = win_sort_key(b);
                                bubble_swap_f64!(i, ka, kb);
                            }
                        }
                        SortMode::Avg => {
                            // Bubble-sort pass: lowest average RTT to top.
                            // Targets with < 3 samples or 100% loss stay put.
                            let n = sort_order.len();
                            let win_sort_key = |slot: usize| -> f64 {
                                if states[slot].win_loss_pct() >= 100.0 { f64::MAX }
                                else { states[slot].win_avg() }
                            };
                            for i in 0..n.saturating_sub(1) {
                                let a = sort_order[i];
                                let b = sort_order[i + 1];
                                if states[a].total_sent < 3 || states[b].total_sent < 3 { continue; }
                                if states[a].waiting || states[b].waiting { continue; }
                                let ka = win_sort_key(a);
                                let kb = win_sort_key(b);
                                bubble_swap_f64!(i, ka, kb);
                            }
                        }
                        SortMode::Name => {
                            // Full stable sort by: mode type, then display name (label > hostname > IP).
                            let name_key = |slot: usize| -> (u8, String, u128) {
                                let type_ord = match effective[slot].0 {
                                    PingMode::Icmp  => 0u8,
                                    PingMode::Udp   => 1,
                                    PingMode::Tcp   => 2,
                                    PingMode::Tls   => 3,
                                    PingMode::Ssh   => 4,
                                    PingMode::Http  => 5,
                                    PingMode::Https => 6,
                                    PingMode::Dns   => 7,
                                    PingMode::Ntp   => 8,
                                    PingMode::Smtp  => 9,
                                    PingMode::Smtps => 10,
                                    PingMode::Exec  => 11,
                                    PingMode::Quic  => 12,
                                };
                                let display = if states[slot].custom_label {
                                    states[slot].label.to_lowercase()
                                } else if let Some(ref h) = hostnames[slot] {
                                    h.to_lowercase()
                                } else {
                                    String::new()
                                };
                                let ip_num = match states[slot].current_ip {
                                    Some(std::net::IpAddr::V4(a)) => u32::from(a) as u128,
                                    Some(std::net::IpAddr::V6(a)) => u128::from(a),
                                    None => u128::MAX,
                                };
                                (type_ord, display, ip_num)
                            };
                            if rev {
                                sort_order.sort_by(|&a, &b| name_key(b).cmp(&name_key(a)));
                            } else {
                                sort_order.sort_by(|&a, &b| name_key(a).cmp(&name_key(b)));
                            }
                        }
                        SortMode::Loss => {
                            // Bubble-sort pass: fewest packet drops to top.
                            let n = sort_order.len();
                            let win_sort_key = |slot: usize| -> f64 {
                                states[slot].win_loss_pct()
                            };
                            for i in 0..n.saturating_sub(1) {
                                let a = sort_order[i];
                                let b = sort_order[i + 1];
                                if states[a].total_sent < 3 || states[b].total_sent < 3 { continue; }
                                if states[a].waiting || states[b].waiting { continue; }
                                let ka = win_sort_key(a);
                                let kb = win_sort_key(b);
                                bubble_swap_f64!(i, ka, kb);
                            }
                        }
                        SortMode::Std => {
                            // Bubble-sort pass: lowest stddev (most consistent RTT) to top.
                            let n = sort_order.len();
                            let win_sort_key = |slot: usize| -> f64 {
                                if states[slot].win_loss_pct() >= 100.0 { f64::MAX }
                                else { states[slot].win_stddev() }
                            };
                            for i in 0..n.saturating_sub(1) {
                                let a = sort_order[i];
                                let b = sort_order[i + 1];
                                if states[a].total_sent < 3 || states[b].total_sent < 3 { continue; }
                                if states[a].waiting || states[b].waiting { continue; }
                                let ka = win_sort_key(a);
                                let kb = win_sort_key(b);
                                bubble_swap_f64!(i, ka, kb);
                            }
                        }
                        SortMode::Jitter => {
                            // Bubble-sort pass: smoothest jitter to top.
                            let n = sort_order.len();
                            let win_sort_key = |slot: usize| -> f64 {
                                if states[slot].win_loss_pct() >= 100.0 { f64::MAX }
                                else { states[slot].win_jitter_avg() }
                            };
                            for i in 0..n.saturating_sub(1) {
                                let a = sort_order[i];
                                let b = sort_order[i + 1];
                                if states[a].total_sent < 3 || states[b].total_sent < 3 { continue; }
                                if states[a].waiting || states[b].waiting { continue; }
                                let ka = win_sort_key(a);
                                let kb = win_sort_key(b);
                                bubble_swap_f64!(i, ka, kb);
                            }
                        }
                        SortMode::Streak => {
                            // Bubble-sort pass: no current drop streak first; longest streak to bottom.
                            let n = sort_order.len();
                            let win_sort_key = |slot: usize| -> u32 {
                                states[slot].cur_drop_streak
                            };
                            for i in 0..n.saturating_sub(1) {
                                let a = sort_order[i];
                                let b = sort_order[i + 1];
                                if states[a].waiting || states[b].waiting { continue; }
                                let ka = win_sort_key(a);
                                let kb = win_sort_key(b);
                                bubble_swap_u32!(i, ka, kb);
                            }
                        }
                        SortMode::Last => {
                            // Bubble-sort pass: most recently responded (host up) to top;
                            // never-up targets sink to the bottom.
                            let n = sort_order.len();
                            let win_sort_key = |slot: usize| -> f64 {
                                states[slot].last_up
                                    .map(|t| now.saturating_duration_since(t).as_secs_f64())
                                    .unwrap_or(f64::MAX)
                            };
                            for i in 0..n.saturating_sub(1) {
                                let a = sort_order[i];
                                let b = sort_order[i + 1];
                                if states[a].waiting || states[b].waiting { continue; }
                                let ka = win_sort_key(a);
                                let kb = win_sort_key(b);
                                bubble_swap_f64!(i, ka, kb);
                            }
                        }
                        SortMode::P50 | SortMode::P95 | SortMode::P99
                        | SortMode::P01 | SortMode::P10 | SortMode::Cv | SortMode::Srtt => {
                            let n = sort_order.len();
                            let win_sort_key = |slot: usize| -> f64 {
                                let s = &states[slot];
                                if s.win_loss_pct() >= 100.0 { return f64::MAX; }
                                match sort_mode {
                                    SortMode::P50  => s.win_median(),
                                    SortMode::P95  => s.win_p95(),
                                    SortMode::P99  => s.win_p99(),
                                    SortMode::P01  => s.win_p01(),
                                    SortMode::P10  => s.win_p10(),
                                    SortMode::Cv   => s.win_cv(),
                                    SortMode::Srtt => if s.srtt > 0.0 { s.srtt } else { f64::MAX },
                                    _              => unreachable!(),
                                }
                            };
                            for i in 0..n.saturating_sub(1) {
                                let a = sort_order[i];
                                let b = sort_order[i + 1];
                                if states[a].total_sent < 3 || states[b].total_sent < 3 { continue; }
                                if states[a].waiting || states[b].waiting { continue; }
                                let ka = win_sort_key(a);
                                let kb = win_sort_key(b);
                                bubble_swap_f64!(i, ka, kb);
                            }
                        }
                        SortMode::None => unreachable!(),
                    }
                    last_sort = now;
                }

                // Update EMA and trail
                let shared_scale = current_scale!(&states);
                for state in states.iter_mut() {
                    if !state.waiting && !state.last_was_drop && state.last_rtt > 0.0 && shared_scale > 0.0 {
                        const EMA_ALPHA: f64 = 0.3;
                        state.bar_ema = if state.bar_ema <= 0.0 {
                            state.last_rtt
                        } else {
                            EMA_ALPHA * state.last_rtt + (1.0 - EMA_ALPHA) * state.bar_ema
                        };
                        let frac = (state.bar_ema / shared_scale).clamp(0.0, 1.0) as f32;
                        state.trail.push_front(frac);
                        if state.trail.len() > 12 { state.trail.pop_back(); }
                    }
                }

                // Request a 1 Hz frame only when the fast cadence isn't running;
                // while it is, the fast ticker owns frame pacing and an extra
                // draw here would collide with its next frame.
                if !fast_mode_active!() { needs_frame = true; }
            }

            // Fast tick (~200 ms) - drives screensaver animation stepping, dialog
            // countdowns, and the fast frame cadence while fast_mode_active! holds.
            _ = fast_ticker.tick(), if fast_mode_active!() => {
                // Auto-dismiss toast dialogs regardless of frozen state
                if let DialogMode::FreezeNotice { dismiss_at, .. } | DialogMode::SortNotice { dismiss_at, .. } | DialogMode::ThemeNotice { dismiss_at, .. } | DialogMode::ViewNotice { dismiss_at, .. } = &mut dialog {
                    if std::time::Instant::now() >= *dismiss_at {
                        dialog = DialogMode::None;
                    }
                }
                // Tick logo animation and close help when draw-out completes
                if let DialogMode::Help { logo, .. } = &mut dialog {
                    logo.tick();
                    if logo.is_done() {
                        dialog = DialogMode::None;
                    }
                }
                // Tick the fullscreen ambient logo
                fullscreen_logo.tick();
                // Expire the graph dissolve animation in every view, so the flag
                // can't stay armed (and pin the fast cadence) in screensaver modes.
                if !frozen { tick_graph_anim(&mut states); }

                // Advance whichever screensaver is active; rendering happens in
                // draw_frame! below using the same animation-locked scale.
                if !frozen {
                    if let Some(ref mut nw) = worm_state {
                        let global_max_metric = states.iter().filter(|s| !s.waiting)
                            .map(|s| nw.metric.value(s, true)).fold(0.0f64, f64::max);
                        let (w, h) = term_size();
                        let nw_cell_w   = (w / 80).clamp(1, 4);
                        let nw_n        = states.len() as u16;
                        let nw_one_row  = w >= 120;
                        let nw_rpt      = if nw_one_row { 1u16 } else { 2 };
                        let nw_snake_h  = h.saturating_sub(nw_n * nw_rpt + 1);
                        nw.step(&states, global_max_metric, w / nw_cell_w, nw_snake_h);
                    } else if let Some(ref mut radar) = radar_state {
                        radar.step(&states);
                    } else if let Some(ref mut ekg) = ekg_state {
                        let (pw, _) = term_size();
                        ekg.push(&states, pw as usize * 2);
                    } else if let Some(ref mut pong) = pong_state {
                        let shared_scale = current_scale!(&states);
                        let global_max_jitter = states.iter().filter(|s| !s.waiting)
                            .map(|s| s.win_jitter_avg()).fold(0.0f64, f64::max);
                        let (pw, ph) = term_size();
                        let n = states.len() as u16;
                        let one_row = pw >= 120;
                        let rpt: u16 = if show_headers { if one_row { 1 } else { 2 } } else { 0 };
                        let col_keys_rows: u16 = if show_col_keys { 2 } else { 0 };
                        let field_h = ph.saturating_sub(n * rpt + col_keys_rows);
                        pong.step(&states, shared_scale, global_max_jitter, pw, field_h, pong_left_margin(&states));
                    } else if let Some(ref mut bb) = bubble_state {
                        let global_max_jitter = states.iter().filter(|s| !s.waiting)
                            .map(|s| s.win_jitter_avg()).fold(0.0f64, f64::max);
                        let (bw, bh) = term_size();
                        let n = states.len() as u16;
                        let rows_per_target: u16 = if show_headers { 1 } else { 0 };
                        let col_keys_h: u16 = if show_col_keys { 2 } else { 0 };
                        let legend_h: u16 = if n > 1 { 1 } else { 0 };
                        let bubble_h = bh.saturating_sub(n * rows_per_target + col_keys_h + legend_h);
                        bb.step(&states, global_max_jitter, bw, bubble_h, &sort_mode);
                    } else if let Some(ref mut sc) = scatter_state {
                        sc.step(&states, args.is_window());
                    }
                }

                needs_frame = true;
            }
        }

        // Centralized frame scheduler: at most one draw per loop iteration,
        // requested by whichever branch changed visible state.
        if needs_frame {
            needs_frame = false;
            draw_frame!();
        }
    }

    // Final session snapshot so the run is restartable up to the moment it ended.
    save_session!();
    // Final --summary-json snapshot with the run's last-known stats.
    save_summary_json!();

    for (_, tx) in &task_cancels { let _ = tx.send(true); }
    drop(probe_tx); // now let rx drain
    // Close any open dialog and do one final draw so the screen is clean, and so the
    // frame we are about to snapshot/preserve matches whatever the user was just looking at.
    dialog = DialogMode::None;
    // IMPORTANT (regression guard): use the exact same renderer as the live loop
    // (`draw_frame!`) for this final frame.  This MUST dispatch to every view -
    // list, graph (single + multi), worm, radar, ekg, bars, pong - and honour the
    // `frozen` snapshot.  A previous version re-implemented the dispatch here by hand
    // and only handled list/graph, so Ctrl-C from any screensaver view snapshotted a
    // *list* frame instead of the view on screen ("Ctrl-C preserve view doesn't work
    // in fullscreen views").  Routing through draw_frame! means new views are covered
    // automatically and can never regress this path again.
    //
    // draw_frame! also updates animation bookkeeping (last_drawn_scale, scale_decrease_since,
    // suppress_scale_anim) that nothing reads after this final frame - allow the dead stores.
    #[allow(unused_assignments)]
    { capture_frame = true; draw_frame!(); }
    // The frame that was just drawn, snapshotted from inside draw_frame! via the
    // CompletedFrame (NOT terminal.current_buffer_mut(), which ratatui has already reset).
    let saved_buf = captured_buf;
    // The buffer's own area carries the viewport's absolute y-offset and exact size, so we
    // derive both the inline cursor anchor and the visible content height from it.
    let inline_viewport_top = if !in_alternate_screen && keep_display {
        saved_buf.as_ref().map(|b| b.area.y).unwrap_or(0)
    } else {
        0
    };
    // How many rows the rendered frame actually fills, measured from the buffer rather
    // than from `viewport_h`.  The inline viewport gets permanently expanded by
    // `ensure_dialog_space!` the first time any dialog opens and is never shrunk back, so
    // `viewport_h` overstates the visible content height.  Using it to position the summary
    // left a block of blank lines between the list and the summary ("lots of linefeeds").
    let inline_content_rows = if !in_alternate_screen && keep_display {
        saved_buf.as_ref().map(buffer_content_rows).unwrap_or(0)
    } else {
        0
    };
    drop(terminal);
    terminal::disable_raw_mode()?;
    // Clear the TUI content so the summary prints on a clean console.
    {
        use std::io::Write;
        let mut out = io::stdout();
        // Ratatui hides the cursor during draw; restore it unconditionally since
        // process::exit() below skips destructors.
        let _ = execute!(out, cursor::Show);
        if in_alternate_screen {
            // Leave alternate screen - restores the main buffer and the cursor to
            // the position saved when EnterAlternateScreen was called.
            let _ = execute!(out, terminal::LeaveAlternateScreen);
            if keep_display {
                if let Some(buf) = saved_buf {
                    // Clear the restored main buffer and replay the last frame so the
                    // graph is visible on the console, with the summary printed below.
                    let _ = execute!(out, terminal::Clear(terminal::ClearType::All), cursor::MoveTo(0, 0));
                    out.flush().ok();
                    dump_buffer_ansi(&buf);
                }
            } else if !args.fullscreen {
                // Startup screensaver or runtime inline→fullscreen: cursor is restored to
                // the viewport top (or user's original cursor) - clear from there to end.
                let _ = execute!(out, terminal::Clear(terminal::ClearType::FromCursorDown));
            }
            // Startup --fullscreen: LeaveAlternateScreen already restored the original
            // terminal content; just fall through and print the summary there.
        } else if viewport_h > 0 {
            if keep_display {
                // Leave the inline TUI visible; jump to the row just below the rendered
                // content (not the full, possibly dialog-expanded viewport - see
                // inline_content_rows) so the summary hugs the list regardless of where
                // ratatui left the cursor or how tall the viewport grew.
                let rows = if inline_content_rows > 0 { inline_content_rows } else { viewport_h };
                let _ = execute!(out, cursor::MoveTo(0, inline_viewport_top + rows));
            } else {
                // Inline mode: erase the viewport rows.
                write!(out, "\x1b[{}A\x1b[J", viewport_h).ok();
            }
        }
        out.flush().ok();
    }
    println!();
    let exec_cmd_strs: Vec<String> = exec_cmd_arcs.iter().map(|a| (**a).clone()).collect();
    let elapsed_secs = crate::session::epoch_ms().saturating_sub(session_ctx.started_at_ms) / 1000;
    print_summary(&states, &mode_labels, &exec_cmd_strs, &deleted_states, &deleted_mode_labels, &[], &args, elapsed_secs);
    crate::logfile::write(&format!(
        "stats (final): probes_sent={} packets_received={} bytes_sent={} bytes_received={} re_resolves={} ui_updates={}",
        session_probes_sent, session_packets_received, session_bytes_sent, session_bytes_received, session_re_resolves, session_ui_updates,
    ));
    crate::logfile::write("vlat stopped, cya on the flip side");
    process::exit(0);
}


fn sync_and_tick_scale_anim(states: &mut [TargetState]) {
    for s in states.iter_mut() {
        s.scale_anim = s.scale_anim.and_then(|n| n.checked_sub(1));
        if s.scale_anim.is_none() { s.bar_anim_start = None; }
        s.threshold_flash = s.threshold_flash.saturating_sub(1);
        s.drop_flash      = s.drop_flash.saturating_sub(1);
        s.resolve_notice  = s.resolve_notice.saturating_sub(1);
    }
}

fn trigger_scale_anim(states: &mut [TargetState], shared_scale: f64, last_drawn: &mut f64) {
    let no_anim = !states.iter().any(|s| !s.waiting && s.scale_anim.is_some());
    if no_anim && *last_drawn > 0.0 && shared_scale != *last_drawn {
        let scale_up = shared_scale > *last_drawn;
        for s in states.iter_mut() {
            if !s.waiting {
                s.scale_anim_old  = *last_drawn;
                s.scale_anim_new  = shared_scale;
                s.scale_anim_up   = scale_up;
                s.scale_anim      = Some(8);
                s.bar_anim_start  = Some(Instant::now());
                s.graph_anim_start = Some(Instant::now());
            }
        }
    } else if !no_anim {
        // Animation in progress: if the new scale is higher than the current
        // target, retarget the in-flight animation rather than blocking it.
        // Capture the current interpolated position as the new start so the
        // bar continues smoothly from where it is toward the higher target.
        let needs_retarget = states.iter()
            .any(|s| !s.waiting && s.scale_anim.is_some() && shared_scale > s.scale_anim_new);
        if needs_retarget {
            let now = Instant::now();
            for s in states.iter_mut() {
                if !s.waiting && s.scale_anim.is_some() {
                    let elapsed = s.bar_anim_start.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
                    let frac    = (elapsed / crate::constants::BAR_ANIM_SECS).clamp(0.0, 1.0);
                    s.scale_anim_old = s.scale_anim_old + (s.scale_anim_new - s.scale_anim_old) * frac;
                    s.scale_anim_new = shared_scale;
                    s.scale_anim_up  = true;
                    s.scale_anim     = Some(8);
                    s.bar_anim_start = Some(now);
                    // graph_anim_start intentionally not reset
                }
            }
        }
    }
    *last_drawn = shared_scale;
}

fn tick_graph_anim(states: &mut [TargetState]) {
    let duration = Duration::from_secs_f64(crate::constants::GRAPH_ANIM_SECS);
    for s in states.iter_mut() {
        if let Some(start) = s.graph_anim_start {
            if start.elapsed() >= duration {
                s.graph_anim_start = None;
            }
        }
    }
}

/// Return the number of rows from the top of `buf` up to and including the last row
/// that contains a non-blank cell.  On exit this lets us place the summary directly
/// below the visible content instead of below an over-tall viewport (the inline list
/// viewport is permanently expanded by ensure_dialog_space! the first time a dialog
/// opens and never shrinks back).  Returns 0 for a completely blank buffer.
fn buffer_content_rows(buf: &ratatui::buffer::Buffer) -> u16 {
    let area = buf.area;
    let mut rows = 0u16;
    for y in area.top()..area.bottom() {
        let has_content = (area.left()..area.right()).any(|x| {
            buf.cell((x, y)).is_some_and(|c| {
                let s = c.symbol();
                !s.is_empty() && s != " "
            })
        });
        if has_content {
            rows = y - area.top() + 1;
        }
    }
    rows
}

/// Replay a ratatui buffer to stdout using raw ANSI escape codes.
/// Used by the Ctrl-C / Ctrl-Q "quit keeping display" path to print the last TUI frame
/// to the main screen buffer after leaving the alternate screen.
fn dump_buffer_ansi(buf: &ratatui::buffer::Buffer) {
    use std::fmt::Write as FmtWrite;
    use std::io::Write;
    use ratatui::style::{Color, Modifier};
    let mut out = io::stdout();

    let area = buf.area;
    let mut line = String::with_capacity(area.width as usize * 20);

    // Trim trailing blank rows so the summary prints immediately below the visible
    // content rather than after a band of empty lines (and so a short frame doesn't
    // needlessly scroll the whole terminal on exit).
    let bottom = area.top() + buffer_content_rows(buf);
    for y in area.top()..bottom {
        line.clear();
        for x in area.left()..area.right() {
            let Some(cell) = buf.cell((x, y)) else { continue };
            if cell.diff_option == ratatui::buffer::CellDiffOption::Skip { continue; }
            line.push_str("\x1b[0m");
            match cell.fg {
                Color::Reset        => {}
                Color::Black        => line.push_str("\x1b[30m"),
                Color::Red          => line.push_str("\x1b[31m"),
                Color::Green        => line.push_str("\x1b[32m"),
                Color::Yellow       => line.push_str("\x1b[33m"),
                Color::Blue         => line.push_str("\x1b[34m"),
                Color::Magenta      => line.push_str("\x1b[35m"),
                Color::Cyan         => line.push_str("\x1b[36m"),
                Color::Gray         => line.push_str("\x1b[37m"),
                Color::DarkGray     => line.push_str("\x1b[90m"),
                Color::LightRed     => line.push_str("\x1b[91m"),
                Color::LightGreen   => line.push_str("\x1b[92m"),
                Color::LightYellow  => line.push_str("\x1b[93m"),
                Color::LightBlue    => line.push_str("\x1b[94m"),
                Color::LightMagenta => line.push_str("\x1b[95m"),
                Color::LightCyan    => line.push_str("\x1b[96m"),
                Color::White        => line.push_str("\x1b[97m"),
                Color::Rgb(r, g, b) => { let _ = write!(line, "\x1b[38;2;{r};{g};{b}m"); }
                Color::Indexed(n)   => { let _ = write!(line, "\x1b[38;5;{n}m"); }
            }
            match cell.bg {
                Color::Reset        => {}
                Color::Black        => line.push_str("\x1b[40m"),
                Color::Red          => line.push_str("\x1b[41m"),
                Color::Green        => line.push_str("\x1b[42m"),
                Color::Yellow       => line.push_str("\x1b[43m"),
                Color::Blue         => line.push_str("\x1b[44m"),
                Color::Magenta      => line.push_str("\x1b[45m"),
                Color::Cyan         => line.push_str("\x1b[46m"),
                Color::Gray         => line.push_str("\x1b[47m"),
                Color::DarkGray     => line.push_str("\x1b[100m"),
                Color::LightRed     => line.push_str("\x1b[101m"),
                Color::LightGreen   => line.push_str("\x1b[102m"),
                Color::LightYellow  => line.push_str("\x1b[103m"),
                Color::LightBlue    => line.push_str("\x1b[104m"),
                Color::LightMagenta => line.push_str("\x1b[105m"),
                Color::LightCyan    => line.push_str("\x1b[106m"),
                Color::White        => line.push_str("\x1b[107m"),
                Color::Rgb(r, g, b) => { let _ = write!(line, "\x1b[48;2;{r};{g};{b}m"); }
                Color::Indexed(n)   => { let _ = write!(line, "\x1b[48;5;{n}m"); }
            }
            let m = cell.modifier;
            if m.contains(Modifier::BOLD)        { line.push_str("\x1b[1m"); }
            if m.contains(Modifier::DIM)         { line.push_str("\x1b[2m"); }
            if m.contains(Modifier::ITALIC)      { line.push_str("\x1b[3m"); }
            if m.contains(Modifier::UNDERLINED)  { line.push_str("\x1b[4m"); }
            if m.contains(Modifier::SLOW_BLINK)  { line.push_str("\x1b[5m"); }
            if m.contains(Modifier::RAPID_BLINK) { line.push_str("\x1b[6m"); }
            if m.contains(Modifier::REVERSED)    { line.push_str("\x1b[7m"); }
            if m.contains(Modifier::HIDDEN)      { line.push_str("\x1b[8m"); }
            if m.contains(Modifier::CROSSED_OUT) { line.push_str("\x1b[9m"); }
            line.push_str(cell.symbol());
        }
        line.push_str("\x1b[0m\r\n");
        out.write_all(line.as_bytes()).ok();
    }
    out.flush().ok();
}

fn fmt_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 { out.push(','); }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn fmt_recv(s: &TargetState) -> String {
    if s.total_sent == 0 {
        return "0".to_string();
    }
    let received = s.total_sent.saturating_sub(s.drops as u64);
    let pct = received as f64 / s.total_sent as f64 * 100.0;
    format!("{} ({:.1}%)", fmt_thousands(received), pct)
}

/// Print the end-of-session summary for a saved session file - the picker's
/// "summary & exit" action.  Rebuilds minimal TargetStates from the saved
/// aggregates so the output matches what quitting the live run printed.
pub fn print_saved_summary(sess: &crate::session::SessionFile, args: &Args) {
    if sess.target_data.is_empty() {
        println!("vlat: session has no recorded probe data");
        return;
    }
    let mut states      = Vec::with_capacity(sess.target_data.len());
    let mut mode_labels = Vec::with_capacity(sess.target_data.len());
    let mut exec_cmds   = Vec::with_capacity(sess.target_data.len());
    for d in &sess.target_data {
        let mut s = TargetState::new(d.label.clone());
        s.custom_label    = d.custom_label;
        s.exec_cmd        = d.exec_cmd.clone();
        s.total_sent      = d.total_sent;
        s.drops           = d.drops;
        s.dups            = d.dups;
        s.latency_sum     = d.latency_sum;
        s.latency_sq_sum  = d.latency_sq_sum;
        s.jitter_sum      = d.jitter_sum;
        s.jitter_count    = d.jitter_count;
        s.lifetime_min    = d.lifetime_min;
        s.lifetime_max    = d.lifetime_max;
        s.max_drop_streak = d.max_drop_streak;
        s.ip_changes      = d.ip_changes;
        s.srtt            = d.srtt;
        s.current_ip      = d.addr.as_deref().and_then(|a| a.parse().ok());
        mode_labels.push(if d.mode_label.is_empty() { "?".to_string() } else { d.mode_label.clone() });
        exec_cmds.push(d.exec_cmd.clone());
        states.push(s);
    }
    let nc  = args.no_color;
    let dim = |s: &str| -> String { if nc { s.to_string() } else { format!("\x1b[2m{}\x1b[0m", s) } };
    let name = sess.name.as_deref().unwrap_or("(unnamed)");
    println!("{}", dim(&format!("  session {} - saved {}", name, sess.saved_at)));
    let elapsed_secs = sess.saved_at_ms().saturating_sub(sess.started_at_ms()) / 1000;
    print_summary(&states, &mode_labels, &exec_cmds, &[], &[], &[], args, elapsed_secs);
}

/// A single wrapped-table column. Most items are a single sub-column (e.g.
/// "avg"); a few are an atomic multi-column unit that must never be split
/// across a wrap boundary (e.g. "min"+"max", which read as one "range").
struct Item {
    labels: Vec<String>,
    widths: Vec<usize>,
    /// cells[sub][row] - already right-padded to widths[sub] and colored.
    cells: Vec<Vec<String>>,
    /// totals[sub] - already right-padded to widths[sub] and colored, or
    /// None where a total isn't meaningful for this column.
    totals: Vec<Option<String>>,
}

impl Item {
    fn width(&self) -> usize {
        self.widths.iter().sum::<usize>() + 2 * self.widths.len().saturating_sub(1)
    }
}

/// Packs items left-to-right into as many groups ("physical lines") as needed
/// so each group's total width fits `budget`. A group always gets at least
/// one item even if that item alone exceeds budget, so an item is never
/// dropped outright - only ever pushed onto its own line.
fn pack_groups(widths: &[usize], budget: usize) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_w = 0usize;
    for (i, &iw) in widths.iter().enumerate() {
        let add_w = iw + if cur.is_empty() { 0 } else { 2 };
        if !cur.is_empty() && budget != usize::MAX && cur_w + add_w > budget {
            groups.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        cur.push(i);
        cur_w += iw + if cur.len() == 1 { 0 } else { 2 };
    }
    if !cur.is_empty() { groups.push(cur); }
    groups
}

/// Renders one physical line for a group of items: every sub-column of every
/// item in the group, joined by two-space gaps. `cell` supplies the already
/// padded/colored text for a given (item index, sub-column index).
fn render_group(items: &[Item], group: &[usize], cell: &dyn Fn(usize, usize) -> String) -> String {
    let mut line = String::new();
    for &gi in group {
        if !line.is_empty() { line.push_str("  "); }
        for si in 0..items[gi].widths.len() {
            if si > 0 { line.push_str("  "); }
            line.push_str(&cell(gi, si));
        }
    }
    line
}

/// Human-readable elapsed time, e.g. "3s", "12m 04s", "1h 03m 12s", "2d 01h 00m 09s".
fn fmt_duration(total_secs: u64) -> String {
    let days  = total_secs / 86_400;
    let hours = (total_secs % 86_400) / 3_600;
    let mins  = (total_secs % 3_600) / 60;
    let secs  = total_secs % 60;
    if days > 0 {
        format!("{}d {:02}h {:02}m {:02}s", days, hours, mins, secs)
    } else if hours > 0 {
        format!("{}h {:02}m {:02}s", hours, mins, secs)
    } else if mins > 0 {
        format!("{}m {:02}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}

/// Prints the end-of-session summary: the "standard" identity + delivery
/// columns plus whatever extended stat columns the user enabled (minus the
/// visual-only `bar`/`recent` columns and the live "up" status badge, none
/// of which make sense in a static console dump). Columns that don't fit the
/// terminal width wrap onto extra, indented lines per row rather than being
/// dropped, so nothing that was requested silently disappears.
fn print_summary(states: &[TargetState], mode_labels: &[String], exec_cmds: &[String], deleted: &[TargetState], deleted_mode_labels: &[String], deleted_exec_cmds: &[String], args: &Args, elapsed_secs: u64) {
    use ratatui::style::Color;
    let nc    = args.no_color;
    let theme = &args.theme;

    // ANSI helpers - all return plain strings when no_color is set.
    let dim  = |s: &str| -> String { if nc { s.to_string() } else { format!("\x1b[2m{}\x1b[0m", s) } };
    let bold = |s: &str| -> String { if nc { s.to_string() } else { format!("\x1b[1m{}\x1b[0m", s) } };
    let rgb  = |r: u8, g: u8, b: u8, s: &str| -> String {
        if nc { s.to_string() } else { format!("\x1b[38;2;{};{};{}m{}\x1b[0m", r, g, b, s) }
    };
    // Apply a ratatui Color as an ANSI foreground escape.
    let color_ansi = |c: Color, s: &str| -> String {
        if nc { return s.to_string(); }
        match c {
            Color::Rgb(r, g, b) => format!("\x1b[38;2;{};{};{}m{}\x1b[0m", r, g, b, s),
            Color::Red          => format!("\x1b[31m{}\x1b[0m", s),
            Color::Yellow       => format!("\x1b[33m{}\x1b[0m", s),
            Color::Green        => format!("\x1b[32m{}\x1b[0m", s),
            Color::Cyan         => format!("\x1b[36m{}\x1b[0m", s),
            Color::White        => format!("\x1b[97m{}\x1b[0m", s),
            _                   => format!("\x1b[37m{}\x1b[0m", s),
        }
    };
    // Same, but bold - matches the live stats view's name-column style
    // (`Style::default().fg(color).add_modifier(Modifier::BOLD)`).
    let bold_color_ansi = |c: Color, s: &str| -> String {
        if nc { return s.to_string(); }
        match c {
            Color::Rgb(r, g, b) => format!("\x1b[1;38;2;{};{};{}m{}\x1b[0m", r, g, b, s),
            Color::Red          => format!("\x1b[1;31m{}\x1b[0m", s),
            Color::Yellow       => format!("\x1b[1;33m{}\x1b[0m", s),
            Color::Green        => format!("\x1b[1;32m{}\x1b[0m", s),
            Color::Cyan         => format!("\x1b[1;36m{}\x1b[0m", s),
            Color::White        => format!("\x1b[1;97m{}\x1b[0m", s),
            _                   => format!("\x1b[1;37m{}\x1b[0m", s),
        }
    };

    // Color an avg-latency value using the theme gradient.
    let latency_color = |ms: f64, text: &str| -> String {
        if nc || text == "—" { return text.to_string(); }
        let (lr, lg, lb) = theme.grad_low;
        let (mr, mg, mb) = theme.grad_mid;
        let (hr, hg, hb) = theme.grad_high;
        if      ms <  50.0 { rgb(lr, lg, lb, text) }
        else if ms < 150.0 { rgb(mr, mg, mb, text) }
        else               { rgb(hr, hg, hb, text) }
    };

    // Color a protocol mode string using theme mode colors - matches the live
    // stats view's mode badge (`Style::default().fg(mode_color).add_modifier(Modifier::DIM)`).
    let fmt_type = |proto: &str, width: usize| -> String {
        let padded = format!("{:<width$}", proto, width=width);
        if nc { return padded; }
        let (r, g, b) = match proto {
            "icmp"  => theme.mode_icmp,
            "udp"   => theme.mode_udp,
            "tcp"   => theme.mode_tcp,
            "http"  => theme.mode_http,
            "https" => theme.mode_https,
            "dns"   => theme.mode_dns,
            "tls"   => theme.mode_tls,
            _       => theme.mode_other,
        };
        format!("\x1b[2;38;2;{};{};{}m{}\x1b[0m", r, g, b, padded)
    };

    // Build a combined list: (state_ref, mode_label, was_deleted, exec_cmd)
    let empty_exec = String::new();
    let all: Vec<(&TargetState, &str, bool, &str)> =
        states.iter().zip(mode_labels.iter()).zip(exec_cmds.iter())
            .map(|((s, m), ec)| (s, m.as_str(), false, ec.as_str()))
        .chain(
            deleted.iter().zip(deleted_mode_labels.iter())
                .zip(deleted_exec_cmds.iter().chain(std::iter::repeat(&empty_exec)))
                .map(|((s, m), ec)| (s, m.as_str(), true, ec.as_str()))
        )
        .collect();

    // Per-target identity color for the name column - matches the live stats
    // view: a cycling palette color per target when multiple targets are
    // shown, or the single-target hostname color otherwise.
    let all_len = all.len();
    let ident_color = |idx: usize| -> Color {
        if all_len > 1 {
            let (r, g, b) = theme.target_color(idx);
            Color::Rgb(r, g, b)
        } else {
            theme.hostname
        }
    };

    if all.is_empty() { return; }

    let any_deleted = !deleted.is_empty();
    let term_w = terminal::size().map(|(w, _)| w as usize).unwrap_or(usize::MAX);

    // Compute identity-column widths across all entries (including deleted).
    let del_tag     = "  [deleted]";
    let del_tag_w   = if any_deleted { del_tag.len() } else { 0 };
    let display_label_len = |s: &TargetState| -> usize {
        if !s.custom_label {
            if let Some(i) = s.label.rfind(" (") { i } else { s.label.len() }
        } else {
            s.label.len()
        }
    };
    let mut label_width = all.iter().map(|(s, _, _, _)| display_label_len(s)).max().unwrap_or(0);
    let type_w      = all.iter().map(|(_, m, _, _)| m.split(':').next().unwrap_or(*m).len()).max().unwrap_or(1).max("type".len());
    let show_port   = all.iter().any(|(_, m, _, _)| m.contains(':'));
    let port_w      = if show_port {
        all.iter().map(|(_, m, _, _)| if let Some(i) = m.find(':') { m[i+1..].len() } else { 0 })
            .max().unwrap_or(0).max("port".len())
    } else { 0 };
    let addr_txt = |s: &TargetState| -> String {
        match s.current_ip {
            Some(ip) => ip.to_string(),
            None => String::new(),
        }
    };
    let addr_w = all.iter()
        .filter(|(_, m, _, _)| *m != "exec")
        .map(|(s, _, _, _)| addr_txt(s).len())
        .max().unwrap_or(0)
        .max("address".len());
    let rule_char = if args.ascii { "-" } else { "\u{2500}" };

    // Totals: sent/received/loss/dup summed across every row (including
    // deleted targets, which are part of the printed table too).
    let total_sent_all: u64  = all.iter().map(|(s, _, _, _)| s.total_sent).sum();
    let total_drops_all: u64 = all.iter().map(|(s, _, _, _)| s.drops as u64).sum();
    let total_dups_all: u64  = all.iter().map(|(s, _, _, _)| s.dups as u64).sum();
    let total_recv_all: u64  = total_sent_all.saturating_sub(total_drops_all);
    let total_sent_txt = fmt_thousands(total_sent_all);
    let total_recv_txt = if total_sent_all == 0 {
        "0".to_string()
    } else {
        format!("{} ({:.1}%)", fmt_thousands(total_recv_all), total_recv_all as f64 / total_sent_all as f64 * 100.0)
    };
    let total_loss_txt = if total_drops_all > 0 {
        format!("{} ({:.1}%)", fmt_thousands(total_drops_all), total_drops_all as f64 / total_sent_all.max(1) as f64 * 100.0)
    } else {
        fmt_thousands(total_drops_all)
    };
    let total_dup_txt = if total_dups_all > 0 {
        format!("{} ({:.1}%)", fmt_thousands(total_dups_all), total_dups_all as f64 / total_sent_all.max(1) as f64 * 100.0)
    } else {
        fmt_thousands(total_dups_all)
    };
    let show_totals = all.len() > 1;

    let col_w  = 8usize;
    let chg_w  = 7usize;
    let any_loss = all.iter().any(|(s, _, _, _)| s.drops > 0);
    let any_dups = all.iter().any(|(s, _, _, _)| s.dups  > 0);
    let loss_txt = |s: &TargetState| -> String {
        if any_loss && s.drops > 0 {
            format!("{} ({:.1}%)", fmt_thousands(s.drops as u64), s.life_loss_pct())
        } else {
            fmt_thousands(s.drops as u64)
        }
    };
    let dup_txt  = |s: &TargetState| -> String {
        if any_dups && s.dups > 0 {
            let pct = s.dups as f64 / s.total_sent as f64 * 100.0;
            format!("{} ({:.1}%)", fmt_thousands(s.dups as u64), pct)
        } else {
            fmt_thousands(s.dups as u64)
        }
    };
    let any_mtr_data = all.iter().any(|(s, _, _, _)| s.win_mtr().is_some());
    let any_changes  = all.iter().any(|(s, _, _, _)| s.ip_changes > 0);

    // Column visibility. Base stats respect args.hidden_base_stats (toggled
    // via 'x' at runtime). Extra stats: the summary always includes the
    // original default set (std, plus mtr when mtr data exists) and adds
    // whatever else --columns explicitly requested on top.
    use crate::cli::{BaseStat, ExtraStat};
    let es = &args.extra_stats;
    let has_extra = |s: &ExtraStat| es.contains(s);

    let show_avg     = !args.hidden_base_stats.contains(&BaseStat::Avg);
    let show_range   = !args.hidden_base_stats.contains(&BaseStat::Range); // min + max
    let show_jitter  = !args.hidden_base_stats.contains(&BaseStat::Jitter);
    let show_std     = true;
    let show_mtr     = any_mtr_data || has_extra(&ExtraStat::Mtr);
    let show_p01     = has_extra(&ExtraStat::P01);
    let show_p10     = has_extra(&ExtraStat::P10);
    let show_p50     = has_extra(&ExtraStat::P50);
    let show_p95     = has_extra(&ExtraStat::P95);
    let show_p99     = has_extra(&ExtraStat::P99);
    let show_cv      = has_extra(&ExtraStat::Cv);
    let show_srtt    = has_extra(&ExtraStat::Srtt);
    let show_streak  = has_extra(&ExtraStat::Streak);
    let show_changes = any_changes;
    // ExtraStat::Bar and ExtraStat::Recent (the live view's inline range bar
    // and per-probe sparkline) and the live "up" status badge are widgets,
    // not tabular data - deliberately never rendered here.

    // Narrow-terminal fallback for the identity columns themselves (mode/name/
    // addr/port): these are never wrapped onto a second line since they're
    // what identifies the row, so as a last resort the label gets truncated.
    let ident_w_full = 2 + type_w + 2 + label_width + 2 + addr_w + if show_port { 2 + port_w } else { 0 };
    if term_w != usize::MAX && ident_w_full + 2 > term_w {
        let excess = (ident_w_full + 2).saturating_sub(term_w);
        label_width = label_width.saturating_sub(excess).max(4);
    }
    let ident_w = 2 + type_w + 2 + label_width + 2 + addr_w + if show_port { 2 + port_w } else { 0 };

    let ell   = if args.ascii { "..." } else { "\u{2026}" };
    let ell_w = ell.chars().count();

    // label column — strip the " (ip)" suffix; truncate with ellipsis if
    // label_width was reduced to fit a narrow terminal.
    let fmt_label = |s: &TargetState| -> (String, usize) {
        let display_label = if !s.custom_label {
            if let Some(i) = s.label.rfind(" (") { &s.label[..i] } else { &s.label }
        } else {
            &s.label
        };
        let vis_len = display_label.chars().count();
        if vis_len > label_width {
            let take = label_width.saturating_sub(ell_w);
            let s: String = display_label.chars().take(take).collect();
            (format!("{}{}", s, ell), label_width)
        } else {
            (display_label.to_string(), vis_len)
        }
    };

    // Formats one extended-stat value by column name.
    let fmt_ext_val = |name: &str, s: &TargetState| -> String {
        let no_data    = s.total_sent == 0;
        let all_lost   = s.total_sent > 0 && s.drops as u64 == s.total_sent;
        let has_window = !s.window.is_empty();
        let fmt_ms     = |v: f64| if no_data || all_lost { "—".to_string() } else { format!("{:.1}ms", v) };
        match name {
            "stddev" => fmt_ms(s.life_stddev()),
            "mtr"    => s.win_mtr().map(|m| format!("{:.1}ms", m))
                            .unwrap_or_else(|| if all_lost { "∞".to_string() } else { "—".to_string() }),
            "p01"    => if has_window { format!("{:.1}ms", s.win_p01())     } else { "—".to_string() },
            "p10"    => if has_window { format!("{:.1}ms", s.win_p10())     } else { "—".to_string() },
            "p50"    => if has_window { format!("{:.1}ms", s.win_median())  } else { "—".to_string() },
            "p95"    => if has_window { format!("{:.1}ms", s.win_p95())     } else { "—".to_string() },
            "p99"    => if has_window { format!("{:.1}ms", s.win_p99())     } else { "—".to_string() },
            "cv%"    => {
                let cv = s.life_cv();
                if cv <= 0.0 { "—".to_string() }
                else if cv < 100.0 { format!("{:.1}%", cv) } else { format!("{:.0}%", cv) }
            }
            "srtt"   => if s.srtt > 0.0 { format!("{:.1}ms", s.srtt) } else { "—".to_string() },
            "streak" => if s.max_drop_streak > 0 { fmt_thousands(s.max_drop_streak as u64) } else { "—".to_string() },
            _ => String::new(),
        }
    };
    let fmt_ms = |s: &TargetState, v: f64| -> String {
        let no_data  = s.total_sent == 0;
        let all_lost = s.total_sent > 0 && s.drops as u64 == s.total_sent;
        if no_data || all_lost { "—".to_string() } else { format!("{:.1}ms", v) }
    };

    // ── Build the wrapped stat columns ──────────────────────────────────────
    // Every non-identity column (core stats + whatever extended stats are
    // enabled) is built as an `Item` up front, then packed onto as many
    // physical lines as the terminal width allows. This is the same
    // left-to-right greedy packing this file already used just for the
    // extended stats; it now covers every stat column so none of them get
    // silently dropped on a narrow terminal - they wrap and stagger instead.
    let mut items: Vec<Item> = Vec::new();

    if show_avg {
        let cells: Vec<String> = all.iter().map(|(s, _, _, _)| {
            let padded = format!("{:>w$}", fmt_ms(s, s.avg_latency()), w=col_w);
            latency_color(s.avg_latency(), &padded)
        }).collect();
        items.push(Item { labels: vec!["avg".into()], widths: vec![col_w], cells: vec![cells], totals: vec![None] });
    }
    if show_jitter {
        let cells: Vec<String> = all.iter().map(|(s, _, _, _)| format!("{:>w$}", fmt_ms(s, s.avg_jitter()), w=col_w)).collect();
        items.push(Item { labels: vec!["jitter".into()], widths: vec![col_w], cells: vec![cells], totals: vec![None] });
    }
    if show_range {
        let min_cells: Vec<String> = all.iter().map(|(s, _, _, _)| format!("{:>w$}", fmt_ms(s, s.life_min()), w=col_w)).collect();
        let max_cells: Vec<String> = all.iter().map(|(s, _, _, _)| format!("{:>w$}", fmt_ms(s, s.life_max()), w=col_w)).collect();
        items.push(Item {
            labels: vec!["min".into(), "max".into()],
            widths: vec![col_w, col_w],
            cells: vec![min_cells, max_cells],
            totals: vec![None, None],
        });
    }
    {
        let vals: Vec<String> = all.iter().map(|(s, _, _, _)| fmt_thousands(s.total_sent)).collect();
        let w = vals.iter().map(|v| v.len()).max().unwrap_or(1).max("sent".len()).max(total_sent_txt.len());
        let cells: Vec<String> = vals.iter().map(|v| format!("{:>w$}", v, w=w)).collect();
        let totals = vec![if show_totals { Some(format!("{:>w$}", total_sent_txt, w=w)) } else { None }];
        items.push(Item { labels: vec!["sent".into()], widths: vec![w], cells: vec![cells], totals });
    }
    {
        let vals: Vec<String> = all.iter().map(|(s, _, _, _)| fmt_recv(s)).collect();
        let w = vals.iter().map(|v| v.len()).max().unwrap_or(1).max("received".len()).max(total_recv_txt.len());
        let cells: Vec<String> = all.iter().zip(vals.iter()).map(|((s, _, _, _), v)| {
            let padded = format!("{:>w$}", v, w=w);
            let all_lost = s.total_sent > 0 && s.drops as u64 == s.total_sent;
            if all_lost { let (dr, dg, db) = theme.drop_marker; rgb(dr, dg, db, &padded) } else { padded }
        }).collect();
        let totals = vec![if show_totals { Some(format!("{:>w$}", total_recv_txt, w=w)) } else { None }];
        items.push(Item { labels: vec!["received".into()], widths: vec![w], cells: vec![cells], totals });
    }
    {
        let vals: Vec<String> = all.iter().map(|(s, _, _, _)| loss_txt(s)).collect();
        let w = vals.iter().map(|v| v.len()).max().unwrap_or(1).max("loss".len()).max(total_loss_txt.len());
        let cells: Vec<String> = all.iter().zip(vals.iter()).map(|((s, _, _, _), v)| {
            let padded = format!("{:>w$}", v, w=w);
            if s.drops > 0 { let (dr, dg, db) = theme.drop_marker; rgb(dr, dg, db, &padded) } else { dim(&padded) }
        }).collect();
        let totals = vec![if show_totals {
            let padded = format!("{:>w$}", total_loss_txt, w=w);
            Some(if total_drops_all > 0 { let (dr, dg, db) = theme.drop_marker; rgb(dr, dg, db, &padded) } else { dim(&padded) })
        } else { None }];
        items.push(Item { labels: vec!["loss".into()], widths: vec![w], cells: vec![cells], totals });
    }
    {
        let vals: Vec<String> = all.iter().map(|(s, _, _, _)| dup_txt(s)).collect();
        let w = vals.iter().map(|v| v.len()).max().unwrap_or(1).max("dup".len()).max(total_dup_txt.len());
        let cells: Vec<String> = all.iter().zip(vals.iter()).map(|((s, _, _, _), v)| {
            let padded = format!("{:>w$}", v, w=w);
            if s.dups > 0 { color_ansi(theme.rtt_warn, &padded) } else { dim(&padded) }
        }).collect();
        let totals = vec![if show_totals {
            let padded = format!("{:>w$}", total_dup_txt, w=w);
            Some(if total_dups_all > 0 { color_ansi(theme.rtt_warn, &padded) } else { dim(&padded) })
        } else { None }];
        items.push(Item { labels: vec!["dup".into()], widths: vec![w], cells: vec![cells], totals });
    }
    if show_changes {
        let w = chg_w;
        let cells: Vec<String> = all.iter().map(|(s, _, _, _)| {
            if s.ip_changes > 0 {
                let padded = format!("{:>w$}", s.ip_changes, w=w);
                color_ansi(theme.ip_change, &padded)
            } else {
                dim(&format!("{:>w$}", "-", w=w))
            }
        }).collect();
        items.push(Item { labels: vec!["changes".into()], widths: vec![w], cells: vec![cells], totals: vec![None] });
    }
    for (name, on) in [
        ("stddev", show_std), ("mtr", show_mtr), ("p01", show_p01), ("p10", show_p10),
        ("p50", show_p50), ("p95", show_p95), ("p99", show_p99), ("cv%", show_cv),
        ("srtt", show_srtt), ("streak", show_streak),
    ] {
        if !on { continue; }
        let vals: Vec<String> = all.iter().map(|(s, _, _, _)| fmt_ext_val(name, s)).collect();
        let w = vals.iter().map(|v| v.chars().count()).max().unwrap_or(0).max(name.len());
        let cells: Vec<String> = vals.iter().map(|v| format!("{:>w$}", v, w=w)).collect();
        items.push(Item { labels: vec![name.to_string()], widths: vec![w], cells: vec![cells], totals: vec![None] });
    }

    let item_widths: Vec<usize> = items.iter().map(|it| it.width()).collect();
    let budget = if term_w == usize::MAX { usize::MAX } else { term_w.saturating_sub(ident_w + 2) };
    let groups = pack_groups(&item_widths, budget);
    let indent = " ".repeat(ident_w + 2);

    let group_widths: Vec<usize> = groups.iter().map(|g| {
        g.iter().map(|&gi| items[gi].width()).sum::<usize>() + 2 * g.len().saturating_sub(1)
    }).collect();
    let max_group_w = group_widths.iter().copied().max().unwrap_or(0);
    let del_w = if any_deleted { del_tag_w } else { 0 };
    let rule_width = ident_w + del_w + if groups.is_empty() { 0 } else { 2 + max_group_w };

    // ── Header ───────────────────────────────────────────────────────────
    let mut header_line1 = format!("  {:<tw$}  {:<lw$}  {:>aw$}", "mode", "name", "addr", tw=type_w, lw=label_width, aw=addr_w);
    if show_port { header_line1.push_str(&format!("  {:>pw$}", "port", pw=port_w)); }
    if let Some(group) = groups.first() {
        header_line1.push_str("  ");
        header_line1.push_str(&render_group(&items, group, &|gi, si| format!("{:>w$}", items[gi].labels[si], w=items[gi].widths[si])));
    }
    if any_deleted { header_line1.push_str("  status"); }
    println!("{}", dim(&header_line1));
    for group in groups.iter().skip(1) {
        let line = render_group(&items, group, &|gi, si| format!("{:>w$}", items[gi].labels[si], w=items[gi].widths[si]));
        println!("{}{}", indent, dim(&line));
    }

    // ── Separator: header section vs. data section ──────────────────────
    // rule_width includes the leading 2-space indent (via ident_w), so the
    // dashes only need to fill the rest, right after the printed "  ".
    println!("  {}", dim(&rule_char.repeat(rule_width.saturating_sub(2))));

    // ── Data rows ─────────────────────────────────────────────────────────
    for (row_idx, (s, mode, was_deleted, exec_cmd)) in all.iter().enumerate() {
        // Split mode into protocol and optional non-standard port.
        let (mode_proto, mode_port) = if let Some(i) = mode.find(':') {
            (&mode[..i], &mode[i+1..])
        } else {
            (*mode, "")
        };
        let is_exec = mode_proto == "exec";

        let f_type = fmt_type(mode_proto, type_w);
        let (label_str, label_vis) = fmt_label(s);
        let pad     = label_width.saturating_sub(label_vis);
        let f_label = format!("{}{}", bold_color_ansi(ident_color(row_idx), &label_str), " ".repeat(pad));

        // address column (and optional port column); exec merges both into a single command display
        let (f_addr, f_port_col) = if is_exec {
            let merge_w = addr_w + if show_port { 2 + port_w } else { 0 };
            let cmd_chars: Vec<char> = exec_cmd.chars().collect();
            let displayed = if cmd_chars.len() <= merge_w {
                format!("{:<width$}", exec_cmd, width=merge_w)
            } else {
                let take = merge_w.saturating_sub(ell_w);
                format!("{}{}", cmd_chars[..take].iter().collect::<String>(), ell)
            };
            (format!("  {}", dim(&displayed)), String::new())
        } else {
            let txt = addr_txt(s);
            let padded = format!("{:>aw$}", txt, aw=addr_w);
            let f_p = if show_port { format!("  {:>pw$}", mode_port, pw=port_w) } else { String::new() };
            (format!("  {}", dim(&padded)), f_p)
        };

        let mut line1 = format!("  {}  {}", f_type, f_label);
        line1.push_str(&f_addr);
        line1.push_str(&f_port_col);
        if let Some(group) = groups.first() {
            line1.push_str("  ");
            line1.push_str(&render_group(&items, group, &|gi, si| items[gi].cells[si][row_idx].clone()));
        }
        if any_deleted {
            line1.push_str(&if *was_deleted { dim(del_tag) } else { String::new() });
        }
        println!("{}", line1);

        for group in groups.iter().skip(1) {
            let line = render_group(&items, group, &|gi, si| items[gi].cells[si][row_idx].clone());
            println!("{}{}", indent, line);
        }
    }

    // ── Totals: sums the delivery columns so a multi-target run has an
    // at-a-glance health check without adding up rows by eye. Skipped for a
    // single target, where it would just repeat that one row.
    if show_totals {
        println!("  {}", dim(&rule_char.repeat(rule_width.saturating_sub(2))));

        let totals_label = "totals";
        let totals_pad   = label_width.saturating_sub(totals_label.chars().count());
        let f_label      = format!("{}{}", bold(totals_label), " ".repeat(totals_pad));

        let mut trow = format!("  {}  {}", " ".repeat(type_w), f_label);
        trow.push_str(&format!("  {:>aw$}", "", aw=addr_w));
        if show_port { trow.push_str(&format!("  {:>pw$}", "", pw=port_w)); }
        if let Some(group) = groups.first() {
            trow.push_str("  ");
            trow.push_str(&render_group(&items, group, &|gi, si| {
                items[gi].totals[si].clone().unwrap_or_else(|| format!("{:>w$}", "", w=items[gi].widths[si]))
            }));
        }
        println!("{}", trow);
        for group in groups.iter().skip(1) {
            let line = render_group(&items, group, &|gi, si| {
                items[gi].totals[si].clone().unwrap_or_else(|| format!("{:>w$}", "", w=items[gi].widths[si]))
            });
            println!("{}{}", indent, line);
        }
    }

    // ── Total time running ───────────────────────────────────────────────
    println!();
    println!("  {}", dim(&format!("total time running: {}", fmt_duration(elapsed_secs))));
}
