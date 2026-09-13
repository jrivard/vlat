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

use vlat::{app, cli, constants, logfile, session};

use cli::Args;
use std::process;

/// Wrap section-header lines in the after_help text with bold+cyan ANSI codes.
/// A header is any non-indented line whose first character is uppercase and ends with ':'.
fn color_after_help(raw: &str) -> String {
    raw.lines().map(|line| {
        let is_header = !line.starts_with(' ')
            && line.ends_with(':')
            && line.chars().next().is_some_and(|c| c.is_uppercase());
        if is_header {
            format!("\x1b[1;36m{}\x1b[0m", line)
        } else {
            line.to_string()
        }
    })
    .collect::<Vec<_>>()
    .join("\n")
}

fn print_explain() {
    use std::io::IsTerminal;
    let color = std::io::stdout().is_terminal()
        && std::env::var("NO_COLOR").is_err();

    let hdr    = |s: &str| -> String { if color { format!("\x1b[1;36m{}\x1b[0m", s)  } else { s.to_string() } };
    let bld    = |s: &str| -> String { if color { format!("\x1b[1m{}\x1b[0m", s)     } else { s.to_string() } };
    let dim    = |s: &str| -> String { if color { format!("\x1b[2m{}\x1b[0m", s)     } else { s.to_string() } };
    let grn    = |s: &str| -> String { if color { format!("\x1b[32m{}\x1b[0m", s)    } else { s.to_string() } };
    let yel    = |s: &str| -> String { if color { format!("\x1b[33m{}\x1b[0m", s)    } else { s.to_string() } };
    let red    = |s: &str| -> String { if color { format!("\x1b[31m{}\x1b[0m", s)    } else { s.to_string() } };
    let mag    = |s: &str| -> String { if color { format!("\x1b[35m{}\x1b[0m", s)    } else { s.to_string() } };
    let red_bg = |s: &str| -> String { if color { format!("\x1b[41;1m{}\x1b[0m", s) } else { format!("[{}]", s) } };

    println!();
    println!("{}", hdr("OUTPUT LEGEND"));

    // Numerical stats (first)
    println!();
    println!("{}", hdr("Numerical stats:"));
    println!("  {}  {} 27.5ms  {} 21ms↔34ms  {} 2.1ms  {}",
        grn("23.3ms"),
        bld("≈"), bld("⇕"), bld("δ"),
        grn("✗ 0 0.0%"),
    );
    println!();
    println!("  {}  window average RTT",                                             bld("≈"));
    println!("  {}  range - best ↔ worst RTT seen in the window",                    bld("⇕"));
    println!("  {}  average jitter - mean abs. difference between consecutive RTTs",  bld("δ"));
    println!("  {}  drop count and loss %  ({} = none  {} = drops present)",         bld("✗"), grn("green"), red("red"));
    println!("  {}  duplicate count and %  (shown only when non-zero)",              bld("⊕"));
    println!();
    println!("  Optional extra stat columns - enable with {}:", bld("--columns"));
    println!("    {}  mtr     mean time to reliability: avg ÷ (1 − loss)", bld("Ω"));
    println!("    {}  std     standard deviation of RTT",                  bld("±"));
    println!("    {}  p01     1st-percentile RTT",     bld("₀"));
    println!("    {}  p10     10th-percentile RTT",    bld("₁"));
    println!("    {}  p50     median RTT",             bld("½"));
    println!("    {}  p95     95th-percentile RTT",    bld("₅"));
    println!("    {}  p99     99th-percentile RTT",    bld("₉"));
    println!("    {}  cv      coefficient of variation (stddev/avg %)",    bld("%"));
    println!("    {}  srtt    RFC 6298 smoothed RTT",                      bld("τ"));
    println!("    {}  streak  consecutive drop streak count",              bld("#"));
    println!("  Identity columns - automatic by default, force with {} / hide via {}:", bld("--columns"), bld("none"));
    println!("    {}     probe-type badge  (auto: shown when targets have mixed modes)", bld("mode"));
    println!("    {}     custom label or hostname  (auto: shown when one exists)",       bld("name"));
    println!("    {}     port suffix on the mode badge, e.g. tcp:443  (auto: non-default ports)", bld("port"));
    println!("    {}     resolved IP address  (auto: always shown)",                     bld("addr"));
    println!("    {}  DNS re-resolve counter ↻N  (auto: shown after 2+ IP changes)",     bld("resolve"));
    println!("  {}  all columns   {} no columns   {} default set (composable, e.g. {})",
        bld("--columns all"), bld("--columns none"), bld("--columns default"), bld("--columns default,mtr"));
    println!();
    println!("  Current RTT: {} stable   {} elevated   {} sustained spike (3+ in a row)",
        grn("green"), red("red text"), red_bg(" red bg "));

    // Trend indicators
    println!();
    println!("{}", hdr("Trend indicators:"));
    println!();
    println!("  MTR direction (shown left of target name):");
    println!();
    println!("    {}  improving steeply",         bld("↑"));
    println!("    {}  improving gently",          bld("↗"));
    println!("    {}  stable",                    dim("→"));
    println!("    {}  degrading gently",          bld("↘"));
    println!("    {}  degrading steeply",         bld("↓"));
    println!("    {}  all recent probes dropped", bld("✕"));
    println!();
    println!("  Per-response history (shown right of stats, newest on left):");
    println!();
    println!("    {}  fast     (RTT < 25% of recent baseline)",  bld("○"));
    println!("    {}  normal   (within normal range)",           bld("○"));
    println!("    {}  elevated (RTT > 175% of recent baseline)", bld("O"));
    println!("    {}  dropped",                                  bld("X"));
    println!("    {}  pending / in-flight",                      dim("."));
    println!();
    println!("  Baseline = recent p95 RTT.  fast and normal both use ○ - normal is dimmer.");
    println!("  All three speed levels use shades of the theme green - no red circles.");

    // Range bar
    println!();
    println!("{}", hdr("Range bar:"));
    println!("  {}{}{}{}{}",
        dim("range  0ms [────"),
        grn("╷"),
        dim("──────"),
        yel("●"),
        dim("────•··────────────────────]  100ms"),
    );
    println!();
    println!("  {}  window minimum - best RTT in the rolling window", grn("╷"));
    println!("  {}  current RTT position  ({} rising  {} falling)", yel("●"), yel("↗"), yel("↘"));
    println!("  {}   fading trail - up to 4 previous positions", dim("•··"));
    println!("  X    most recent probe was a drop");

    // Timeline
    println!();
    println!("{}", hdr("Timeline:"));
    println!("  {}{}{}{}{}{}{}  {}",
        dim("timeline  now "),
        grn("⣀⣀⣀⣤⣤"),
        yel("⣶⣶⣷⣷"),
        red("⣿⣿⣿"),
        mag("⣷"),
        yel("⣶⣤"),
        grn("⣀⣀⣀⣀"),
        dim("30s"),
    );
    println!();
    println!("  Past on the left; newest samples on the right (now).");
    println!("  Each braille cell encodes 2 samples across 4 height levels.");
    println!("  {}  {}  {}  {} = drop or no data",
        grn("green (low)"), yel("yellow (mid)"), red("red (high)"), mag("magenta"));
    println!("  Use --ascii for block characters instead of braille.");

    // View switching
    println!();
    println!("{}", hdr("View switching  (v / 0-8):"));
    println!("  v opens a picker:  list  single  graph  ekg  radar  bars  cards  scatter  worm  bubble");
    println!("  0=list/single  1=graph  2=ekg  3=worm  4=radar  5=bars  6=cards  7=bubble  8=scatter  (jump directly)");
    println!("  0 shows single with exactly one target, list otherwise");
    println!();
    println!("  {}   one line per target, no graph", bld("list   "));
    println!("  {}   detailed view for one target - only available with a single target", bld("single "));
    println!("  {}   fullscreen area chart - Y-axis, gridlines, {} / {} reference lines", bld("graph  "), dim("avg"), dim("p95"));
    println!("  {}   EKG monitor - scrolling latency trace", bld("ekg    "));
    println!("  {}   radar sweep - targets plotted by RTT", bld("radar  "));
    println!("  {}   vertical bar chart", bld("bars   "));
    println!("         bar height = current RTT  {}  avg RTT (window or lifetime)  {}  p95 RTT (wide columns)", bld("─"), bld("╌"));
    println!("         ghost trail = 1–4 previous probe RTTs, fading left by age   {}  = drop", bld("✗"));
    println!("         Y-axis: scale labels at 25 / 50 / 75 / 100 %%");
    println!("         footer per column: target label, current RTT, avg");
    println!("  {}   grid of per-target panels - RTT, recent sparkline, range bar, and stats", bld("cards  "));
    println!("         panels are capped in size; ~3 per row on a typical console, extra targets clipped");
    println!("  {}   scatter plot - average RTT vs packet loss ({} to pick axes)", bld("scatter"), bld("a"));
    println!("  {}   retro worm screensaver", bld("worm   "));
    println!("  {}   floating latency bubbles - bubble size tracks average RTT", bld("bubble "));
    // pong (--view pong) is intentionally left out of this list and the in-app
    // picker/hotkeys - it's still WIP. See ui/pong.rs for details; do not delete.

    // Rolling window
    println!();
    println!("{}", hdr("Rolling window  (-w):"));
    println!();
    println!("  Stats can be computed over a sliding time window (10s–24h) or lifetime (default).");
    println!("  Only probes sent within the window are counted; older results expire automatically.");
    println!("  Set the window with -w:  vlat -w 60s   vlat -w 10m   vlat -w 1h   vlat -w 0 (lifetime)");
    println!();
    println!("  Window-based:");
    for (name, what) in [
        ("avg      ", "mean RTT over the window"),
        ("min      ", "best RTT seen in the window"),
        ("max      ", "worst RTT seen in the window"),
        ("p95      ", "95th-percentile RTT in the window"),
        ("jtr      ", "average jitter (mean abs. diff. between consecutive RTTs)"),
        ("mtr      ", "mean time to reliability - avg ÷ (1 − loss rate)"),
        ("RTT color", "green/red trend is relative to the window average"),
    ] {
        println!("    {}  {}", bld(name), what);
    }
    println!();
    println!("  Not window-based (lifetime totals):");
    for (name, what) in [
        ("drp            ", "all drops since the session started"),
        ("sent / received", "probe counts for the full session"),
    ] {
        println!("    {}  {}", bld(name), what);
    }
    println!();
    println!("  Press {} to set the window at runtime (0 = lifetime; resets the graph).", bld("w"));
    println!("  Use {} to set the graph time-axis width, and {} to set the starting window.", bld("--span"), bld("-w"));

    // Sorting
    println!();
    println!("{}", hdr("Sorting  (multi-target):"));
    println!();
    println!("  When watching multiple targets, rows can be sorted automatically.");
    println!("  Press {} to cycle through modes, or set a default with {}.", bld("s"), bld("--sort"));
    println!();
    for (mode, desc) in [
        ("auto", "starts in mtr order - best performers rise, worst sink (default)"),
        ("mtr ", "best performers rise to the top; worst sink - re-evaluated periodically"),
        ("avg ", "lowest average RTT to top; ignores packet loss"),
        ("name", "alphabetical by label, hostname, or IP - grouped by probe type"),
        ("none", "specified order matching the command-line argument list"),
    ] {
        println!("    {}  {}", bld(mode), desc);
    }
    println!();
    println!("  In {} mode a {} or {} arrow appears next to a target's label when it moves up or down.",
        bld("mtr"), grn("▲"), red("▼"));
    println!("  mtr sort key: {} - penalises both high latency and packet loss.", bld("avg ÷ (1 − loss rate)"));
    println!("  Targets with fewer than 3 samples or still waiting for a reply are not reordered.");

    println!();
    println!("{}", hdr("Output columns - summary (shown on exit):"));
    println!("  (all values cover the full session lifetime except mtr, which uses the final window)");
    println!();
    for (name, desc) in [
        ("avg     ", "lifetime average RTT"),
        ("stddev  ", "standard deviation of RTT - spread around the average"),
        ("mtr     ", "mean time to reliability using the final window (same formula as live mtr)"),
        ("jitter  ", "lifetime average jitter"),
        ("min     ", "lifetime minimum RTT"),
        ("max     ", "lifetime maximum RTT"),
        ("sent    ", "total probes transmitted"),
        ("received", "probes that got a response"),
        ("loss    ", "drop count and loss % (column omitted when zero for all targets)"),
        ("changes ", "number of times the target's IP address changed (omitted when none)"),
    ] {
        println!("  {}  {}", bld(name), desc);
    }

    // Probe modes
    println!();
    println!("{}", hdr("Probe modes:"));
    println!("  (RTT is measured from probe start to first valid response for all types)");
    println!();
    for (name, desc) in [
        ("icmp ", "Raw ICMP echo - most accurate; requires root or CAP_NET_RAW."),
        ("udp  ", "Sends a UDP datagram; success on ICMP Port Unreachable reply."),
        ("tcp  ", "TCP connect; measures handshake RTT; refused counts as success."),
        ("http ", "HTTP GET /; any valid HTTP response counts as success."),
        ("https", "HTTPS GET / with TLS; cert validated against system trust store by default (skip with --tls-no-verify)."),
        ("dns  ", "DNS A-record query over UDP (default name: example.net)."),
        ("tls  ", "TCP connect + TLS handshake only; measures time to secure channel."),
        ("ntp  ", "NTP request over UDP; measures RTT to a time server (default port: 123)."),
        ("ssh  ", "TCP connect + read SSH-2.0 banner; measures time to first banner byte (default port: 22)."),
        ("smtp ", "TCP connect + read SMTP 220 greeting; measures time to first banner byte (default port: 25)."),
        ("smtps", "TLS connect + read SMTP 220 greeting (default port: 465)."),
        ("exec ", "Run a shell command via sh -c; exit 0 = Hit, non-zero = Drop. RTT = wall-clock duration."),
        ("quic ", "QUIC handshake - UDP-based, measures QUIC+TLS 1.3 setup time (default port: 443)."),
    ] {
        println!("  {}  {}", bld(name), desc);
    }
    println!();
    println!("  exec probes set VLAT_HOST, VLAT_IP, VLAT_PORT in the child environment.");
    println!("  All probes are killed after the timeout (max 60s); exec procs receive SIGKILL.");

    // ICMP permissions
    println!();
    println!("{}", hdr("ICMP permissions:"));
    println!("  vlat tries ICMP first and falls back to UDP automatically.");
    println!("  To enable ICMP:");
    println!("    sudo vlat ...");
    println!("    sudo setcap cap_net_raw+ep $(which vlat)");
    println!();
}

/// Heuristically detect whether the terminal supports UTF-8 / Unicode.
/// Returns true if Unicode is likely safe, false if we should fall back to ASCII.
fn detect_unicode_terminal() -> bool {
    // Windows: check if we're running in Windows Terminal or a UTF-8 capable console.
    // On Windows, GetConsoleOutputCP() == 65001 means UTF-8.
    #[cfg(target_os = "windows")]
    {
        // Windows Terminal always sets WT_SESSION; trust it for Unicode.
        if std::env::var("WT_SESSION").is_ok() {
            return true;
        }
        // Fallback: code page 65001 = UTF-8.
        // SAFETY: GetConsoleOutputCP is always safe to call.
        let cp = unsafe { windows_sys::Win32::System::Console::GetConsoleOutputCP() };
        return cp == 65001;
    }

    // Unix: check locale env vars for UTF-8 marker.
    #[cfg(not(target_os = "windows"))]
    {
        for var in &["LC_ALL", "LC_CTYPE", "LANG"] {
            if let Ok(val) = std::env::var(var) {
                let v = val.to_uppercase();
                if v.contains("UTF-8") || v.contains("UTF8") {
                    return true;
                }
            }
        }
        // Also check TERM - some minimal environments set UTF-8 via this
        if let Ok(term) = std::env::var("TERM") {
            if term.contains("utf") || term.contains("UTF") {
                return true;
            }
        }
        // If no locale at all is set, modern Linux/Mac default to UTF-8
        // so treat absence of an explicit non-UTF locale as UTF-8 capable.
        let has_locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .any(|v| std::env::var(v).is_ok());
        !has_locale
    }
}

/// Return the path to the user's defaults file, or None if none exists.
/// Checks $VLAT_CONFIG first, then $XDG_CONFIG_HOME/vlat/defaults,
/// then ~/.config/vlat/defaults.
fn default_config_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("VLAT_CONFIG") {
        return Some(std::path::PathBuf::from(p));
    }
    let p = vlat::paths::xdg_config_dir().join("vlat").join("defaults");
    if p.exists() { Some(p) } else { None }
}

/// Expand `@filename` entries in an argv iterator into the lines of that file.
/// Lines beginning with `#` and blank lines are ignored.
/// Inline comments (` #...` after a space) are stripped.
/// Returns an error string if any referenced file cannot be read.
fn expand_arg_files(raw: impl Iterator<Item = String>) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for arg in raw {
        if let Some(path) = arg.strip_prefix('@') {
            let content = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read argument file '{}': {}", path, e))?;
            for line in content.lines() {
                // Strip inline comments and trim whitespace
                let line = match line.find(" #") {
                    Some(i) => &line[..i],
                    None    => line,
                };
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') { continue; }
                out.push(line.to_string());
            }
        } else {
            out.push(arg);
        }
    }
    Ok(out)
}

#[cfg(target_os = "windows")]
fn enable_vt_processing() {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE,
    };
    unsafe {
        for handle_id in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let h = GetStdHandle(handle_id);
            let mut mode = 0u32;
            if GetConsoleMode(h, &mut mode) != 0 {
                let _ = SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
            }
        }
    }
}

#[tokio::main]
async fn main() {
    #[cfg(target_os = "windows")]
    enable_vt_processing();

    if std::env::args().any(|a| a == "-xk2") {
        println!("amb is my hero");
        process::exit(0);
    }
    if std::env::args().any(|a| a == "--explain") {
        print_explain();
        process::exit(0);
    }
    // Build effective argv: program name + defaults file args + CLI args.
    // This lets CLI flags override anything in the defaults file.
    let mut raw_args: Vec<String> = std::env::args().collect();
    if let Some(cfg) = default_config_path() {
        let cfg_arg = format!("@{}", cfg.display());
        raw_args.insert(1, cfg_arg);
    }
    let argv = match expand_arg_files(raw_args.into_iter()) {
        Ok(v)    => v,
        Err(msg) => { eprintln!("vlat: {}", msg); process::exit(1); }
    };

    use clap::{CommandFactory, FromArgMatches};
    use std::io::IsTerminal;

    // Handle --completions before full arg parsing so clap doesn't error on missing targets.
    if let Some(pos) = argv.iter().position(|a| a == "--completions") {
        if let Some(shell_str) = argv.get(pos + 1) {
            use clap_complete::generate;
            if let Ok(shell) = shell_str.parse::<clap_complete::Shell>() {
                let mut cmd = Args::command();
                generate(shell, &mut cmd, "vlat", &mut std::io::stdout());
                process::exit(0);
            }
        }
    }

    // Colorize after_help section headers when the terminal supports color.
    let color = std::io::stdout().is_terminal()
        && std::env::var("NO_COLOR").is_err()
        && std::env::var("TERM").map_or(true, |t| t != "dumb");
    let mut cmd = Args::command();
    if color {
        cmd = cmd.after_help(color_after_help(cli::AFTER_HELP));
    }
    let matches = match cmd.try_get_matches_from(&argv) {
        Ok(m) => m,
        Err(e) => e.exit(),
    };
    let mut args = Args::from_arg_matches(&matches)
        .unwrap_or_else(|e: clap::Error| e.exit());

    // ── demo mode ─────────────────────────────────────────────────────────────
    // --demo runs synthetic targets instead of real ones (see vlat::demo), so
    // it has no real target list to restart later - keep it out of the session
    // picker / restart machinery entirely rather than saving an unrestartable session.
    // Exception: if the user explicitly names the session with --session-name,
    // they've asked for it to be kept, so let it save under that name.
    if args.demo {
        if !args.targets.is_empty() {
            eprintln!("vlat: --demo cannot be combined with explicit targets");
            process::exit(1);
        }
        if args.restart.is_some() {
            eprintln!("vlat: --demo cannot be combined with --restart");
            process::exit(1);
        }
        if let Some(n) = args.demo_count {
            if n == 0 || n > constants::MAX_TARGETS {
                eprintln!("vlat: --demo-count must be between 1 and {}", constants::MAX_TARGETS);
                process::exit(1);
            }
        }
        if args.session_name.is_none() {
            args.no_session = true;
        }
    } else if args.demo_count.is_some() {
        eprintln!("vlat: --demo-count requires --demo");
        process::exit(1);
    }

    if args.summary_json_interval.is_some() && args.summary_json.is_none() {
        eprintln!("vlat: --summary-json-interval requires --summary-json");
        process::exit(1);
    }

    // ── session restart / picker ─────────────────────────────────────────────
    // --restart <name> loads a saved session by name.  No targets at all (CLI
    // or defaults file) opens the session picker; if no sessions exist yet,
    // fall back to the "no targets" help.
    let mut restart_choice: Option<(std::path::PathBuf, session::SessionFile)> = None;
    let mut summary_only = false;
    if args.demo {
        // Handled above; skip restart lookup and the "no targets" session picker.
    } else if let Some(ref name) = args.restart {
        match session::find_by_name(name) {
            Some(found) => restart_choice = Some(found),
            None => {
                eprintln!("vlat: no saved session named '{}'", name);
                let names = session::named_sessions();
                if names.is_empty() {
                    eprintln!("  (no named sessions exist - name one with --session-name)");
                } else {
                    eprintln!("  named sessions: {}", names.join(", "));
                }
                process::exit(1);
            }
        }
    } else if args.targets.is_empty() {
        if session::list_sessions().is_empty() {
            print_no_targets_help(color);
            process::exit(1);
        }
        // Best-effort initial theme for the picker itself: honors --theme and
        // --no-color the same way the real run eventually will (that full
        // resolution happens later, after restart - see below). Random isn't
        // re-rolled here since the real run rolls its own pick independently.
        let picker_theme_name = if args.no_color {
            cli::ThemeName::Nocolor
        } else if args.theme_name == cli::ThemeName::Random {
            cli::ThemeName::Default
        } else {
            args.theme_name.clone()
        };
        match session::run_picker(picker_theme_name) {
            Ok(Some((path, sess, action))) => {
                summary_only = matches!(action, session::PickerAction::Summary);
                restart_choice = Some((path, sess));
            }
            Ok(None) => process::exit(0),
            Err(e) => {
                eprintln!("vlat: session picker failed: {}", e);
                process::exit(1);
            }
        }
    }

    // Restarting: rebuild argv as [saved settings] + [user CLI args] and reparse.
    // Flags the user passed explicitly are dropped from the saved set, so the
    // CLI wins; the defaults file is skipped entirely (the session already
    // captures the full setting state).
    let mut restart_loaded: Option<(std::path::PathBuf, session::SessionFile)> = None;
    let mut user_set_columns = false;
    if let Some((path, sess)) = restart_choice {
        // Which args did the user set explicitly? Parsed WITHOUT the defaults
        // file so that saved session settings override defaults-file settings.
        let user_argv = expand_arg_files(std::env::args())
            .unwrap_or_else(|_| std::env::args().collect());
        let probe_cmd = Args::command();
        let user_set: std::collections::HashSet<String> =
            match probe_cmd.clone().try_get_matches_from(&user_argv) {
                Ok(m) => probe_cmd
                    .get_arguments()
                    .filter(|a| m.value_source(a.get_id().as_str())
                        == Some(clap::parser::ValueSource::CommandLine))
                    .map(|a| a.get_id().to_string())
                    .collect(),
                Err(_) => Default::default(),
            };
        user_set_columns = user_set.contains("extra_stats");

        let mut final_argv: Vec<String> = vec![user_argv[0].clone()];
        final_argv.extend(session::restart_argv(&sess, &probe_cmd, &user_set));
        final_argv.extend(user_argv.into_iter().skip(1));

        let m2 = Args::command()
            .try_get_matches_from(&final_argv)
            .unwrap_or_else(|e| e.exit());
        args = Args::from_arg_matches(&m2).unwrap_or_else(|e: clap::Error| e.exit());
        restart_loaded = Some((path, sess));
    }
    // Auto-detect: force ASCII mode if terminal doesn't appear to support UTF-8
    if !args.ascii && !detect_unicode_terminal() {
        args.ascii = true;
    }
    // --no-color implies nocolor theme
    use cli::{ThemeName, ViewMode};
    if args.no_color { args.theme_name = ThemeName::Nocolor; }
    // Translate --view into internal bool flags used throughout the runtime
    match args.view {
        ViewMode::Graph   => { args.fullscreen = true; }
        ViewMode::Worm    => { args.worm   = true; }
        ViewMode::Radar   => { args.radar  = true; }
        ViewMode::Ekg     => { args.ekg    = true; }
        ViewMode::Bars    => { args.bars   = true; }
        ViewMode::Cards   => { args.cards  = true; }
        ViewMode::Bubble  => { args.bubble  = true; }
        ViewMode::Scatter => { args.scatter = true; }
        ViewMode::List    => { args.list    = true; }
        ViewMode::Single  => { args.single  = true; }
        ViewMode::Pong    => { args.pong    = true; }
    }
    // worm defaults to the worm CGA palette unless the user picked something explicitly
    if args.worm && args.theme_name == ThemeName::Default {
        args.theme_name = ThemeName::Worm;
    }
    if args.theme_name == ThemeName::Random {
        let all = [
            ThemeName::Default, ThemeName::Nord, ThemeName::Gruvbox, ThemeName::Dracula,
            ThemeName::Solarized, ThemeName::Okabe, ThemeName::Highcontrast,
            ThemeName::Phosphor, ThemeName::Nocolor,
        ];
        let idx = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize)
            .unwrap_or(0)
            % all.len();
        args.theme_name = all[idx].clone();
    }
    args.theme = args.theme_name.to_theme();
    match cli::resolve_columns(&args.extra_stats) {
        Ok((stats, vis)) => { args.extra_stats = stats; args.column_vis = vis; }
        Err(e) => { eprintln!("vlat: {}", e); process::exit(1); }
    }
    // Saved runtime column state ('x' dialog) isn't CLI-expressible - apply it
    // after resolve_columns, unless the user passed --columns explicitly.
    let is_restart = restart_loaded.is_some();
    let session_ctx = match restart_loaded {
        Some((path, sess)) => {
            session::apply_saved_columns(&mut args, &sess, user_set_columns);
            if summary_only {
                app::print_saved_summary(&sess, &args);
                process::exit(0);
            }
            session::SessionCtx::restarted(!args.no_session, args.session_name.clone(), path, sess.name.clone())
        }
        None => session::SessionCtx::new(!args.no_session, args.session_name.clone()),
    };
    if let Some(ref path) = args.debug_log {
        if let Err(e) = logfile::init(path) {
            eprintln!("vlat: cannot open log file '{}': {}", path, e);
            process::exit(1);
        }
    }
    for line in args.config_log_lines() {
        logfile::write(&line);
    }
    if is_restart {
        logfile::write(&format!("session: restarting from '{}'", session_ctx.path.display()));
    }
    if let Err(e) = app::run(args, session_ctx).await {
        eprintln!("vlat: {}", e);
        process::exit(1);
    }
}

fn print_no_targets_help(color: bool) {
    let bold = |s: &str| if color { format!("\x1b[1m{}\x1b[0m", s)    } else { s.to_string() };
    let cyan = |s: &str| if color { format!("\x1b[1;36m{}\x1b[0m", s) } else { s.to_string() };
    eprintln!("{}: no targets specified\n", bold("vlat"));
    eprintln!("Specify one or more hostnames or IP addresses to monitor:\n");
    eprintln!("  {} example.net", cyan("vlat"));
    eprintln!("  {} 192.168.1.1 example.com", cyan("vlat"));
    eprintln!("  {} example.com:tcp:443", cyan("vlat"));
    eprintln!("\nOnce sessions have been saved, running '{}' with no targets shows", cyan("vlat"));
    eprintln!("a menu of recent sessions to restart.");
    eprintln!("\nRun '{}' for the full target format and options.", bold("vlat --help"));
}
