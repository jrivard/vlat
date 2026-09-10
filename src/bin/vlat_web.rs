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

// Browser demo: same rendering code as the native TUI (vlat::ui / vlat::state),
// driven by synthetic probe data instead of real sockets - crossterm/tokio
// don't target wasm32, and raw ICMP/TCP/TLS obviously can't run in a browser
// sandbox. See the "getting vlat running in the browser" discussion this
// binary came out of: it exists to *sample the experience*, not to monitor
// anything real.

use std::{cell::RefCell, io, rc::Rc, time::Duration};

use clap::Parser;
use ratzilla::{
    event::{KeyCode, KeyEvent},
    ratatui::{layout::Rect, Terminal},
    DomBackend, WebRenderer,
};

use vlat::{
    cli::{Args, BaseStat, SortMode, ThemeName, ViewMode, EXTRA_STAT_ALL},
    constants::{FREEZE_NOTICE_SECS, HELP_DISMISS_SECS, WARNING_DISMISS_SECS},
    demo::{self, Rng},
    state::TargetState,
    time::Instant,
    ui::{
        compute_col_widths, compute_scale, draw_bars, draw_bubble, draw_cards, draw_ekg,
        draw_fullscreen_multi_ui, draw_list_ui, draw_pong, draw_radar, draw_scatter, draw_worm,
        dialogs::{item_section, nav_skip, STAT_TOGGLE_COUNT},
        explain_max_scroll, help_menu_items, AxisField, AxisMetric, BarsState, BubbleState,
        CardsState, ColWidthsStabilizer, DialogMode, EkgState, HelpItem, LogoAnim, PongState,
        RadarState, ScatterState, ViewCtx, WormState, HELP_SORTS, HELP_THEMES, VIEW_DISPLAY_ORDER,
        VIEW_PICKER_ORDER,
    },
};

/// How often a live sort mode re-sorts the target list.
const SORT_INTERVAL: Duration = Duration::from_millis(1500);

/// `ViewMode` <-> `HELP_VIEWS`/`VIEW_PICKER_ORDER` index, mirroring app.rs's
/// (private, native-only) `FullscreenView::view_idx`/`enter_view_id!` so the
/// same picker tables can drive view switching here.
///
/// Pong (id 9) stays mapped in both directions even though it's hidden from
/// VIEW_DISPLAY_ORDER/VIEW_PICKER_ORDER (see ui/dialogs.rs) - it's WIP, not
/// deleted, and this keeps `--view pong` and any direct id-9 dispatch valid.
fn view_help_id(view: &ViewMode) -> usize {
    match view {
        ViewMode::List    => 0,
        ViewMode::Graph   => 1,
        ViewMode::Worm    => 2,
        ViewMode::Radar   => 3,
        ViewMode::Ekg     => 4,
        ViewMode::Bars    => 5,
        ViewMode::Cards   => 6,
        ViewMode::Bubble  => 7,
        ViewMode::Scatter => 8,
        ViewMode::Pong    => 9,
        ViewMode::Single  => 10,
    }
}

/// `None` for id 10 (single) - the demo always runs multiple targets, so single
/// view (one-target-only in the native app) has nothing to switch to, exactly
/// like `enter_view_id!`'s fallback arm.
fn view_from_help_id(id: usize) -> Option<ViewMode> {
    match id {
        0 => Some(ViewMode::List),
        1 => Some(ViewMode::Graph),
        2 => Some(ViewMode::Worm),
        3 => Some(ViewMode::Radar),
        4 => Some(ViewMode::Ekg),
        5 => Some(ViewMode::Bars),
        6 => Some(ViewMode::Cards),
        7 => Some(ViewMode::Bubble),
        8 => Some(ViewMode::Scatter),
        9 => Some(ViewMode::Pong),
        _ => None,
    }
}

fn view_picker_cursor(view: &ViewMode) -> usize {
    let id = view_help_id(view);
    VIEW_PICKER_ORDER.iter().position(|&i| i == id).unwrap_or(0)
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

fn theme_name_at_idx(idx: usize) -> ThemeName {
    match idx {
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

fn theme_idx(theme_name: &'static str) -> usize {
    HELP_THEMES.iter().position(|&n| n == theme_name).unwrap_or(0)
}

/// Sort key for a single target under `mode` - mirrors the per-mode key
/// closures in app.rs's periodic bubble-sort pass (lower sorts first in both
/// places), minus the multi-tick swap animation: the demo just fully re-sorts
/// every `SORT_INTERVAL` instead, which is simpler and just as legible at
/// browser-demo scale (5 targets).
fn sort_key(mode: &SortMode, s: &TargetState) -> f64 {
    if s.waiting || s.total_sent < 3 { return f64::MAX; }
    if !matches!(mode, SortMode::Streak) && s.win_loss_pct() >= 100.0 { return f64::MAX; }
    match mode {
        SortMode::None   => 0.0,
        SortMode::Name   => 0.0, // handled separately, by label
        SortMode::Mtr    => s.win_mtr().unwrap_or(f64::MAX),
        SortMode::Avg    => s.win_avg(),
        SortMode::Loss   => s.win_loss_pct(),
        SortMode::Std    => s.win_stddev(),
        SortMode::Jitter => s.win_jitter_avg(),
        SortMode::Streak => s.cur_drop_streak as f64,
        SortMode::P50    => s.win_median(),
        SortMode::P95    => s.win_p95(),
        SortMode::P99    => s.win_p99(),
        SortMode::P01    => s.win_p01(),
        SortMode::P10    => s.win_p10(),
        SortMode::Cv     => s.win_cv(),
        SortMode::Srtt   => if s.srtt > 0.0 { s.srtt } else { f64::MAX },
        SortMode::Last   => s.last_up.map(|t| Instant::now().saturating_duration_since(t).as_secs_f64()).unwrap_or(f64::MAX),
    }
}

/// Effective on/off state of the 5 identity columns, matching app.rs's
/// (private, native-only) `identity_column_states` - minus the `effective`
/// mode/port vector the demo doesn't track, so `auto_port` just checks the
/// mode-label strings directly instead.
fn identity_column_states(args: &Args, states: &[TargetState], mode_labels: &[String]) -> [bool; 5] {
    let vis = &args.column_vis;
    let auto_mode = !mode_labels.iter().all(|m| m == "icmp");
    let auto_name = states.iter().any(|s| s.custom_label || s.host.parse::<std::net::IpAddr>().is_err());
    let auto_port = mode_labels.iter().any(|m| m.contains(':'));
    let auto_resolve = states.iter().any(|s| s.ip_changes > 1);
    [
        vis.mode.unwrap_or(auto_mode),
        vis.name.unwrap_or(auto_name),
        vis.port.unwrap_or(auto_port),
        vis.addr.unwrap_or(true),
        vis.resolve.unwrap_or(auto_resolve),
    ]
}

// Rng/Profile/demo_targets/decide live in vlat::demo now - shared with the
// native `--demo` flag (probe::spawn_demo_task) so both present the same
// tuned behavior and any retuning only happens in one place.

const PROBE_INTERVAL:  Duration = Duration::from_millis(800);
const GRAPH_INTERVAL:  Duration = Duration::from_millis(1000);
const PLACEHOLDER_DIM: u16 = 100; // initial screensaver seed size; corrected on first real step()

struct Demo {
    args:        Args,
    states:      Vec<TargetState>,
    mode_labels: Vec<String>,
    profiles:    Vec<demo::Profile>,
    seqs:        Vec<usize>,
    down_remaining: Vec<u32>, // consecutive forced-drop probes left in the current outage, per target
    rng:         Rng,

    view: ViewMode,
    tick: u64,
    area: Rect, // last-drawn frame area; key handling runs outside draw_web, so dialogs that need a size (Explain's scroll bound) read this instead

    last_probe: Instant,
    last_graph: Instant,
    graph_max_entries: usize,

    dialog:         DialogMode,
    frozen:         bool,
    show_headers:   bool,
    show_col_keys:  bool,
    sort_mode:         SortMode,
    sort_order:        Vec<usize>,
    sort_mode_changed: Option<Instant>,
    last_sort:         Instant,

    col_widths: ColWidthsStabilizer,
    worm:    WormState,
    radar:   RadarState,
    ekg:     EkgState,
    bars:    BarsState,
    cards:   CardsState,
    bubble:  BubbleState,
    scatter: ScatterState,
    pong:    PongState,
}

impl Demo {
    fn new() -> Self {
        use vlat::cli::resolve_columns;

        let mut args = Args::parse_from(["vlat-web"]);
        if let Ok((stats, vis)) = resolve_columns(&args.extra_stats) {
            args.extra_stats = stats;
            args.column_vis  = vis;
        }
        args.theme  = args.theme_name.to_theme();
        args.window = 0; // lifetime stats, matches the CLI default

        let targets = demo::demo_targets(5);
        let n = targets.len();
        let mut states      = Vec::with_capacity(n);
        let mut mode_labels = Vec::with_capacity(n);
        let mut profiles    = Vec::with_capacity(n);
        for dt in targets {
            let mut s = TargetState::new(dt.label);
            // Real targets get `host`/`current_ip` populated once DNS resolution
            // completes (see app.rs); the demo has no resolver to run, so it
            // fakes an already-resolved RFC1918 address up front instead of
            // leaving current_ip as None, which would otherwise leave the UI
            // stuck showing the resolving spinner / "?.?.?.?" forever.
            s.current_ip = dt.addr.parse().ok();
            s.host       = dt.addr;
            states.push(s);
            mode_labels.push(dt.mode_tag.to_string());
            profiles.push(dt.profile);
        }

        let now = Instant::now();
        Demo {
            args,
            states,
            mode_labels,
            profiles,
            seqs: vec![0; n],
            down_remaining: vec![0; n],
            rng:  Rng::seeded(),

            view: ViewMode::Graph,
            tick: 0,
            area: Rect::new(0, 0, PLACEHOLDER_DIM, PLACEHOLDER_DIM),

            last_probe: now,
            last_graph: now,
            graph_max_entries: 400,

            dialog:        DialogMode::None,
            frozen:        false,
            show_headers:  true,
            show_col_keys: false,
            sort_mode:         SortMode::None,
            sort_order:        (0..n).collect(),
            sort_mode_changed: None,
            last_sort:         now,

            col_widths: ColWidthsStabilizer::new(),
            worm:    WormState::new(n, PLACEHOLDER_DIM, PLACEHOLDER_DIM),
            radar:   RadarState::new(n),
            ekg:     EkgState::new(n),
            bars:    BarsState::new(n),
            cards:   CardsState::new(n),
            bubble:  BubbleState::new(n, PLACEHOLDER_DIM, PLACEHOLDER_DIM),
            scatter: ScatterState::new(),
            pong:    PongState::new(n, PLACEHOLDER_DIM, 0),
        }
    }

    /// Simulate one probe round-trip per target - stands in for spawn_probe_task.
    /// The drop/outage/spike decision itself is shared with the native
    /// `--demo` flag - see `vlat::demo::decide`.
    fn simulate_probes(&mut self) {
        for i in 0..self.states.len() {
            self.seqs[i] += 1;
            let seq = self.seqs[i];
            self.states[i].record_sent(seq);

            let outcome = demo::decide(&self.profiles[i], &mut self.rng, &mut self.down_remaining[i]);
            self.states[i].record_result(seq, outcome, self.args.window, false);
        }
    }

    fn flush_graph(&mut self) {
        for s in self.states.iter_mut().filter(|s| !s.waiting) {
            s.flush_to_graph(self.graph_max_entries);
        }
    }

    fn global_max_jitter(&self) -> f64 {
        self.states.iter().filter(|s| !s.waiting)
            .map(|s| s.win_jitter_avg()).fold(0.0f64, f64::max)
    }

    fn shared_scale(&self) -> f64 {
        let global_p95 = self.states.iter().filter(|s| !s.waiting)
            .map(|s| s.win_p95()).fold(f64::MIN, f64::max);
        compute_scale(&self.args, global_p95)
    }

    /// Full re-sort of `sort_order` under the current `sort_mode` - simpler than
    /// app.rs's animated one-swap-per-tick bubble sort (see `sort_key`).
    fn resort(&mut self) {
        if self.sort_mode == SortMode::None {
            self.sort_order = (0..self.states.len()).collect();
            return;
        }
        let states = &self.states;
        if self.sort_mode == SortMode::Name {
            self.sort_order.sort_by(|&a, &b| states[a].label.to_lowercase().cmp(&states[b].label.to_lowercase()));
        } else {
            let mode = &self.sort_mode;
            self.sort_order.sort_by(|&a, &b| {
                sort_key(mode, &states[a]).partial_cmp(&sort_key(mode, &states[b])).unwrap()
            });
        }
        if self.args.reverse_sort { self.sort_order.reverse(); }
    }
}

fn main() -> io::Result<()> {
    console_error_panic_hook::set_once();

    let demo = Rc::new(RefCell::new(Demo::new()));

    let backend = DomBackend::new()?;
    let mut terminal = Terminal::new(backend)?;

    terminal.on_key_event({
        let demo = demo.clone();
        move |key_event: KeyEvent| {
            let code = key_event.code;
            let mut d = demo.borrow_mut();
            let d = &mut *d;

            // Toasts (freeze/sort/theme notices): any key dismisses them, and the
            // same keypress then falls through to the normal binding below - so,
            // e.g., pressing '1' while a notice is showing both clears it and
            // switches view in one press. Mirrors app.rs's pre-clear step.
            if matches!(d.dialog, DialogMode::FreezeNotice { .. } | DialogMode::SortNotice { .. } | DialogMode::ThemeNotice { .. }) {
                d.dialog = DialogMode::None;
            }

            let dialog = std::mem::replace(&mut d.dialog, DialogMode::None);
            d.dialog = match dialog {
                DialogMode::None => match code {
                    KeyCode::Char(ch @ '1'..='9') => {
                        d.view = match ch {
                            '1' => ViewMode::Graph,
                            '2' => ViewMode::Ekg,
                            '3' => ViewMode::Worm,
                            '4' => ViewMode::Radar,
                            '5' => ViewMode::Bars,
                            '6' => ViewMode::Cards,
                            '7' => ViewMode::Bubble,
                            '8' => ViewMode::Scatter,
                            // '9' (pong) intentionally unmapped - hidden while WIP, see
                            // ui/pong.rs and VIEW_DISPLAY_ORDER in ui/dialogs.rs.
                            _ => d.view,
                        };
                        DialogMode::None
                    }
                    // 'l' is repurposed as a direct List-view shortcut here - native
                    // uses 'l' to start/stop CSV logging, which has no meaning
                    // without a filesystem; list view is otherwise picker-only ('v').
                    KeyCode::Char('l') | KeyCode::Char('L') => { d.view = ViewMode::List; DialogMode::None }
                    KeyCode::Char(' ') => {
                        d.frozen = !d.frozen;
                        DialogMode::FreezeNotice {
                            dismiss_at: Instant::now() + Duration::from_secs(FREEZE_NOTICE_SECS),
                            now_frozen: d.frozen,
                        }
                    }
                    KeyCode::Char('v') => DialogMode::ViewPicker { cursor: view_picker_cursor(&d.view) },
                    KeyCode::Char('s') if d.states.len() > 1 => DialogMode::SortPicker { cursor: sort_idx(&d.sort_mode) },
                    KeyCode::Char('t') => DialogMode::ThemePicker { cursor: theme_idx(d.args.theme.name) },
                    KeyCode::Char('a') if d.view == ViewMode::Scatter => DialogMode::AxisPicker {
                        cursor: AxisMetric::ALL.iter().position(|&m| m == d.scatter.x_axis).unwrap_or(0),
                        field:  AxisField::X,
                    },
                    KeyCode::Char('i') if matches!(d.view, ViewMode::Graph | ViewMode::Worm | ViewMode::Radar | ViewMode::Ekg | ViewMode::Bars | ViewMode::Pong | ViewMode::Bubble | ViewMode::Scatter) => {
                        d.show_headers = !d.show_headers;
                        DialogMode::None
                    }
                    KeyCode::Char('k') => { d.show_col_keys = !d.show_col_keys; DialogMode::None }
                    KeyCode::Char('c') => DialogMode::StatColumnToggle {
                        cursor: 0,
                        identity: identity_column_states(&d.args, &d.states, &d.mode_labels),
                    },
                    KeyCode::Char('e') => DialogMode::Explain { scroll: 0 },
                    KeyCode::Char('w') => DialogMode::WindowInput { input: String::new() },
                    KeyCode::Char('h') | KeyCode::Char('?') | KeyCode::Enter => DialogMode::Help {
                        page: 0, scroll: 0,
                        dismiss_at: Instant::now() + Duration::from_secs(HELP_DISMISS_SECS),
                        logo: LogoAnim::new(), cursor: 0, sub_menu: None, collapsed: [false; 3],
                    },
                    _ => DialogMode::None,
                },

                // Any key dismisses a warning toast (matches app.rs: consumes the
                // keypress rather than falling through, unlike the freeze/sort/theme
                // toasts above).
                DialogMode::Warning { .. } => DialogMode::None,

                DialogMode::WindowInput { mut input } => {
                    match code {
                        KeyCode::Esc | KeyCode::Char('q') => DialogMode::None,
                        KeyCode::Enter => {
                            let raw = input.trim().to_string();
                            let new_secs = if raw.is_empty() {
                                Some(if d.args.window == 0 { 300 } else { 0 })
                            } else {
                                vlat::cli::parse_window_input_secs(&raw)
                                    .filter(|&s| s == 0 || s >= vlat::constants::WINDOW_MIN_SECS)
                            };
                            if let Some(secs) = new_secs {
                                d.args.window = secs;
                                let effective = if d.args.window == 0 { 300 } else { d.args.window };
                                let we = ((effective * 1000) / GRAPH_INTERVAL.as_millis().max(1) as u64 + 1) as usize;
                                d.graph_max_entries = we.max(d.args.graph_span_cols());
                                for s in d.states.iter_mut() { s.resize_window(secs, d.graph_max_entries); }
                            }
                            DialogMode::None
                        }
                        KeyCode::Backspace => { input.pop(); DialogMode::WindowInput { input } }
                        KeyCode::Char(ch) if !key_event.ctrl => { input.push(ch); DialogMode::WindowInput { input } }
                        _ => DialogMode::WindowInput { input },
                    }
                }

                DialogMode::Explain { mut scroll } => {
                    let max = explain_max_scroll(d.args.ascii, d.area.height);
                    match code {
                        KeyCode::Esc | KeyCode::Char('e') | KeyCode::Char('q') => DialogMode::None,
                        KeyCode::Up   | KeyCode::Char('k') => { scroll = scroll.saturating_sub(1); DialogMode::Explain { scroll } }
                        KeyCode::Down | KeyCode::Char('j') => { scroll = scroll.saturating_add(1).min(max); DialogMode::Explain { scroll } }
                        KeyCode::PageUp   => { scroll = scroll.saturating_sub(10); DialogMode::Explain { scroll } }
                        KeyCode::PageDown => { scroll = scroll.saturating_add(10).min(max); DialogMode::Explain { scroll } }
                        KeyCode::Home => DialogMode::Explain { scroll: 0 },
                        KeyCode::End  => DialogMode::Explain { scroll: max },
                        _ => DialogMode::Explain { scroll },
                    }
                }

                // Live-preview pickers: Up/Down apply the highlighted choice
                // immediately (matching app.rs's standalone SortPicker/ThemePicker/
                // ViewPicker - as opposed to the Help dialog's nested sub-menus,
                // which confirm on Enter; the demo routes Help's view/sort/theme
                // items straight to these same pickers instead of reimplementing
                // that second, confirm-on-Enter interaction pattern too).
                DialogMode::SortPicker { mut cursor } => {
                    let n = HELP_SORTS.len();
                    match code {
                        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('s') => DialogMode::None,
                        KeyCode::Char('r') => {
                            d.args.reverse_sort = !d.args.reverse_sort;
                            if d.sort_mode != SortMode::None { d.resort(); }
                            DialogMode::SortPicker { cursor }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            cursor = if cursor == 0 { n - 1 } else { cursor - 1 };
                            d.sort_mode = sort_mode_at_idx(cursor);
                            d.sort_mode_changed = Some(Instant::now());
                            d.resort();
                            DialogMode::SortPicker { cursor }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            cursor = (cursor + 1) % n;
                            d.sort_mode = sort_mode_at_idx(cursor);
                            d.sort_mode_changed = Some(Instant::now());
                            d.resort();
                            DialogMode::SortPicker { cursor }
                        }
                        _ => DialogMode::SortPicker { cursor },
                    }
                }

                DialogMode::ThemePicker { mut cursor } => {
                    let n = HELP_THEMES.len();
                    match code {
                        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('t') => DialogMode::None,
                        KeyCode::Up | KeyCode::Char('k') => {
                            cursor = if cursor == 0 { n - 1 } else { cursor - 1 };
                            d.args.theme_name = theme_name_at_idx(cursor);
                            d.args.theme = d.args.theme_name.to_theme();
                            d.args.theme_changed = Some(Instant::now());
                            DialogMode::ThemePicker { cursor }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            cursor = (cursor + 1) % n;
                            d.args.theme_name = theme_name_at_idx(cursor);
                            d.args.theme = d.args.theme_name.to_theme();
                            d.args.theme_changed = Some(Instant::now());
                            DialogMode::ThemePicker { cursor }
                        }
                        _ => DialogMode::ThemePicker { cursor },
                    }
                }

                DialogMode::ViewPicker { mut cursor } => {
                    let n = VIEW_PICKER_ORDER.len();
                    match code {
                        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('v') => DialogMode::None,
                        KeyCode::Up | KeyCode::Char('k') => {
                            cursor = if cursor == 0 { n - 1 } else { cursor - 1 };
                            if let Some(v) = view_from_help_id(VIEW_PICKER_ORDER.get(cursor).copied().unwrap_or(0)) { d.view = v; }
                            DialogMode::ViewPicker { cursor }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            cursor = (cursor + 1) % n;
                            if let Some(v) = view_from_help_id(VIEW_PICKER_ORDER.get(cursor).copied().unwrap_or(0)) { d.view = v; }
                            DialogMode::ViewPicker { cursor }
                        }
                        KeyCode::Char(ch @ '1'..='9') => {
                            let display_c = (ch as u8 - b'1') as usize;
                            let id = VIEW_DISPLAY_ORDER.get(display_c).copied().unwrap_or(0);
                            if let Some(v) = view_from_help_id(id) { d.view = v; }
                            DialogMode::None
                        }
                        _ => DialogMode::ViewPicker { cursor },
                    }
                }

                DialogMode::AxisPicker { mut cursor, field } if d.view == ViewMode::Scatter => {
                    let n = AxisMetric::ALL.len();
                    match code {
                        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('a') => DialogMode::None,
                        KeyCode::Up | KeyCode::Char('k') => {
                            cursor = if cursor == 0 { n - 1 } else { cursor - 1 };
                            let m = AxisMetric::ALL[cursor];
                            match field { AxisField::X => d.scatter.set_x_axis(m), AxisField::Y => d.scatter.set_y_axis(m) }
                            DialogMode::AxisPicker { cursor, field }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            cursor = (cursor + 1) % n;
                            let m = AxisMetric::ALL[cursor];
                            match field { AxisField::X => d.scatter.set_x_axis(m), AxisField::Y => d.scatter.set_y_axis(m) }
                            DialogMode::AxisPicker { cursor, field }
                        }
                        KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                            let new_field = match field { AxisField::X => AxisField::Y, AxisField::Y => AxisField::X };
                            let current = match new_field { AxisField::X => d.scatter.x_axis, AxisField::Y => d.scatter.y_axis };
                            let idx = AxisMetric::ALL.iter().position(|&m| m == current).unwrap_or(0);
                            DialogMode::AxisPicker { cursor: idx, field: new_field }
                        }
                        _ => DialogMode::AxisPicker { cursor, field },
                    }
                }
                // Scatter axis picker only makes sense while the scatter view is
                // active (mirrors native, where 'a' is gated the same way).
                DialogMode::AxisPicker { .. } => DialogMode::None,

                DialogMode::StatColumnToggle { mut cursor, mut identity } => {
                    const IDENTITY_COUNT: usize = 5;
                    const BASE_STATS: &[BaseStat] = &[BaseStat::Avg, BaseStat::Range, BaseStat::Jitter, BaseStat::Drops];
                    match code {
                        KeyCode::Esc | KeyCode::Char('c') | KeyCode::Char('q') => DialogMode::None,
                        KeyCode::Up | KeyCode::Char('k') => {
                            cursor = if cursor == 0 { STAT_TOGGLE_COUNT - 1 } else { cursor - 1 };
                            DialogMode::StatColumnToggle { cursor, identity }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            cursor = (cursor + 1) % STAT_TOGGLE_COUNT;
                            DialogMode::StatColumnToggle { cursor, identity }
                        }
                        KeyCode::Char(' ') | KeyCode::Enter => {
                            let idx = cursor;
                            if idx < IDENTITY_COUNT {
                                let now_on = !identity[idx];
                                identity[idx] = now_on;
                                match idx {
                                    0 => d.args.column_vis.mode    = Some(now_on),
                                    1 => d.args.column_vis.name    = Some(now_on),
                                    2 => d.args.column_vis.port    = Some(now_on),
                                    3 => d.args.column_vis.addr    = Some(now_on),
                                    _ => d.args.column_vis.resolve = Some(now_on),
                                }
                            } else if idx < IDENTITY_COUNT + BASE_STATS.len() {
                                let stat = BASE_STATS[idx - IDENTITY_COUNT].clone();
                                if d.args.hidden_base_stats.contains(&stat) {
                                    d.args.hidden_base_stats.retain(|s| s != &stat);
                                } else {
                                    d.args.hidden_base_stats.push(stat);
                                }
                            } else if let Some(stat) = EXTRA_STAT_ALL.get(idx - IDENTITY_COUNT - BASE_STATS.len()) {
                                if d.args.extra_stats.contains(stat) {
                                    d.args.extra_stats.retain(|s| s != stat);
                                } else {
                                    d.args.extra_stats.push(stat.clone());
                                    let extra_stats = &mut d.args.extra_stats;
                                    extra_stats.sort_by_key(|s| EXTRA_STAT_ALL.iter().position(|a| a == s).unwrap_or(usize::MAX));
                                }
                            }
                            DialogMode::StatColumnToggle { cursor, identity }
                        }
                        _ => DialogMode::StatColumnToggle { cursor, identity },
                    }
                }

                // draw_help_dialog (ui/dialogs.rs) always renders the full,
                // native `help_menu_items()` list regardless of what's passed
                // here for navigation - so cursor bookkeeping must walk that
                // same list, or Enter would act on a different item than the
                // one highlighted on screen. ViewMenu/SortMenu/ThemeMenu route
                // to the standalone pickers above instead of nesting Help's
                // native confirm-on-Enter sub-menu, so `sub_menu` stays None.
                DialogMode::Help { page, scroll, dismiss_at, logo, mut cursor, sub_menu, mut collapsed } => {
                    let sort_available = d.states.len() > 1;
                    let has_headers = matches!(d.view, ViewMode::Graph | ViewMode::Worm | ViewMode::Radar | ViewMode::Ekg | ViewMode::Bars | ViewMode::Pong | ViewMode::Bubble);
                    // Worm/Radar metric picking isn't wired up in this demo (no
                    // MetricPicker dialog here, unlike native) - only offer the
                    // axis menu for Scatter, which reuses AxisPicker below.
                    let show_axis_menu = d.view == ViewMode::Scatter;
                    let items = help_menu_items(sort_available, has_headers, show_axis_menu);
                    let item_count = items.len();
                    let mut next: Option<DialogMode> = None;
                    match code {
                        KeyCode::Esc | KeyCode::Char('h') | KeyCode::Char('q') => { next = Some(DialogMode::None); }
                        KeyCode::Up => {
                            let n = item_count.max(1);
                            let mut c = if cursor == 0 { n - 1 } else { cursor - 1 };
                            while nav_skip(&items, c, &collapsed) { c = if c == 0 { n - 1 } else { c - 1 }; }
                            cursor = c;
                        }
                        KeyCode::Down => {
                            let n = item_count.max(1);
                            let mut c = (cursor + 1) % n;
                            while nav_skip(&items, c, &collapsed) { c = (c + 1) % n; }
                            cursor = c;
                        }
                        KeyCode::Left | KeyCode::Right => {
                            if let Some(sec) = item_section(&items, cursor) {
                                let s = sec as usize;
                                collapsed[s] = !collapsed[s];
                                if collapsed[s] {
                                    if let Some(sep_idx) = items.iter().position(|it| matches!(it, HelpItem::Separator { sec: Some(n2), .. } if *n2 == sec)) {
                                        cursor = sep_idx;
                                    }
                                } else if let Some(new_pos) = items.iter().enumerate().skip(cursor + 1)
                                    .find(|(j, it)| !matches!(it, HelpItem::Separator { .. }) && !nav_skip(&items, *j, &collapsed))
                                    .map(|(j, _)| j)
                                {
                                    cursor = new_pos;
                                }
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(&item) = items.get(cursor) {
                                match item {
                                    HelpItem::ToggleHelp | HelpItem::CloseHelp => { next = Some(DialogMode::None); }
                                    HelpItem::Explain => { next = Some(DialogMode::Explain { scroll: 0 }); }
                                    HelpItem::ToggleColKeys => { d.show_col_keys = !d.show_col_keys; }
                                    HelpItem::ToggleExtraStats => {
                                        next = Some(DialogMode::StatColumnToggle {
                                            cursor: 0,
                                            identity: identity_column_states(&d.args, &d.states, &d.mode_labels),
                                        });
                                    }
                                    HelpItem::ToggleHeaders => { d.show_headers = !d.show_headers; }
                                    HelpItem::FreezeToggle => {
                                        d.frozen = !d.frozen;
                                        next = Some(DialogMode::FreezeNotice {
                                            dismiss_at: Instant::now() + Duration::from_secs(FREEZE_NOTICE_SECS),
                                            now_frozen: d.frozen,
                                        });
                                    }
                                    HelpItem::ViewMenu  => { next = Some(DialogMode::ViewPicker { cursor: view_picker_cursor(&d.view) }); }
                                    HelpItem::AxisMenu => {
                                        next = Some(DialogMode::AxisPicker {
                                            cursor: AxisMetric::ALL.iter().position(|&m| m == d.scatter.x_axis).unwrap_or(0),
                                            field:  AxisField::X,
                                        });
                                    }
                                    HelpItem::SortMenu if sort_available => { next = Some(DialogMode::SortPicker { cursor: sort_idx(&d.sort_mode) }); }
                                    HelpItem::SortMenu => {} // n/a: single target, no-op (matches native)
                                    HelpItem::ThemeMenu => { next = Some(DialogMode::ThemePicker { cursor: theme_idx(d.args.theme.name) }); }
                                    HelpItem::SetWindow => { next = Some(DialogMode::WindowInput { input: String::new() }); }
                                    // These depend on native-only functionality this demo
                                    // doesn't have (a writable config file, real DNS, log
                                    // files, a process to exit) - explain that instead of
                                    // silently doing nothing.
                                    HelpItem::SaveDefaults => {
                                        next = Some(DialogMode::Warning {
                                            dismiss_at: Instant::now() + Duration::from_secs(WARNING_DISMISS_SECS),
                                            message: "saving defaults isn't available in the browser demo (no config file)".to_string(),
                                        });
                                    }
                                    HelpItem::ReResolve => {
                                        next = Some(DialogMode::Warning {
                                            dismiss_at: Instant::now() + Duration::from_secs(WARNING_DISMISS_SECS),
                                            message: "nothing to re-resolve \u{2014} the demo's addresses are fixed, not looked up over DNS".to_string(),
                                        });
                                    }
                                    HelpItem::LoggingMenu => {
                                        next = Some(DialogMode::Warning {
                                            dismiss_at: Instant::now() + Duration::from_secs(WARNING_DISMISS_SECS),
                                            message: "CSV/JSON logging isn't available in the browser demo (no filesystem)".to_string(),
                                        });
                                    }
                                    HelpItem::Quit => {
                                        next = Some(DialogMode::Warning {
                                            dismiss_at: Instant::now() + Duration::from_secs(WARNING_DISMISS_SECS),
                                            message: "there's no process to quit in a browser \u{2014} just close the tab".to_string(),
                                        });
                                    }
                                    HelpItem::Separator { .. } => {}
                                }
                            }
                        }
                        _ => {}
                    }
                    next.unwrap_or(DialogMode::Help { page, scroll, dismiss_at, logo, cursor, sub_menu, collapsed })
                }

                // Not used by the demo: SaveDefaults/FilenameInput both depend on a
                // real filesystem, and Help routes SaveDefaults to a Warning instead
                // of ever constructing this dialog (see above). Kept for exhaustiveness.
                other => other,
            };
        }
    })?;

    terminal.draw_web(move |f| {
        let mut d = demo.borrow_mut();
        let d = &mut *d;

        d.area = f.area();

        let now = Instant::now();

        // Toast dialogs (freeze/sort/theme/warning notices) and Help auto-dismiss
        // on a timer, same as the native app's fast-ticker check.
        let toast_dismiss_at = match &d.dialog {
            DialogMode::FreezeNotice { dismiss_at, .. }
            | DialogMode::SortNotice { dismiss_at, .. }
            | DialogMode::ThemeNotice { dismiss_at, .. }
            | DialogMode::Warning { dismiss_at, .. }
            | DialogMode::Help { dismiss_at, .. } => Some(*dismiss_at),
            _ => None,
        };
        if let Some(dismiss_at) = toast_dismiss_at {
            if now >= dismiss_at { d.dialog = DialogMode::None; }
        }
        // Play the help dialog's one-shot draw-in logo animation while it's open.
        if let DialogMode::Help { logo, .. } = &mut d.dialog { logo.tick(); }

        if !d.frozen {
            if now.duration_since(d.last_probe) >= PROBE_INTERVAL {
                d.last_probe = now;
                d.simulate_probes();
            }
            if now.duration_since(d.last_graph) >= GRAPH_INTERVAL {
                d.last_graph = now;
                d.flush_graph();
            }
            if d.sort_mode != SortMode::None && now.duration_since(d.last_sort) >= SORT_INTERVAL {
                d.last_sort = now;
                d.resort();
            }
        }
        d.tick += 1;

        let area = d.area;
        let n    = d.states.len();
        let sort_order  = d.sort_order.clone();
        let sort_arrows = vec![None; n];
        let col_widths  = d.col_widths.apply(compute_col_widths(
            &d.states, d.args.is_window(), &d.args.extra_stats, &d.args.hidden_base_stats,
            false, &d.args.column_vis,
        ));
        let shared_scale = d.shared_scale();

        let ctx = ViewCtx {
            args:              &d.args,
            mode_labels:       &d.mode_labels,
            col_widths:        &col_widths,
            shared_scale,
            log_fmt:           "",
            tick:              d.tick,
            dialog:            &d.dialog,
            sort_order:        &sort_order,
            sort_arrows:       &sort_arrows,
            sort_mode:         &d.sort_mode,
            sort_mode_changed: d.sort_mode_changed,
            frozen:            d.frozen,
            show_headers:      d.show_headers,
            show_col_keys:     d.show_col_keys,
        };

        match d.view {
            ViewMode::List | ViewMode::Single => draw_list_ui(f, &d.states, &ctx),
            ViewMode::Graph => draw_fullscreen_multi_ui(f, &d.states, &ctx, None),
            ViewMode::Worm => {
                let cell_w   = (area.width / 80).clamp(1, 4);
                let one_row  = area.width >= 120;
                let rpt: u16 = if one_row { 1 } else { 2 };
                let snake_h  = area.height.saturating_sub(n as u16 * rpt + 1);
                if !d.frozen {
                    let global_max_jitter = d.global_max_jitter();
                    d.worm.step(&d.states, global_max_jitter, area.width / cell_w, snake_h);
                }
                draw_worm(f, &d.states, &d.worm, &ctx);
            }
            ViewMode::Radar => {
                if !d.frozen { d.radar.step(&d.states); }
                draw_radar(f, &d.states, &d.radar, &ctx);
            }
            ViewMode::Ekg => {
                if !d.frozen { d.ekg.push(&d.states, area.width as usize * 2); }
                draw_ekg(f, &d.states, &mut d.ekg, &ctx);
            }
            ViewMode::Bars => draw_bars(f, &d.states, &mut d.bars, &ctx),
            ViewMode::Cards => draw_cards(f, &d.states, &mut d.cards, &ctx),
            ViewMode::Bubble => {
                let rows_per_target: u16 = 1;
                let legend_h: u16 = if n > 1 { 1 } else { 0 };
                let bubble_h = area.height.saturating_sub(n as u16 * rows_per_target + legend_h);
                if !d.frozen {
                    let global_max_jitter = d.global_max_jitter();
                    d.bubble.step(&d.states, global_max_jitter, area.width, bubble_h, &d.sort_mode);
                }
                draw_bubble(f, &d.states, &d.bubble, &ctx);
            }
            ViewMode::Scatter => {
                if !d.frozen { d.scatter.step(&d.states, d.args.is_window()); }
                draw_scatter(f, &d.states, &d.scatter, &ctx);
            }
            ViewMode::Pong => {
                let one_row  = area.width >= 120;
                let rpt: u16 = if one_row { 1 } else { 2 };
                let field_h  = area.height.saturating_sub(n as u16 * rpt);
                let left_margin = vlat::ui::pong_left_margin(&d.states);
                if !d.frozen {
                    let shared_scale = d.shared_scale();
                    let global_max_jitter = d.global_max_jitter();
                    d.pong.step(&d.states, shared_scale, global_max_jitter, area.width, field_h, left_margin);
                }
                draw_pong(f, &d.states, &mut d.pong, &ctx);
            }
        }
    });

    Ok(())
}
