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

use clap::{Parser, ValueEnum};
#[cfg(feature = "native")]
use clap_complete::Shell;
use crate::ui::Theme;

pub const AFTER_HELP: &str = "\
EXAMPLES:
  Standard form:
    vlat example.net                       # single target, ICMP (or UDP fallback)
    vlat 192.168.1.1 10.0.0.1              # two targets side-by-side
    vlat example.net:tcp:443               # per-target mode and port
    vlat example.net:tcp:443 192.168.1.1 example.net:udp  # mixed modes per target
    vlat 192.168.1.1-20                    # octet range - expands to .1 through .20
    vlat 192.168.1.0/24:tcp:443            # CIDR block - expands to every host in the /24
    vlat -4 example.net                    # force IPv4 resolution
    vlat -6 example.net                    # force IPv6 resolution
    vlat example.net 2001:db8::1           # IPv4 and IPv6 target together
    vlat example.net -m tcp --tcp-port 443 # global TCP mode
    vlat example.net -c 10                 # stop after 10 probes
    vlat example.net --warn-rtt 100        # bell + flash when RTT exceeds 100ms
    vlat --bind 192.168.1.5 example.net    # bind probes to a specific local address
    vlat example.net --max-range 200       # fix chart scale to 200ms
    vlat example.net:tcp:443,interval=500ms 10.0.0.1,interval=200ms  # per-target intervals
    vlat api.example.org:http --http-path /health                   # global HTTP path
    vlat a.example.org:http,hpath=/a b.example.org:http,hpath=/b   # per-target HTTP paths
    vlat mail.example.org:smtp             # SMTP banner grab (port 25)
    vlat mail.example.org:smtps           # SMTPS banner grab with TLS (port 465)
    vlat localhost:exec --exec-cmd \"curl -sf https://api.example.org/health\"  # exec health check
    vlat host1:exec,exec=./check-a.sh host2:exec,exec=./check-b.sh           # per-target scripts

  URI form:
    vlat https://example.org                                                # URI form, mode+port from scheme
    vlat https://example.org:8443/health                                    # URI path becomes the hpath override
    vlat tcp://10.0.0.1:2222 ssh://build.example.org                        # URI form for other modes

TARGET SPEC FORMAT:
  host[:mode[:port]][,key=val...]
  mode = icmp | udp | tcp | http | https | dns | tls | ntp | ssh | smtp | smtps | exec | quic

  URI FORM (alternative to host:mode:port):
    mode://host[:port][/path][,key=val...]
    Any mode with a natural URI scheme works: http, https, tcp, udp, dns, tls,
    ntp, ssh, smtp, smtps, quic. (icmp and exec have no URI form - icmp has no
    port/connection concept, and exec runs a local command, not a network target.)
    A path is only accepted for http/https and becomes the hpath override
    (an explicit ,hpath= after the URI still wins). IPv6 hosts must be
    bracketed, as in any URI: https://[::1]:8443/health
    URI form examples: https://example.org  tcp://10.0.0.1:2222  ssh://build.example.org

  IP RANGES (expanded into individual targets before parsing):
    A.B.C.D-E              octet range, e.g. 192.168.1.1-20  ->  .1 .2 ... .20
    A.B.C.D/N              CIDR block, e.g. 192.168.1.0/24   ->  every host in the block
                            (network and broadcast addresses excluded for /30 and shorter)
    Any :mode:port suffix and ,key=val overrides apply to every host the range expands to.
    Not available in URI form (mode://host/...) since / already starts the URI path.
    vlat allows at most 256 total targets after expansion, from any combination of inputs.

  Per-target overrides (comma-separated key=value after the host spec):
    interval=<duration>   probe interval (e.g. 500ms, 2s) - overrides --interval
    timeout=<secs>        probe timeout in seconds (max 60s) - overrides --timeout
    resolve=<secs>        DNS re-resolve interval         - overrides --resolve-interval
    label=<text>          display label - replaces hostname/IP in the UI
    hpath=<path>          HTTP path for http/https probes - overrides --http-path
                          (e.g. /health, /status?check=1)
    exec=<cmd>            shell command for exec probes - overrides --exec-cmd
                          (command must not contain commas; use --exec-cmd for complex commands)

  Standard form examples:
    example.net:tcp:443,interval=500ms,timeout=2s
    192.168.1.1,interval=200ms
    host.internal,resolve=30
    10.0.0.1,label=gateway
    prod.example.org:tcp:443,label=prod-lb
    api.example.org:http:8080,hpath=/health,label=api-health
    api.example.org:https,hpath=/status?check=1
    mail.example.org:smtp,label=mail
    svc.internal:exec,exec=./check.sh,label=svc-health

  Global flags are the default for any target that does not specify inline.

SESSIONS:
  vlat saves its session (settings + summary stats) to
  $XDG_STATE_HOME/vlat/sessions (usually ~/.local/state/vlat/sessions)
  every 60 seconds and at exit. The 10 most recent unnamed sessions are
  kept; named sessions are kept until deleted.

  Run vlat with no targets to pick a saved session from a menu
  (Enter restart, s summary & exit, r rename, c copy command, d delete,
  q quit). The menu shows the selected session's full CLI command;
  c copies it to the clipboard (OSC 52). Or restart by name:

    vlat --session-name home 192.168.1.1 example.net   # run + name the session
    vlat --restart home                                # restart it later
    vlat --restart home --theme nord                   # restart, override a setting
    vlat --restart home 10.0.0.1                       # restart + add a target
    vlat --no-session example.net                      # don't save this run

  Restarting re-runs the session's saved settings and targets; probe
  stats start fresh (history is not carried over). CLI flags override
  the session's saved settings; the defaults file is skipped when
  restarting.

DEFAULTS FILE:
  vlat automatically loads default flags from a config file at startup.
  CLI flags always override file settings.

  Search order:
    $VLAT_CONFIG              if set, use this path
    $XDG_CONFIG_HOME/vlat/defaults  (usually ~/.config/vlat/defaults)

  Create the file:
    mkdir -p ~/.config/vlat
    echo '--theme nord' >> ~/.config/vlat/defaults
    echo '--interval 500ms' >> ~/.config/vlat/defaults

  The file uses the same format as the argument files below.

ARGUMENT FILES:
  Any argument may be replaced with @filename to read arguments from a file.
  Each non-blank line in the file is treated as one argument.
  Lines starting with # and text after \" #\" are ignored as comments.

  Example file (targets.txt):
    # production hosts
    web.example.org:tcp:443,label=web
    db.internal,interval=500ms,label=db
    10.0.0.1,label=gateway

  Usage: vlat @targets.txt -w 60s

PROBE TYPES:
  icmp    Raw ICMP echo - requires root or CAP_NET_RAW; falls back to udp if unavailable
  udp     UDP probe - success on ICMP Port Unreachable reply; no root needed
  tcp     TCP handshake - measures connect RTT; connection refused counts as a drop
  http    HTTP GET / over plain TCP - any valid HTTP response counts as success (path: --http-path / hpath=)
  https   HTTPS GET / with TLS - certificate validated against system trust store by default (skip with --tls-no-verify; path: --http-path / hpath=)
  dns     DNS A-record query over UDP - use --dns-query to set the name (default: example.net)
  tls     TLS handshake only - measures TCP+TLS setup time (see TLS OPTIONS)
  ntp     NTP request over UDP - measures RTT to a time server (default port: 123)
  ssh     SSH banner grab - TCP connect + read SSH-2.0 banner (default port: 22)
  smtp    SMTP banner grab - TCP connect + read 220 greeting (default port: 25)
  smtps   SMTPS banner grab - TLS connect + read 220 greeting (default port: 465)
  exec    Run a shell command and measure its duration - exit 0 = success, non-zero = drop
          Use --exec-cmd for the global command, or exec=<cmd> per target (no commas in cmd).
          Env vars exposed to the command: VLAT_HOST, VLAT_IP, VLAT_PORT.
  quic    QUIC handshake - UDP-based, measures QUIC+TLS 1.3 setup time (default port: 443)

TLS OPTIONS (apply to https, tls, smtps, and quic probe types):
  --tls-no-verify       skip certificate validation (use only for internal/trusted targets)
  --tls-cert <path>     validate against a specific PEM file instead of the system trust store
  --tls-version <ver>   negotiate a specific TLS version: any (default), 1.2, 1.3

LIVE KEYS (while running):
  h         toggle help
  e         explain output legend
  t         change color theme (picker)
  v         change view (picker): list  single  graph  ekg  radar  bars  cards  scatter  worm  bubble
  0,1-8     jump to view: 0=list/single 1=graph 2=ekg 3=worm 4=radar 5=bars 6=cards 7=bubble 8=scatter
  i         toggle target headers  (graph / worm / radar / ekg / bubble views)
  k         toggle column key header
  x         show / hide columns (dialog)
  w         set stats window (dialog; 0 = lifetime)
  s         cycle sort order  (multi-target): mtr → avg → none → name
  d         save current view / theme / sort as defaults
  r         re-resolve DNS for all hostnames now
  c / j     start/stop CSV / JSON logging
  Space     freeze display (probes continue in background)
  q / Esc   quit and show summary
  Ctrl-C    quit

TIME FORMAT:
  -w, -t, and -i all accept a plain number (seconds) or a value with a unit suffix:
    500ms   milliseconds
    30      seconds (no suffix = seconds)
    1.5s    seconds (decimal)
    5m      minutes
    1h      hours
  Examples: -i 500ms   -t 2s   -w 1h

AUTHOR:
  Jason D. Rivard
";

pub fn vlat_styles() -> clap::builder::styling::Styles {
    use clap::builder::styling::{AnsiColor, Effects, Styles};
    Styles::styled()
        .header(AnsiColor::Cyan.on_default() | Effects::BOLD)
        .usage(AnsiColor::Cyan.on_default() | Effects::BOLD)
        .literal(AnsiColor::White.on_default() | Effects::BOLD)
        .placeholder(AnsiColor::White.on_default() | Effects::BOLD)
}

/// Parse a duration string: plain number = seconds, or suffix with ms/s/m.
/// Returns milliseconds as u64.  Examples: `1`, `0.5`, `1s`, `500ms`, `2m`
pub fn parse_duration_ms(s: &str) -> Result<u64, String> {
    let err = || format!("invalid duration '{}' - examples: 500ms, 1s, 5m, 1h", s);
    if let Some(ms) = s.strip_suffix("ms") {
        ms.parse::<f64>().map(|v| v.round() as u64).map_err(|_| err())
    } else if let Some(h) = s.strip_suffix('h') {
        h.parse::<f64>().map(|v| (v * 3_600_000.0).round() as u64).map_err(|_| err())
    } else if let Some(m) = s.strip_suffix('m') {
        m.parse::<f64>().map(|v| (v * 60_000.0).round() as u64).map_err(|_| err())
    } else {
        let secs = s.strip_suffix('s').unwrap_or(s);
        secs.parse::<f64>().map(|v| (v * 1000.0).round() as u64).map_err(|_| err())
    }
}

/// Returns whole seconds (u64) - used for --window.
pub fn parse_interval(s: &str) -> Result<u64, String> { parse_duration_ms(s) }

/// Returns whole seconds (u64); 0 is allowed (means lifetime mode) - used for --window.
fn parse_window_secs(s: &str) -> Result<u64, String> {
    let ms = parse_duration_ms(s)?;
    if ms == 0 { return Ok(0); }
    let secs = (ms / 1000).max(1);
    if secs < crate::constants::WINDOW_MIN_SECS {
        return Err(format!("must be at least {}s, or 0 for lifetime mode", crate::constants::WINDOW_MIN_SECS));
    }
    if secs > crate::constants::WINDOW_MAX_SECS {
        return Err(format!("must be at most {}s (24h)", crate::constants::WINDOW_MAX_SECS));
    }
    Ok(secs)
}

/// Returns whole seconds (u64), minimum 10s - used for --span and --resolve-interval.
fn parse_nonzero_window_secs(s: &str) -> Result<u64, String> {
    let secs = parse_duration_ms(s).map(|ms| (ms / 1000).max(1))?;
    if secs < crate::constants::WINDOW_MIN_SECS {
        return Err(format!("must be at least {}s", crate::constants::WINDOW_MIN_SECS));
    }
    if secs > crate::constants::WINDOW_MAX_SECS {
        return Err(format!("must be at most {}s (24h)", crate::constants::WINDOW_MAX_SECS));
    }
    Ok(secs)
}

/// Format seconds as a compact human-readable string (e.g. "2h3m10s", "5m", "30s").
/// Returns "lifetime" for 0.
pub fn format_window_hms(secs: u64) -> String {
    if secs == 0 { return "lifetime".to_string(); }
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let mut out = String::new();
    if h > 0 { out.push_str(&format!("{}h", h)); }
    if m > 0 { out.push_str(&format!("{}m", m)); }
    if s > 0 || out.is_empty() { out.push_str(&format!("{}s", s)); }
    out
}

/// Parse a window value typed interactively: plain integer = seconds, or compound "2h3m10s".
/// Returns None for unparseable input; 0 means lifetime.
pub fn parse_window_input_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Ok(n) = s.parse::<u64>() { return Some(n); }
    let mut rem = s;
    let mut total = 0u64;
    let mut found = false;
    for (suf, mult) in &[("h", 3600u64), ("m", 60), ("s", 1)] {
        if let Some(pos) = rem.find(suf) {
            if let Ok(n) = rem[..pos].parse::<u64>() {
                total += n * mult;
                rem = &rem[pos + suf.len()..];
                found = true;
            } else {
                return None;
            }
        }
    }
    if found && rem.is_empty() { Some(total) } else { None }
}

/// Returns seconds as f64 - used for --timeout.
fn parse_timeout_secs(s: &str) -> Result<f64, String> {
    let v = parse_duration_ms(s).map(|ms| ms as f64 / 1000.0)?;
    if v > crate::constants::MAX_PROBE_TIMEOUT_SECS {
        return Err(format!("timeout cannot exceed {}s", crate::constants::MAX_PROBE_TIMEOUT_SECS));
    }
    Ok(v)
}

#[derive(Clone, ValueEnum, Debug, PartialEq)]
pub enum ThemeName {
    /// Full color (default)
    Default,
    /// Arctic-inspired blue-gray palette (Nord)
    Nord,
    /// Warm earth tones (Gruvbox)
    Gruvbox,
    /// Dark purple/pink/cyan (Dracula)
    Dracula,
    /// Ethan Schoonover's Solarized dark
    Solarized,
    /// Okabe-Ito palette - safe for red/green color blindness
    Okabe,
    /// Maximum saturation for high-contrast environments
    Highcontrast,
    /// Amber phosphor CRT monitor - warm monochrome
    Phosphor,
    /// Retro green phosphor + amber CRT - classic 80s terminal
    Retro,
    /// Monochrome - no hue, brightness only
    Nocolor,
    /// Classic NetWare CGA palette (used automatically with --worm)
    #[value(hide = true)]
    Worm,
    /// Pick a random theme at startup
    #[value(hide = true)]
    Random,
}

impl ThemeName {
    /// Build the corresponding `Theme` value.
    pub fn to_theme(&self) -> Theme {
        match self {
            ThemeName::Default      => Theme::colorful(),
            ThemeName::Nord         => Theme::nord(),
            ThemeName::Gruvbox      => Theme::gruvbox(),
            ThemeName::Dracula      => Theme::dracula(),
            ThemeName::Solarized    => Theme::solarized(),
            ThemeName::Okabe        => Theme::okabe(),
            ThemeName::Highcontrast => Theme::highcontrast(),
            ThemeName::Phosphor     => Theme::phosphor(),
            ThemeName::Retro        => Theme::retro(),
            ThemeName::Nocolor      => Theme::nocolor(),
            ThemeName::Worm         => Theme::worm(),
            ThemeName::Random       => Theme::colorful(), // resolved before this is called
        }
    }

    /// The manually-cyclable theme (`Shift+T` / `t`) at `idx`, wrapping via
    /// modulo. Order matches `crate::ui::HELP_THEMES`. `Worm` and `Random`
    /// are deliberately excluded - both are auto-selected, never cycled to.
    pub fn at_idx(idx: usize) -> ThemeName {
        match idx % 10 {
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
        }
    }

    /// The next theme after whichever one is named `current_name` (a live
    /// `Theme::name`, e.g. `args.theme.name`) in `Shift+T` cycle order.
    pub fn cycle_next(current_name: &str) -> ThemeName {
        let n = crate::ui::HELP_THEMES.len();
        let idx = crate::ui::HELP_THEMES.iter().position(|&t| t == current_name).unwrap_or(0);
        Self::at_idx((idx + 1) % n)
    }
}

#[derive(Clone, Copy, ValueEnum, Debug, PartialEq)]
pub enum ViewMode {
    /// List - one line per target, no graph
    List,
    /// Single - detailed view for one target (only valid with a single target)
    Single,
    /// Fullscreen graph
    Graph,
    /// Worm screensaver
    Worm,
    /// Radar sweep
    Radar,
    /// EKG monitor
    Ekg,
    /// Vertical bar chart
    Bars,
    /// Grid of per-target panels
    Cards,
    /// Floating latency bubbles
    Bubble,
    /// RTT vs loss scatter plot
    Scatter,
    /// Pong screensaver
    ///
    /// HIDDEN (WIP): kept out of `--help` and the in-app view picker/hotkeys
    /// while it's rough around the edges. Still fully wired up and reachable
    /// via `--view pong` directly - do not delete, just re-enable in
    /// `ui/dialogs.rs` (VIEW_DISPLAY_ORDER / VIEW_PICKER_ORDER) once refined.
    #[value(hide = true)]
    Pong,
}

/// Metric usable on either scatter-view axis (see `--scatter-x` / `--scatter-y`,
/// or the in-app axis picker: 'a' while the scatter view is active). Also
/// reused by the worm and radar views' single-metric pickers (`--worm-metric`,
/// `--radar-metric`), which drive worm speed/length or radar blip distance
/// instead of screen position.
#[derive(Clone, Copy, ValueEnum, Debug, PartialEq)]
pub enum ScatterAxis {
    /// Average RTT
    Avg,
    /// Average jitter
    Jitter,
    /// RTT standard deviation
    Stddev,
    /// Median (p50) RTT
    Median,
    /// 95th-percentile RTT
    P95,
    /// 99th-percentile RTT
    P99,
    /// Packet loss percentage
    Loss,
    /// Coefficient of variation (stddev / avg)
    Cv,
    /// Combined latency + loss score (MTR-style)
    Mtr,
}

impl ScatterAxis {
    pub fn to_axis_metric(self) -> crate::ui::AxisMetric {
        use crate::ui::AxisMetric;
        match self {
            ScatterAxis::Avg    => AxisMetric::Avg,
            ScatterAxis::Jitter => AxisMetric::Jitter,
            ScatterAxis::Stddev => AxisMetric::StdDev,
            ScatterAxis::Median => AxisMetric::Median,
            ScatterAxis::P95    => AxisMetric::P95,
            ScatterAxis::P99    => AxisMetric::P99,
            ScatterAxis::Loss   => AxisMetric::Loss,
            ScatterAxis::Cv     => AxisMetric::Cv,
            ScatterAxis::Mtr    => AxisMetric::Mtr,
        }
    }
}

#[derive(Clone, ValueEnum, Debug, PartialEq)]
pub enum OutputFormat {
    /// Comma-separated values (one row per probe)
    Csv,
    /// Newline-delimited JSON (one object per probe)
    Json,
}


/// Multi-target fullscreen sort order.
#[derive(Clone, ValueEnum, Debug, PartialEq)]
pub enum SortMode {
    /// No automatic sorting
    None,
    /// Sort by type, then label, hostname, or IP address
    Name,
    /// Best combined latency and loss to top; worst to bottom
    Mtr,
    /// Fastest average RTT to top; slowest to bottom
    Avg,
    /// Fewest packet drops to top
    Loss,
    /// Most consistent RTT (lowest stddev) to top
    Std,
    /// Smoothest jitter to top
    Jitter,
    /// No current drop streak first; longest streak to bottom
    Streak,
    /// Lowest median (p50) RTT to top
    P50,
    /// Lowest 95th-percentile RTT to top
    P95,
    /// Lowest 99th-percentile RTT to top
    P99,
    /// Lowest best-case (p01) RTT to top
    P01,
    /// Lowest 10th-percentile RTT to top
    P10,
    /// Lowest coefficient of variation (stddev/avg) to top
    Cv,
    /// Lowest smoothed RTT (RFC 6298 SRTT) to top
    Srtt,
    /// Most recently responded (host up) to top
    Last,
}

impl SortMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            SortMode::None   => "none",
            SortMode::Name   => "name",
            SortMode::Mtr    => "mtr",
            SortMode::Avg    => "avg",
            SortMode::Loss   => "loss",
            SortMode::Std    => "std",
            SortMode::Jitter => "jitter",
            SortMode::Streak => "streak",
            SortMode::P50    => "p50",
            SortMode::P95    => "p95",
            SortMode::P99    => "p99",
            SortMode::P01    => "p01",
            SortMode::P10    => "p10",
            SortMode::Cv     => "cv",
            SortMode::Srtt   => "srtt",
            SortMode::Last   => "last",
        }
    }
}

#[derive(Clone, ValueEnum, Debug, PartialEq)]
pub enum PingMode {
    /// Raw ICMP - requires root or CAP_NET_RAW
    Icmp,
    /// UDP probe - waits for ICMP Port Unreachable reply; no root needed
    Udp,
    /// TCP connect - measures handshake RTT; no root needed
    Tcp,
    /// HTTP GET / - any valid HTTP response counts as success
    Http,
    /// HTTPS GET / - TLS required; certificate validated by default (skip with --tls-no-verify)
    Https,
    /// DNS A-record query over UDP (default query: example.net; use --dns-query to override)
    Dns,
    /// TLS handshake only - measures TCP+TLS RTT; certificates validated by default (see TLS OPTIONS)
    Tls,
    /// NTP request over UDP - measures RTT to a stratum-1/2 time server (default port: 123)
    Ntp,
    /// SSH banner grab - TCP connect + read SSH-2.0 banner (default port: 22)
    Ssh,
    /// SMTP banner grab - TCP connect + read 220 greeting (default port: 25)
    Smtp,
    /// SMTPS banner grab - TLS connect + read 220 greeting (default port: 465)
    Smtps,
    /// Run a shell command; exit 0 = success/Hit, non-zero = Drop
    Exec,
    /// QUIC handshake - measures UDP+QUIC+TLS 1.3 setup time (default port: 443)
    Quic,
}

impl PingMode {
    pub fn default_port(&self, args: &Args) -> u16 {
        match self {
            PingMode::Tcp   => args.tcp_port,
            PingMode::Http  => 80,
            PingMode::Https | PingMode::Tls => crate::constants::DEFAULT_TLS_PORT,
            PingMode::Dns   => crate::constants::DEFAULT_DNS_PORT,
            PingMode::Ntp   => crate::constants::DEFAULT_NTP_PORT,
            PingMode::Ssh   => crate::constants::DEFAULT_SSH_PORT,
            PingMode::Smtp  => crate::constants::DEFAULT_SMTP_PORT,
            PingMode::Smtps => crate::constants::DEFAULT_SMTPS_PORT,
            PingMode::Quic  => crate::constants::DEFAULT_QUIC_PORT,
            PingMode::Exec  => 0,
            _               => args.udp_port,
        }
    }
}

#[derive(Parser)]
#[command(
    name = "vlat",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("GIT_HASH"), ")"),
    about = "Modern terminal latency monitor - run 'vlat --explain' for output legend",
    after_help = AFTER_HELP,
    styles = vlat_styles(),
)]
pub struct Args {
    // ── Positional ────────────────────────────────────────────────────────────
    /// One or more hostnames or IPs to monitor (omit to pick a saved session)
    pub targets: Vec<String>,

    // ── Core behavior ─────────────────────────────────────────────────────────
    /// Probe method (default: icmp with udp fallback when unavailable)
    #[arg(short, long, value_enum)]
    pub mode: Option<PingMode>,

    /// Time between probes (see TIME FORMAT)
    #[arg(short, long, default_value = "1s", value_parser = parse_interval)]
    pub interval: u64,

    /// Stop after this many probes per target (omit for continuous)
    #[arg(short, long)]
    pub count: Option<u64>,

    /// Probe timeout - max 60s; probes (including exec) are killed on expiry (see TIME FORMAT)
    #[arg(short, long, default_value = "10s", value_parser = parse_timeout_secs)]
    pub timeout: f64,

    /// Time period used to calculate stats like avg, loss, and jitter (0 = lifetime; see TIME FORMAT)
    #[arg(short, long, default_value = "0", value_parser = parse_window_secs)]
    pub window: u64,

    /// Run with synthetic simulated targets instead of real network probes -
    /// no target hosts needed; see --demo-count to change how many (default 5)
    #[arg(long)]
    pub demo: bool,

    /// Number of synthetic targets for --demo (default 5, max 256; requires --demo)
    #[arg(long, value_name = "N")]
    pub demo_count: Option<usize>,

    // ── Display / output ──────────────────────────────────────────────────────
    /// Display layout to start with (list, single, graph, worm, radar, ekg, bars, cards, bubble, scatter)
    #[arg(short = 'v', long = "view", value_enum, default_value = "single", overrides_with = "view")]
    pub view: ViewMode,

    /// Color theme
    #[arg(long = "theme", value_enum, default_value = "default", overrides_with = "theme_name")]
    pub theme_name: ThemeName,

    /// Scatter view: metric for the X axis (also changeable in-app with 'a')
    #[arg(long = "scatter-x", value_enum, default_value = "avg")]
    pub scatter_x: ScatterAxis,

    /// Scatter view: metric for the Y axis (also changeable in-app with 'a')
    #[arg(long = "scatter-y", value_enum, default_value = "loss")]
    pub scatter_y: ScatterAxis,

    /// Worm view: metric that drives worm speed and length (also changeable in-app with 'a')
    #[arg(long = "worm-metric", value_enum, default_value = "jitter")]
    pub worm_metric: ScatterAxis,

    /// Radar view: metric that drives blip distance from center (also changeable in-app with 'a')
    #[arg(long = "radar-metric", value_enum, default_value = "avg")]
    pub radar_metric: ScatterAxis,

    /// Use ASCII-only characters instead of braille/unicode (colour is preserved)
    #[arg(short, long)]
    pub ascii: bool,

    /// Show a header row labelling each stat column (toggle with 'k')
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub keys: bool,

    /// Disable colour output (equivalent to --theme nocolor)
    #[arg(long)]
    pub no_color: bool,

    /// Always show full target labels in fullscreen legend rows (graph/bubble/worm),
    /// even when they overflow. By default, overlapping labels (e.g. "home-alpha" /
    /// "home-beta") are shortened to fit - shared words are stripped first, then, if
    /// still too wide, each label is cut to the shortest prefix that stays unique.
    #[arg(long, hide = true)]
    pub no_condense_labels: bool,

    /// Columns and visual elements to display. Default set includes the recent sparkline
    /// and range bar; numeric extras are off by default. Stat values: mtr std p01 p10
    /// p50 p95 p99 cv srtt streak last recent bar status.
    /// Identity values (auto-shown unless overridden): mode name port addr resolve.
    /// Pseudo-values: all (all columns), none (reset to empty), default (default set).
    /// 'none' may be combined: --columns none,mtr shows only mtr; 'all' is exclusive.
    /// Can be repeated or comma-separated: --columns p99,mtr  or  --columns p99 --columns mtr
    #[arg(long = "columns", value_name = "NAME", value_delimiter = ',', default_value = "default")]
    pub extra_stats: Vec<ExtraStat>,

    /// Runtime-hidden default stat columns (toggled via 'x' dialog, not a CLI flag).
    #[arg(skip)]
    pub hidden_base_stats: Vec<BaseStat>,

    /// Forced show/hide state for identity columns (mode/name/port/addr/resolve).
    /// None = automatic (today's conditional display rules). Populated from --columns
    /// at startup and toggled at runtime via the 'x' dialog; not a CLI flag itself.
    #[arg(skip)]
    pub column_vis: ColumnVis,

/// Sort order in multi-target fullscreen mode (toggleable with 's')
    #[arg(long = "sort", default_value = "none", overrides_with = "sort")]
    pub sort: SortMode,

    /// Reverse the sort direction (worst to top instead of best to top; toggleable with 'r' in sort dialog)
    #[arg(long)]
    pub reverse_sort: bool,

    /// Fix the maximum range for all charts (ms). Disables auto-scaling.
    #[arg(long)]
    pub max_range: Option<f64>,

    /// Time span shown by the graph (default: --window, or 5m when window is 0). Graph always fills terminal width; columns scale to fit.
    #[arg(long, value_parser = parse_nonzero_window_secs)]
    pub span: Option<u64>,

    /// Number of per-return history rows shown above the stats line in single view (default: 0; adjust with Up/Down)
    #[arg(long, default_value_t = crate::constants::SINGLE_HISTORY_ROWS)]
    pub history_rows: u16,

    /// How often the graph timeline adds a new sample - independent of probe rate (default: 1s)
    #[arg(long, default_value = "1s", value_parser = parse_interval)]
    pub graph_interval: u64,

    // ── Networking ────────────────────────────────────────────────────────────
    /// Force IPv4 resolution
    #[arg(short = '4', group = "ip_version")]
    pub ipv4: bool,

    /// Force IPv6 resolution
    #[arg(short = '6', group = "ip_version")]
    pub ipv6: bool,

    /// Source IP address to bind probes to - use when the host has multiple network interfaces
    #[arg(long = "bind", value_name = "ADDR")]
    pub bind_addr: Option<String>,

    /// Re-check DNS for hostname changes every DURATION (default: 5 min; 0 = never; see TIME FORMAT)
    #[arg(long, default_value = "300s", value_parser = parse_nonzero_window_secs)]
    pub resolve_interval: u64,

    /// DNS server for hostname resolution - overrides the system resolver.
    /// Format: [scheme://]host[:port]
    /// Schemes: udp (default), tcp, tls (DoT, port 853), https (DoH, port 443), quic (DoQ, port 853)
    /// Examples: 192.168.1.1  10.0.0.1:5353  tcp://10.0.0.1  tls://10.0.0.53  https://10.0.0.53/dns-query
    #[arg(long, value_name = "SERVER")]
    pub dns_server: Option<String>,

    /// Disable automatic DNS re-resolution
    #[arg(long)]
    pub no_dns_refresh: bool,

    // ── Protocol-specific ─────────────────────────────────────────────────────
    /// Destination port for --mode tcp
    #[arg(long, default_value_t = crate::constants::DEFAULT_TCP_PORT)]
    pub tcp_port: u16,

    /// Destination port for --mode udp
    #[arg(long, default_value_t = crate::constants::DEFAULT_UDP_PORT)]
    pub udp_port: u16,

    /// HTTP path for http/https probes (default: /); per-target hpath= overrides this
    #[arg(long, default_value = "/")]
    pub http_path: String,

    /// Hostname to query when using --mode dns (UDP A-record lookup)
    #[arg(long, default_value = crate::constants::DEFAULT_DNS_QUERY)]
    pub dns_query: String,

    /// Skip TLS certificate validation (https, tls, smtps, quic probes) - use only for internal/trusted targets
    #[arg(long)]
    pub tls_no_verify: bool,

    /// Validate TLS certificate against a specific PEM file - implies verification (https and tls probes)
    #[arg(long)]
    pub tls_cert: Option<String>,

    /// TLS version to negotiate: any, 1.2, or 1.3 (https and tls probes; default: any)
    #[arg(long, default_value = "any", value_parser = parse_tls_version)]
    pub tls_version: TlsVersionArg,

    /// Shell command to run for --mode exec probes (exit 0 = success; per-target: exec=<cmd>)
    #[arg(long)]
    pub exec_cmd: Option<String>,

    // ── Output / logging ──────────────────────────────────────────────────────
    /// Write probe results to a file (CSV or JSON; format inferred from extension)
    #[arg(long, short = 'o')]
    pub output: Option<String>,

    /// Output format when using --output or the 'c'/'j' key
    #[arg(long, value_enum)]
    pub output_format: Option<OutputFormat>,

    /// Append timestamped probe errors and DNS events to a file for troubleshooting
    #[arg(long, value_name = "PATH")]
    pub debug_log: Option<String>,

    /// Write a live summary-stats snapshot (one target per entry) to this JSON file,
    /// overwritten regularly for the whole run - handy for dashboards/monitoring
    /// that just want "current state" rather than a per-probe event log
    #[arg(long, value_name = "PATH")]
    pub summary_json: Option<String>,

    /// How often to rewrite --summary-json, in seconds (default 5; requires --summary-json)
    #[arg(long, value_name = "SECS")]
    pub summary_json_interval: Option<u64>,

    // ── Alerts ────────────────────────────────────────────────────────────────
    /// Ring the terminal bell on each packet drop
    #[arg(long)]
    pub alert: bool,

    /// Alert (bell + label flash) when round-trip time exceeds N milliseconds
    #[arg(long)]
    pub warn_rtt: Option<f64>,

    // ── Rarely touched ────────────────────────────────────────────────────────
    /// Suppress the ICMP permission warning dialog at startup
    #[arg(long)]
    pub no_icmp_warn: bool,

    /// Allow exec probes to run with elevated privileges (root or CAP_NET_RAW); shows a startup warning
    #[arg(long)]
    pub allow_elevated_exec: bool,

    // ── Internal (not CLI args) ───────────────────────────────────────────────
    /// Resolved theme - populated after arg parsing, not a CLI arg.
    #[arg(skip)]
    pub theme: Theme,

    /// Timestamp of the last runtime theme change - populated at runtime, not a CLI arg.
    #[arg(skip)]
    pub theme_changed: Option<crate::time::Instant>,

    /// Internal: fullscreen graph active (derived from --view or set at runtime)
    #[arg(skip)]
    pub fullscreen: bool,

    /// Internal: worm screensaver active (derived from --view or set at runtime)
    #[arg(skip)]
    pub worm: bool,

    /// Internal: radar sweep active (derived from --view or set at runtime)
    #[arg(skip)]
    pub radar: bool,

    /// Internal: ekg monitor active (derived from --view or set at runtime)
    #[arg(skip)]
    pub ekg: bool,

    /// Internal: bars chart active (derived from --view or set at runtime)
    #[arg(skip)]
    pub bars: bool,

    /// Internal: cards grid active (derived from --view or set at runtime)
    #[arg(skip)]
    pub cards: bool,

    /// Internal: list one-line-per-target view active (derived from --view or set at runtime)
    #[arg(skip)]
    pub list: bool,

    /// Internal: single-target detailed view active (derived from --view or set at runtime;
    /// only valid with exactly one target)
    #[arg(skip)]
    pub single: bool,

    #[arg(skip)]
    pub pong: bool,

    #[arg(skip)]
    pub bubble: bool,

    #[arg(skip)]
    pub scatter: bool,

    // ── Sessions ──────────────────────────────────────────────────────────────
    /// Do not save session state for this run
    #[arg(long = "no-session")]
    pub no_session: bool,

    /// Name this session - named sessions are kept until deleted and restartable by name
    #[arg(long = "session-name", value_name = "NAME")]
    pub session_name: Option<String>,

    /// Restart a saved session by name (see SESSIONS; run vlat with no targets for a picker)
    #[arg(long = "restart", value_name = "NAME")]
    pub restart: Option<String>,

    // ── Meta ──────────────────────────────────────────────────────────────────
    /// Print a shell completion script to stdout and exit (bash, fish, zsh, elvish, powershell)
    #[cfg(feature = "native")]
    #[arg(long = "completions", value_name = "SHELL")]
    pub completions: Option<Shell>,
}

impl Args {
    pub fn is_window(&self) -> bool { self.window > 0 }

    pub fn config_log_lines(&self) -> Vec<String> {
        let mode = self.mode.as_ref()
            .map(|m| format!("{:?}", m).to_lowercase())
            .unwrap_or_else(|| "auto".to_string());
        let count = self.count.map(|c| c.to_string()).unwrap_or_else(|| "unlimited".to_string());
        let mut stats: Vec<String> = self.extra_stats.iter()
            .map(|s| format!("{:?}", s).to_lowercase()).collect();
        let vis_str = |v: Option<bool>| match v { Some(true) => "on", Some(false) => "off", None => "auto" };
        for (name, v) in [("mode", self.column_vis.mode), ("name", self.column_vis.name),
                          ("port", self.column_vis.port), ("addr", self.column_vis.addr),
                          ("resolve", self.column_vis.resolve)] {
            if v.is_some() { stats.push(format!("{}={}", name, vis_str(v))); }
        }
        let tls_ver = match self.tls_version {
            TlsVersionArg::Any => "any",
            TlsVersionArg::V12 => "1.2",
            TlsVersionArg::V13 => "1.3",
        };
        vec![
            format!("config: targets=[{}] mode={} interval={}ms timeout={:.2}s count={} window={}",
                self.targets.join(", "), mode, self.interval, self.timeout, count,
                format_window_hms(self.window)),
            format!("config: view={} theme={} ascii={} keys={} no-color={} sort={} reverse-sort={} max-range={} span={} graph-interval={}ms columns=[{}]",
                format!("{:?}", self.view).to_lowercase(),
                format!("{:?}", self.theme_name).to_lowercase(),
                self.ascii, self.keys, self.no_color,
                format!("{:?}", self.sort).to_lowercase(),
                self.reverse_sort,
                self.max_range.map(|r| format!("{:.0}ms", r)).unwrap_or_else(|| "auto".to_string()),
                self.span.map(format_window_hms).unwrap_or_else(|| "auto".to_string()),
                self.graph_interval,
                stats.join(",")),
            format!("config: ipv4={} ipv6={} bind={} resolve-interval={}s dns-server={} no-dns-refresh={}",
                self.ipv4, self.ipv6,
                self.bind_addr.as_deref().unwrap_or("-"),
                self.resolve_interval,
                self.dns_server.as_deref().unwrap_or("-"),
                self.no_dns_refresh),
            format!("config: tcp-port={} udp-port={} http-path={} dns-query={} tls-no-verify={} tls-cert={} tls-version={} exec-cmd={}",
                self.tcp_port, self.udp_port, self.http_path, self.dns_query,
                self.tls_no_verify,
                self.tls_cert.as_deref().unwrap_or("-"),
                tls_ver,
                self.exec_cmd.as_deref().unwrap_or("-")),
            format!("config: output={} output-format={} summary-json={} summary-json-interval={}s alert={} warn-rtt={} no-icmp-warn={} allow-elevated-exec={}",
                self.output.as_deref().unwrap_or("-"),
                self.output_format.as_ref().map(|f| format!("{:?}", f).to_lowercase()).unwrap_or_else(|| "auto".to_string()),
                self.summary_json.as_deref().unwrap_or("-"),
                self.summary_json_interval.unwrap_or(crate::constants::SUMMARY_JSON_DEFAULT_SECS),
                self.alert,
                self.warn_rtt.map(|r| format!("{:.0}ms", r)).unwrap_or_else(|| "-".to_string()),
                self.no_icmp_warn, self.allow_elevated_exec),
        ]
    }

    fn effective_span_secs(&self) -> u64 {
        if let Some(s) = self.span { return s; }
        if self.window > 0 && self.window <= 300 { return self.window; }
        300
    }

    /// Number of graph samples that represent the full --span duration.
    /// The graph always fills terminal width; this controls the time range shown.
    pub fn graph_span_cols(&self) -> usize {
        ((self.effective_span_secs() * 1000) / self.graph_interval.max(1)) as usize
    }

    /// Span duration in milliseconds.
    pub fn span_ms(&self) -> u64 {
        self.effective_span_secs() * 1000
    }
}

/// Base stat columns that are shown by default but can be hidden at runtime.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BaseStat { Avg, Range, Jitter, Drops }

/// Forced visibility for the identity columns on the target row.
/// `None` = automatic: the column follows its built-in conditional display rule
/// (mode badge with mixed modes, port when non-default, resolve counter after
/// 2+ IP changes, …). `Some(bool)` = forced on/off via --columns or the 'x' dialog.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColumnVis {
    pub mode:    Option<bool>,
    pub name:    Option<bool>,
    pub port:    Option<bool>,
    pub addr:    Option<bool>,
    pub resolve: Option<bool>,
}

/// Column values accepted by --columns: identity columns, stat columns, and pseudo-values.
#[derive(Clone, Debug, PartialEq, Eq, Hash, ValueEnum)]
pub enum ExtraStat {
    /// Show all columns (exclusive - cannot be combined with other values)
    All,
    /// Show the default set of columns; may be combined with individual columns
    Default,
    /// Show no columns (resets the set; later values add back on top)
    None,
    /// Probe-type badge before the name (auto: shown when targets have mixed modes)
    Mode,
    /// Target name - custom label or hostname (auto: shown when one exists)
    Name,
    /// Port suffix on the mode badge, e.g. tcp:443 (auto: shown for non-default ports)
    Port,
    /// Resolved IP address column (auto: always shown)
    Addr,
    /// DNS re-resolve counter, e.g. \u{21bb}3 (auto: shown after 2+ IP changes)
    Resolve,
    /// Mean time to reliability: avg / (1 − loss) - blends latency and loss  (Ω / w)
    Mtr,
    /// Standard deviation of RTT  (± / s)
    Std,
    /// 1st-percentile RTT  (₀ / 0)
    P01,
    /// 10th-percentile RTT  (₁ / 1)
    P10,
    /// Median RTT  (½ / p)
    P50,
    /// 95th-percentile RTT  (₅ / 5)
    P95,
    /// 99th-percentile RTT  (₉ / 9)
    P99,
    /// Coefficient of variation: stddev / avg as a percentage  (% / %)
    Cv,
    /// RFC 6298 smoothed RTT  (τ / t)
    Srtt,
    /// Current consecutive-drop streak  (# / #)
    Streak,
    /// Time since the last successful response, e.g. "5m" (↑ / u)
    Last,
    /// Show the per-probe sparkline on every target row (on by default)
    Recent,
    /// Show the inline range bar on every target row (on by default)
    Bar,
    /// Probe/uptime summary: probe count and elapsed time, plus a note once it's
    /// worth mentioning - "down" while down, or "last drop" if up but has dropped
    /// before. Same text shared with the single-target view's status line;
    /// excludes the up/down badge itself.
    Status,
}

/// Canonical display order of the stat columns used by 'all' and the 'x' dialog.
/// Identity columns (mode/name/port/addr/resolve) are handled via ColumnVis, not this list.
/// `Status` is appended last (rather than grouped with the other time-based stats
/// next to `Last`) so its bit/index position never shifts the ones already assigned
/// to `Recent`/`Bar` in the toggle dialog and the space_hidden/no_data bitmasks.
pub const EXTRA_STAT_ALL: &[ExtraStat] = &[
    ExtraStat::Mtr, ExtraStat::Std,
    ExtraStat::P01, ExtraStat::P10, ExtraStat::P50, ExtraStat::P95, ExtraStat::P99,
    ExtraStat::Cv, ExtraStat::Srtt, ExtraStat::Streak, ExtraStat::Last,
    ExtraStat::Recent, ExtraStat::Bar,
    ExtraStat::Status,
];

/// The default set: sparkline and range bar are on; numeric extras are off.
pub fn default_extra_stats() -> Vec<ExtraStat> { vec![ExtraStat::Recent, ExtraStat::Bar] }

/// Validates pseudo-values (all/default/none), expands them, deduplicates, and preserves order.
/// Returns the stat-column list (never All/Default/None or identity variants) plus the forced
/// visibility of the identity columns. Identity columns stay automatic unless explicitly named
/// (forced on) or swept away by 'none' (forced off until named again).
/// 'none' resets the accumulated set at the point it appears; 'none,mtr' = just mtr.
pub fn resolve_columns(raw: &[ExtraStat]) -> Result<(Vec<ExtraStat>, ColumnVis), String> {
    let has_all = raw.contains(&ExtraStat::All);
    if has_all && raw.iter().any(|s| *s != ExtraStat::All) {
        return Err("--columns 'all' cannot be combined with other values".to_string());
    }
    if has_all {
        let vis = ColumnVis {
            mode: Some(true), name: Some(true), port: Some(true),
            addr: Some(true), resolve: Some(true),
        };
        return Ok((EXTRA_STAT_ALL.to_vec(), vis));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out  = Vec::new();
    let mut vis  = ColumnVis::default();
    for stat in raw {
        match stat {
            ExtraStat::None => {
                seen.clear(); out.clear();
                vis = ColumnVis {
                    mode: Some(false), name: Some(false), port: Some(false),
                    addr: Some(false), resolve: Some(false),
                };
            }
            ExtraStat::Default => {
                for s in default_extra_stats() {
                    if seen.insert(s.clone()) { out.push(s); }
                }
            }
            ExtraStat::Mode    => vis.mode    = Some(true),
            ExtraStat::Name    => vis.name    = Some(true),
            ExtraStat::Port    => vis.port    = Some(true),
            ExtraStat::Addr    => vis.addr    = Some(true),
            ExtraStat::Resolve => vis.resolve = Some(true),
            ExtraStat::All => unreachable!(),
            s => if seen.insert(s.clone()) { out.push(s.clone()); }
        }
    }
    Ok((out, vis))
}

#[derive(Clone, Debug, PartialEq)]
pub enum TlsVersionArg { Any, V12, V13 }

fn parse_tls_version(s: &str) -> Result<TlsVersionArg, String> {
    match s {
        "any" => Ok(TlsVersionArg::Any),
        "1.2" => Ok(TlsVersionArg::V12),
        "1.3" => Ok(TlsVersionArg::V13),
        other => Err(format!("unknown TLS version '{}' - valid: any, 1.2, 1.3", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_interval;

    #[test]
    fn plain_number_as_seconds() {
        assert_eq!(parse_interval("1"), Ok(1000));
    }

    #[test]
    fn plain_decimal_as_seconds() {
        assert_eq!(parse_interval("0.5"), Ok(500));
    }

    #[test]
    fn s_suffix() {
        assert_eq!(parse_interval("2s"), Ok(2000));
    }

    #[test]
    fn s_suffix_decimal() {
        assert_eq!(parse_interval("1.5s"), Ok(1500));
    }

    #[test]
    fn ms_suffix() {
        assert_eq!(parse_interval("500ms"), Ok(500));
    }

    #[test]
    fn ms_suffix_rounds() {
        assert_eq!(parse_interval("1.5ms"), Ok(2));
    }

    #[test]
    fn m_suffix() {
        assert_eq!(parse_interval("2m"), Ok(120_000));
    }

    #[test]
    fn m_suffix_decimal() {
        assert_eq!(parse_interval("0.5m"), Ok(30_000));
    }

    #[test]
    fn zero_ms() {
        assert_eq!(parse_interval("0ms"), Ok(0));
    }

    #[test]
    fn large_value() {
        assert_eq!(parse_interval("60s"), Ok(60_000));
    }

    #[test]
    fn invalid_empty() {
        assert!(parse_interval("").is_err());
    }

    #[test]
    fn invalid_alpha() {
        assert!(parse_interval("abc").is_err());
    }

    #[test]
    fn invalid_unknown_suffix() {
        assert!(parse_interval("1x").is_err());
    }

    #[test]
    fn invalid_ms_prefix_is_not_a_number() {
        assert!(parse_interval("xms").is_err());
    }

    use super::{resolve_columns, default_extra_stats, ColumnVis, ExtraStat, EXTRA_STAT_ALL};

    #[test]
    fn columns_default_keeps_identity_auto() {
        let (stats, vis) = resolve_columns(&[ExtraStat::Default]).unwrap();
        assert_eq!(stats, default_extra_stats());
        assert_eq!(vis, ColumnVis::default());
    }

    #[test]
    fn columns_identity_value_forces_on() {
        let (stats, vis) = resolve_columns(&[ExtraStat::Default, ExtraStat::Resolve, ExtraStat::Port]).unwrap();
        assert_eq!(stats, default_extra_stats());
        assert_eq!(vis.resolve, Some(true));
        assert_eq!(vis.port, Some(true));
        assert_eq!(vis.mode, None);
        assert_eq!(vis.addr, None);
    }

    #[test]
    fn columns_none_forces_identity_off() {
        let (stats, vis) = resolve_columns(&[ExtraStat::None]).unwrap();
        assert!(stats.is_empty());
        assert_eq!(vis.mode, Some(false));
        assert_eq!(vis.name, Some(false));
        assert_eq!(vis.port, Some(false));
        assert_eq!(vis.addr, Some(false));
        assert_eq!(vis.resolve, Some(false));
    }

    #[test]
    fn columns_none_then_add_back() {
        let (stats, vis) = resolve_columns(&[ExtraStat::None, ExtraStat::Addr, ExtraStat::Mtr]).unwrap();
        assert_eq!(stats, vec![ExtraStat::Mtr]);
        assert_eq!(vis.addr, Some(true));
        assert_eq!(vis.name, Some(false));
    }

    #[test]
    fn columns_all_is_exclusive_and_forces_identity_on() {
        assert!(resolve_columns(&[ExtraStat::All, ExtraStat::Mtr]).is_err());
        let (stats, vis) = resolve_columns(&[ExtraStat::All]).unwrap();
        assert_eq!(stats, EXTRA_STAT_ALL.to_vec());
        assert_eq!(vis.mode, Some(true));
        assert_eq!(vis.resolve, Some(true));
    }

    #[test]
    fn columns_identity_never_lands_in_stat_list() {
        let (stats, _) = resolve_columns(&[ExtraStat::Mode, ExtraStat::Name, ExtraStat::P99]).unwrap();
        assert_eq!(stats, vec![ExtraStat::P99]);
    }
}
