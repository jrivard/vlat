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

use ratatui::style::Color;
use super::lerp_rgb;

/// All semantic colors for the UI. Add new themes by implementing `Theme` differently.
#[derive(Clone)]
#[allow(dead_code)]
pub struct Theme {
    // ── Theme identity ────────────────────────────────────────────────────────
    pub name: &'static str,

    // ── RTT / latency coloring (stats line) ───────────────────────────────────
    pub rtt_normal:        Color,      // unremarkable RTT text
    pub rtt_warn:          Color,      // score > 1.0 text
    pub rtt_alert_bg:      Color,      // score > 2.0 background
    pub rtt_flash_bg:      Color,      // warning_streak ≥ 3 background
    pub rtt_good:          Color,      // score < -1.0 text
    pub rtt_great_bg:      Color,      // score < -2.0 background

    // ── Drop indicators ───────────────────────────────────────────────────────
    pub drop_color:        Color,      // range bar X, stat-line DROP background
    pub drop_bar_dim:      (u8,u8,u8), // dim dashes between X marks in range bar
    pub drop_marker:       (u8,u8,u8), // ✕ in fullscreen graph
    pub drop_bg_ascii:     (u8,u8,u8), // drop column shading (ASCII mode)
    pub drop_bg_unicode:   (u8,u8,u8), // drop column shading (unicode mode)
    pub drop_sparkline:    (u8,u8,u8), // drop cell in sparkline/timeline

    // ── RTT gradient (sparkline, timeline) ────────────────────────────────────
    pub grad_low:          (u8,u8,u8), // green - low latency
    pub grad_mid:          (u8,u8,u8), // yellow - mid latency
    pub grad_high:         (u8,u8,u8), // red - high latency
    pub grad_pending:      (u8,u8,u8), // dim dot before data arrives

    // ── Range bar ─────────────────────────────────────────────────────────────
    pub range_scale:       (u8,u8,u8), // scale label + travelling-label background
    pub range_trail:       [u8; 4],    // fading trail dot brightness levels

    // ── Fullscreen graph area ─────────────────────────────────────────────────
    pub graph_pending:     (u8,u8,u8), // in-flight pending dim color
    pub graph_grid:        (u8,u8,u8), // gridlines
    pub graph_avg:         (u8,u8,u8), // avg reference label
    pub graph_p95:         (u8,u8,u8), // p95 reference label
    pub anim_amber:        (u8,u8,u8), // Y-axis animation bg base (amber phase)
    pub anim_red:          (u8,u8,u8), // Y-axis animation bg base (red phase)

    // ── Multi-target palette (8 distinct colors) ──────────────────────────────
    pub targets:           [(u8,u8,u8); 8],

    // ── Protocol mode badge colors ────────────────────────────────────────────
    pub mode_icmp:         (u8,u8,u8),
    pub mode_udp:          (u8,u8,u8),
    pub mode_tcp:          (u8,u8,u8),
    pub mode_http:         (u8,u8,u8),
    pub mode_https:        (u8,u8,u8),
    pub mode_dns:          (u8,u8,u8),
    pub mode_tls:          (u8,u8,u8),
    pub mode_other:        (u8,u8,u8),

    // ── Header / labels ───────────────────────────────────────────────────────
    pub hostname:          Color,      // single-target hostname
    pub hostname_flash_fg: Color,      // threshold alert flash - fg
    pub hostname_flash_bg: Color,      // threshold alert flash - bg
    pub resolving:         Color,      // DNS spinner
    pub ip_change:         Color,      // IP change counter
    pub log_badge:         Color,      // [CSV ●] / [JSON ●] indicator

    // ── Chrome ────────────────────────────────────────────────────────────────
    pub uptime:            (u8,u8,u8), // corner uptime overlay
    pub col_key_rule:      (u8,u8,u8), // separator line under column-key names
    pub xaxis_dim:         (u8,u8,u8), // dim X-axis time labels
    pub xaxis_now:         Color,      // "now" X-axis label
    pub small_term:        (u8,u8,u8), // "terminal too small" message

    // ── Dialogs ───────────────────────────────────────────────────────────────
    pub dlg_warning:       Color,      // warning dialog border + text
    pub dlg_timer:         (u8,u8,u8), // countdown timer in warning dialog
    pub dlg_help_title:    Color,      // help dialog title background
    pub dlg_help_key:      Color,      // help dialog key labels
    pub dlg_help_label:    (u8,u8,u8), // help dialog descriptions
}

/// Extract RGB components from a ratatui Color, with fallbacks for named variants.
pub(crate) fn color_to_rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Red           => (210, 60,  60),
        Color::Yellow        => (180, 180,  0),
        Color::Green         => (60,  180, 60),
        Color::Magenta       => (180, 60,  180),
        Color::Cyan          => (60,  180, 180),
        Color::Blue          => (60,  60,  210),
        Color::White         => (220, 220, 220),
        Color::Gray          => (140, 140, 140),
        Color::DarkGray      => (80,  80,  80),
        _                    => (160, 160, 160),
    }
}

impl Theme {
    /// Convert a stored `(r,g,b)` tuple to a ratatui `Color::Rgb`.
    #[inline] pub fn c(&self, rgb: (u8,u8,u8)) -> Color { Color::Rgb(rgb.0, rgb.1, rgb.2) }

    /// `drop_color` as an RGB tuple, for fading calculations.
    #[inline] pub fn drop_color_rgb(&self) -> (u8, u8, u8) { color_to_rgb(self.drop_color) }

    /// Numerator color for the drop/sent indicator, driven by the loss rate.
    ///
    /// Gradient: dim-gray → `rtt_warn` (≈1%) → midpoint → `drop_color` (≥25%).
    /// Each segment uses smoothstep easing.  Returns a `Color::Rgb` value ready
    /// for use in a `Style::fg(…)` call.
    pub fn drop_num_color(&self, rate: f64) -> Color {
        const GRAY: (u8, u8, u8) = (90, 90, 90); // "dim zero" anchor
        let warn = color_to_rgb(self.rtt_warn);
        let crit = color_to_rgb(self.drop_color);
        let mid  = lerp_rgb(warn.0, warn.1, warn.2, crit.0, crit.1, crit.2, 0.5);
        let ease = |t: f64| t * t * (3.0 - 2.0 * t);

        let rgb = if rate <= 0.01 {
            // gray → warn over 0 %..1 %
            let t = ease((rate / 0.01).clamp(0.0, 1.0));
            lerp_rgb(GRAY.0, GRAY.1, GRAY.2, warn.0, warn.1, warn.2, t)
        } else if rate <= 0.10 {
            // warn → mid over 1 %..10 %
            let t = ease(((rate - 0.01) / 0.09).clamp(0.0, 1.0));
            lerp_rgb(warn.0, warn.1, warn.2, mid.0, mid.1, mid.2, t)
        } else {
            // mid → crit over 10 %..25 %
            let t = ease(((rate - 0.10) / 0.15).clamp(0.0, 1.0));
            lerp_rgb(mid.0, mid.1, mid.2, crit.0, crit.1, crit.2, t)
        };
        Color::Rgb(rgb.0, rgb.1, rgb.2)
    }

    /// Smooth two-stop gradient: low (0.0) → mid (0.5) → high (1.0).
    pub fn gradient_color(&self, norm: f64) -> (u8,u8,u8) {
        let (lr, lg, lb) = self.grad_low;
        let (mr, mg, mb) = self.grad_mid;
        let (hr, hg, hb) = self.grad_high;
        if norm <= 0.5 {
            lerp_rgb(lr, lg, lb, mr, mg, mb, norm * 2.0)
        } else {
            lerp_rgb(mr, mg, mb, hr, hg, hb, (norm - 0.5) * 2.0)
        }
    }

    /// Color for multi-target slot `idx` (wraps through the 8-color palette).
    pub fn target_color(&self, idx: usize) -> (u8,u8,u8) {
        self.targets[idx % self.targets.len()]
    }

    /// Color for a protocol mode label string (e.g. "icmp", "tcp:443").
    pub fn mode_color(&self, label: &str) -> Color {
        let proto = label.split(':').next().unwrap_or(label);
        let (r,g,b) = match proto {
            "icmp"  => self.mode_icmp,
            "udp"   => self.mode_udp,
            "tcp"   => self.mode_tcp,
            "http"  => self.mode_http,
            "https" => self.mode_https,
            "dns"   => self.mode_dns,
            "tls"   => self.mode_tls,
            _       => self.mode_other,
        };
        Color::Rgb(r, g, b)
    }
}

impl Default for Theme {
    fn default() -> Self { Theme::colorful() }
}

impl Theme {
    /// Full-color theme (default).
    pub fn colorful() -> Self {
        Self {
            name: "colorful",
            rtt_normal:        Color::White,
            rtt_warn:          Color::Red,
            rtt_alert_bg:      Color::Red,
            rtt_flash_bg:      Color::LightRed,
            rtt_good:          Color::Green,
            rtt_great_bg:      Color::Green,

            drop_color:        Color::Red,
            drop_bar_dim:      (100, 30, 30),
            drop_marker:       (220, 60, 60),
            drop_bg_ascii:     (100, 40, 55),
            drop_bg_unicode:   (80, 30, 45),
            drop_sparkline:    (220, 60, 180),

            grad_low:          (0, 210, 70),
            grad_mid:          (210, 190, 0),
            grad_high:         (210, 40, 0),
            grad_pending:      (45, 45, 45),

            range_scale:       (200, 130, 0),
            range_trail:       [180, 120, 70, 40],

            graph_pending:     (70, 70, 70),
            graph_grid:        (40, 40, 40),
            graph_avg:         (80, 200, 120),
            graph_p95:         (200, 150, 40),
            anim_amber:        (55, 28, 0),
            anim_red:          (180, 0, 0),

            targets: [
                (40,  160, 220),
                (220, 170,  30),
                (60,  200,  90),
                (220,  80, 160),
                (100, 200, 200),
                (220, 110,  40),
                (160, 120, 240),
                (200, 200,  80),
            ],

            mode_icmp:   (100, 180, 255),
            mode_udp:    (255, 190,  80),
            mode_tcp:    ( 80, 210, 120),
            mode_http:   (255, 140,  80),
            mode_https:  ( 80, 200, 180),
            mode_dns:    (140, 200, 255),
            mode_tls:    (160, 200, 255),
            mode_other:  (160, 160, 160),

            hostname:          Color::Cyan,
            hostname_flash_fg: Color::Black,
            hostname_flash_bg: Color::Yellow,
            resolving:         Color::Cyan,
            ip_change:         Color::Yellow,
            log_badge:         Color::Green,

            uptime:      (80, 80, 80),
            col_key_rule:(46, 46, 46),
            xaxis_dim:   (80, 80, 80),
            xaxis_now:   Color::White,
            small_term:  (160, 160, 160),

            dlg_warning:    Color::Yellow,
            dlg_timer:      (120, 120, 120),
            dlg_help_title: Color::Cyan,
            dlg_help_key:   Color::White,
            dlg_help_label: (160, 160, 160),
        }
    }

    /// Colorblind-friendly theme using the Okabe-Ito palette.
    pub fn okabe() -> Self {
        // Okabe-Ito palette (colorblind-safe):
        //   sky blue  (86, 180, 233)   blue      (0, 114, 178)
        //   orange    (230, 159,   0)  vermillion (213, 94, 0)
        //   bluegreen (0, 158, 115)    yellow    (240, 228, 66)
        //   pink      (204, 121, 167)  black     (0, 0, 0)
        Self {
            name: "okabe",
            rtt_normal:        Color::White,
            rtt_warn:          Color::Rgb(230, 159, 0),   // orange
            rtt_alert_bg:      Color::Rgb(180, 80, 0),    // dark orange bg
            rtt_flash_bg:      Color::Rgb(213, 94, 0),    // vermillion bg
            rtt_good:          Color::Rgb(86, 180, 233),  // sky blue
            rtt_great_bg:      Color::Rgb(0, 100, 160),   // dark blue bg

            drop_color:        Color::Rgb(204, 121, 167), // reddish-purple (Okabe-Ito pink)
            drop_bar_dim:      (100, 50, 80),
            drop_marker:       (204, 121, 167),
            drop_bg_ascii:     (70, 35, 60),
            drop_bg_unicode:   (50, 25, 45),
            drop_sparkline:    (204, 121, 167),

            grad_low:          (86, 180, 233),   // sky blue
            grad_mid:          (230, 159,   0),  // orange-yellow
            grad_high:         (213,  94,   0),  // vermillion

            grad_pending:      (45, 45, 45),

            range_scale:       (230, 159, 0),    // orange
            range_trail:       [180, 120, 70, 40],

            graph_pending:     (70, 70, 70),
            graph_grid:        (40, 40, 40),
            graph_avg:         (86, 180, 233),   // sky blue
            graph_p95:         (230, 159, 0),    // orange

            anim_amber:        (55, 35, 0),      // dark orange tint
            anim_red:          (120, 55, 0),     // dark vermillion tint

            targets: [
                ( 86, 180, 233),  // sky blue
                (230, 159,   0),  // orange
                (  0, 158, 115),  // bluish-green
                (204, 121, 167),  // reddish-purple
                (  0, 114, 178),  // blue
                (240, 228,  66),  // yellow
                (213,  94,   0),  // vermillion
                (140, 200, 255),  // light blue
            ],

            mode_icmp:   ( 86, 180, 233),
            mode_udp:    (230, 159,   0),
            mode_tcp:    (  0, 158, 115),
            mode_http:   (213,  94,   0),
            mode_https:  (  0, 114, 178),
            mode_dns:    (140, 200, 255),
            mode_tls:    (160, 210, 255),
            mode_other:  (160, 160, 160),

            hostname:          Color::Rgb(86, 180, 233),
            hostname_flash_fg: Color::Black,
            hostname_flash_bg: Color::Rgb(230, 159, 0),
            resolving:         Color::Rgb(86, 180, 233),
            ip_change:         Color::Rgb(230, 159, 0),
            log_badge:         Color::Rgb(0, 158, 115),

            uptime:      (80, 80, 80),
            col_key_rule:(46, 46, 46),
            xaxis_dim:   (80, 80, 80),
            xaxis_now:   Color::White,
            small_term:  (160, 160, 160),

            dlg_warning:    Color::Rgb(230, 159, 0),
            dlg_timer:      (120, 120, 120),
            dlg_help_title: Color::Rgb(86, 180, 233),
            dlg_help_key:   Color::White,
            dlg_help_label: (160, 160, 160),
        }
    }

    /// Classic CGA screensaver palette.
    /// Worm target colors are the eight CGA bright colors; the rest of the UI
    /// stays readable on a black background.
    pub fn worm() -> Self {
        Self {
            name: "worm",
            rtt_normal:        Color::White,
            rtt_warn:          Color::Rgb(255, 85, 85),
            rtt_alert_bg:      Color::Rgb(170, 0, 0),
            rtt_flash_bg:      Color::Rgb(255, 85, 85),
            rtt_good:          Color::Rgb(85, 255, 85),
            rtt_great_bg:      Color::Rgb(0, 170, 0),

            drop_color:        Color::Rgb(255, 85, 85),
            drop_bar_dim:      (100, 30, 30),
            drop_marker:       (220, 60, 60),
            drop_bg_ascii:     (100, 40, 55),
            drop_bg_unicode:   (80, 30, 45),
            drop_sparkline:    (255, 85, 255),

            grad_low:          (85, 255, 85),
            grad_mid:          (255, 255, 85),
            grad_high:         (255, 85, 85),
            grad_pending:      (40, 40, 40),

            range_scale:       (255, 255, 85),
            range_trail:       [180, 120, 70, 40],

            graph_pending:     (60, 60, 60),
            graph_grid:        (35, 35, 35),
            graph_avg:         (85, 255, 85),
            graph_p95:         (255, 255, 85),
            anim_amber:        (55, 28, 0),
            anim_red:          (170, 0, 0),

            // Eight CGA bright colors - the classic NetWare worm palette
            targets: [
                ( 85, 255, 255), // bright cyan
                ( 85, 255,  85), // bright green
                (255,  85,  85), // bright red
                ( 85,  85, 255), // bright blue
                (255,  85, 255), // bright magenta
                (255, 255,  85), // bright yellow
                (255, 255, 255), // bright white
                (255, 170,   0), // bright orange (CGA brown → orange on RGB)
            ],

            mode_icmp:   ( 85, 255, 255),
            mode_udp:    (255, 255,  85),
            mode_tcp:    ( 85, 255,  85),
            mode_http:   (255, 170,   0),
            mode_https:  ( 85, 255, 255),
            mode_dns:    (170, 170, 255),
            mode_tls:    (170, 200, 255),
            mode_other:  (170, 170, 170),

            hostname:          Color::Rgb(85, 255, 255),
            hostname_flash_fg: Color::Black,
            hostname_flash_bg: Color::Rgb(255, 255, 85),
            resolving:         Color::Rgb(85, 255, 255),
            ip_change:         Color::Rgb(255, 255, 85),
            log_badge:         Color::Rgb(85, 255, 85),

            uptime:      (80, 80, 80),
            col_key_rule:(41, 41, 41),
            xaxis_dim:   (80, 80, 80),
            xaxis_now:   Color::White,
            small_term:  (170, 170, 170),

            dlg_warning:    Color::Rgb(255, 255, 85),
            dlg_timer:      (120, 120, 120),
            dlg_help_title: Color::Rgb(85, 255, 255),
            dlg_help_key:   Color::White,
            dlg_help_label: (170, 170, 170),
        }
    }

    /// Arctic-inspired blue-gray palette (Nord).
    pub fn nord() -> Self {
        // Nord palette - https://www.nordtheme.com/
        //   Polar Night:  #2E3440 #3B4252 #434C5E #4C566A
        //   Snow Storm:   #D8DEE9 #E5E9F0 #ECEFF4
        //   Frost:        #8FBCBB #88C0D0 #81A1C1 #5E81AC
        //   Aurora:       Red #BF616A  Orange #D08770  Yellow #EBCB8B  Green #A3BE8C  Purple #B48EAD
        Self {
            name: "nord",
            rtt_normal:        Color::Rgb(216, 222, 233),  // snow storm
            rtt_warn:          Color::Rgb(235, 203, 139),  // aurora yellow
            rtt_alert_bg:      Color::Rgb(208, 135, 112),  // aurora orange
            rtt_flash_bg:      Color::Rgb(191, 97, 106),   // aurora red
            rtt_good:          Color::Rgb(163, 190, 140),  // aurora green
            rtt_great_bg:      Color::Rgb(94, 129, 172),   // frost blue

            drop_color:        Color::Rgb(191, 97, 106),
            drop_bar_dim:      (90, 50, 55),
            drop_marker:       (191, 97, 106),
            drop_bg_ascii:     (70, 40, 45),
            drop_bg_unicode:   (50, 28, 35),
            drop_sparkline:    (180, 142, 173),             // aurora purple

            grad_low:          (163, 190, 140),  // aurora green
            grad_mid:          (235, 203, 139),  // aurora yellow
            grad_high:         (191, 97, 106),   // aurora red
            grad_pending:      (59, 66, 82),     // polar night

            range_scale:       (235, 203, 139),
            range_trail:       [200, 140, 80, 45],

            graph_pending:     (67, 76, 94),
            graph_grid:        (46, 52, 64),
            graph_avg:         (136, 192, 208),  // frost
            graph_p95:         (235, 203, 139),
            anim_amber:        (60, 50, 20),
            anim_red:          (80, 40, 42),

            targets: [
                (136, 192, 208),  // frost #88C0D0
                (235, 203, 139),  // aurora yellow
                (163, 190, 140),  // aurora green
                (180, 142, 173),  // aurora purple
                (129, 161, 193),  // frost #81A1C1
                (208, 135, 112),  // aurora orange
                (94,  129, 172),  // frost #5E81AC
                (191, 97,  106),  // aurora red
            ],

            mode_icmp:   (136, 192, 208),
            mode_udp:    (235, 203, 139),
            mode_tcp:    (163, 190, 140),
            mode_http:   (208, 135, 112),
            mode_https:  (129, 161, 193),
            mode_dns:    (143, 188, 187),
            mode_tls:    (94,  129, 172),
            mode_other:  (76,  86,  106),

            hostname:          Color::Rgb(136, 192, 208),
            hostname_flash_fg: Color::Rgb(46, 52, 64),
            hostname_flash_bg: Color::Rgb(235, 203, 139),
            resolving:         Color::Rgb(136, 192, 208),
            ip_change:         Color::Rgb(235, 203, 139),
            log_badge:         Color::Rgb(163, 190, 140),

            uptime:      (76, 86, 106),
            col_key_rule:(51, 57, 70),
            xaxis_dim:   (76, 86, 106),
            xaxis_now:   Color::Rgb(216, 222, 233),
            small_term:  (143, 188, 187),

            dlg_warning:    Color::Rgb(235, 203, 139),
            dlg_timer:      (100, 110, 130),
            dlg_help_title: Color::Rgb(136, 192, 208),
            dlg_help_key:   Color::Rgb(216, 222, 233),
            dlg_help_label: (143, 188, 187),
        }
    }

    /// Warm earth-tone palette (Gruvbox dark).
    pub fn gruvbox() -> Self {
        // Gruvbox dark bright - https://github.com/morhetz/gruvbox
        //   fg: #EBDBB2   Red #FB4934  Green #B8BB26  Yellow #FABD2F
        //   Blue #83A598  Purple #D3869B  Aqua #8EC07C  Orange #FE8019
        Self {
            name: "gruvbox",
            rtt_normal:        Color::Rgb(235, 219, 178),  // fg #EBDBB2
            rtt_warn:          Color::Rgb(250, 189, 47),   // bright yellow
            rtt_alert_bg:      Color::Rgb(215, 153, 33),
            rtt_flash_bg:      Color::Rgb(254, 128, 25),   // bright orange
            rtt_good:          Color::Rgb(184, 187, 38),   // bright green
            rtt_great_bg:      Color::Rgb(121, 116, 14),   // dark green

            drop_color:        Color::Rgb(251, 73, 52),    // bright red
            drop_bar_dim:      (120, 35, 25),
            drop_marker:       (251, 73, 52),
            drop_bg_ascii:     (100, 40, 30),
            drop_bg_unicode:   (80, 30, 22),
            drop_sparkline:    (211, 134, 155),             // bright purple

            grad_low:          (184, 187, 38),   // bright green
            grad_mid:          (250, 189, 47),   // bright yellow
            grad_high:         (254, 128, 25),   // bright orange
            grad_pending:      (60, 56, 54),

            range_scale:       (250, 189, 47),
            range_trail:       [200, 140, 80, 45],

            graph_pending:     (80, 73, 69),
            graph_grid:        (60, 56, 54),
            graph_avg:         (142, 192, 124),  // bright aqua
            graph_p95:         (250, 189, 47),
            anim_amber:        (60, 42, 20),
            anim_red:          (100, 35, 25),

            targets: [
                (131, 165, 152),  // bright blue  #83A598
                (250, 189, 47),   // bright yellow #FABD2F
                (184, 187, 38),   // bright green  #B8BB26
                (211, 134, 155),  // bright purple #D3869B
                (142, 192, 124),  // bright aqua   #8EC07C
                (254, 128, 25),   // bright orange #FE8019
                (251, 73,  52),   // bright red    #FB4934
                (235, 219, 178),  // fg            #EBDBB2
            ],

            mode_icmp:   (131, 165, 152),
            mode_udp:    (250, 189, 47),
            mode_tcp:    (184, 187, 38),
            mode_http:   (254, 128, 25),
            mode_https:  (142, 192, 124),
            mode_dns:    (131, 165, 152),
            mode_tls:    (211, 134, 155),
            mode_other:  (168, 153, 132),

            hostname:          Color::Rgb(131, 165, 152),
            hostname_flash_fg: Color::Rgb(40, 40, 40),
            hostname_flash_bg: Color::Rgb(250, 189, 47),
            resolving:         Color::Rgb(131, 165, 152),
            ip_change:         Color::Rgb(250, 189, 47),
            log_badge:         Color::Rgb(184, 187, 38),

            uptime:      (102, 92, 84),
            col_key_rule:(65, 60, 56),
            xaxis_dim:   (102, 92, 84),
            xaxis_now:   Color::Rgb(235, 219, 178),
            small_term:  (168, 153, 132),

            dlg_warning:    Color::Rgb(250, 189, 47),
            dlg_timer:      (120, 110, 95),
            dlg_help_title: Color::Rgb(131, 165, 152),
            dlg_help_key:   Color::Rgb(235, 219, 178),
            dlg_help_label: (168, 153, 132),
        }
    }

    /// Dracula dark theme - purple, pink, cyan accents.
    pub fn dracula() -> Self {
        // Dracula - https://draculatheme.com/
        //   fg #F8F8F2  comment #6272A4
        //   Cyan #8BE9FD  Green #50FA7B  Orange #FFB86C
        //   Pink #FF79C6  Purple #BD93F9  Red #FF5555  Yellow #F1FA8C
        Self {
            name: "dracula",
            rtt_normal:        Color::Rgb(248, 248, 242),  // fg
            rtt_warn:          Color::Rgb(255, 184, 108),  // orange
            rtt_alert_bg:      Color::Rgb(180, 100, 50),
            rtt_flash_bg:      Color::Rgb(255, 85, 85),    // red
            rtt_good:          Color::Rgb(80, 250, 123),   // green
            rtt_great_bg:      Color::Rgb(35, 120, 60),

            drop_color:        Color::Rgb(255, 85, 85),
            drop_bar_dim:      (110, 35, 35),
            drop_marker:       (255, 85, 85),
            drop_bg_ascii:     (80, 28, 28),
            drop_bg_unicode:   (58, 20, 20),
            drop_sparkline:    (255, 121, 198),             // pink

            grad_low:          (80, 250, 123),   // green
            grad_mid:          (241, 250, 140),  // yellow
            grad_high:         (255, 85, 85),    // red
            grad_pending:      (68, 71, 90),

            range_scale:       (241, 250, 140),  // yellow
            range_trail:       [210, 150, 90, 50],

            graph_pending:     (68, 71, 90),
            graph_grid:        (50, 52, 65),
            graph_avg:         (139, 233, 253),  // cyan
            graph_p95:         (255, 184, 108),  // orange
            anim_amber:        (60, 40, 15),
            anim_red:          (100, 28, 28),

            targets: [
                (139, 233, 253),  // cyan    #8BE9FD
                (255, 184, 108),  // orange  #FFB86C
                (80,  250, 123),  // green   #50FA7B
                (255, 121, 198),  // pink    #FF79C6
                (189, 147, 249),  // purple  #BD93F9
                (241, 250, 140),  // yellow  #F1FA8C
                (255, 85,  85),   // red     #FF5555
                (248, 248, 242),  // fg      #F8F8F2
            ],

            mode_icmp:   (139, 233, 253),
            mode_udp:    (255, 184, 108),
            mode_tcp:    (80,  250, 123),
            mode_http:   (255, 184, 108),
            mode_https:  (139, 233, 253),
            mode_dns:    (189, 147, 249),
            mode_tls:    (189, 147, 249),
            mode_other:  (98,  114, 164),

            hostname:          Color::Rgb(189, 147, 249),  // purple
            hostname_flash_fg: Color::Rgb(40, 42, 54),
            hostname_flash_bg: Color::Rgb(255, 184, 108),
            resolving:         Color::Rgb(139, 233, 253),
            ip_change:         Color::Rgb(255, 184, 108),
            log_badge:         Color::Rgb(80, 250, 123),

            uptime:      (98, 114, 164),
            col_key_rule:(55, 57, 71),
            xaxis_dim:   (98, 114, 164),
            xaxis_now:   Color::Rgb(248, 248, 242),
            small_term:  (139, 233, 253),

            dlg_warning:    Color::Rgb(255, 184, 108),
            dlg_timer:      (98, 114, 164),
            dlg_help_title: Color::Rgb(189, 147, 249),
            dlg_help_key:   Color::Rgb(248, 248, 242),
            dlg_help_label: (98, 114, 164),
        }
    }

    /// Solarized dark - Ethan Schoonover's classic.
    pub fn solarized() -> Self {
        // Solarized dark - https://ethanschoonover.com/solarized/
        //   base03 #002B36  base02 #073642  base01 #586E75  base0 #839496  base1 #93A1A1
        //   Yellow #B58900  Orange #CB4B16  Red #DC322F  Magenta #D33682
        //   Violet #6C71C4  Blue #268BD2  Cyan #2AA198  Green #859900
        Self {
            name: "solarized",
            rtt_normal:        Color::Rgb(131, 148, 150),  // base0
            rtt_warn:          Color::Rgb(181, 137, 0),    // yellow
            rtt_alert_bg:      Color::Rgb(203, 75, 22),    // orange
            rtt_flash_bg:      Color::Rgb(220, 50, 47),    // red
            rtt_good:          Color::Rgb(133, 153, 0),    // green
            rtt_great_bg:      Color::Rgb(0, 70, 55),      // dark teal

            drop_color:        Color::Rgb(220, 50, 47),
            drop_bar_dim:      (100, 25, 22),
            drop_marker:       (220, 50, 47),
            drop_bg_ascii:     (75, 20, 18),
            drop_bg_unicode:   (50, 14, 12),
            drop_sparkline:    (211, 54, 130),              // magenta

            grad_low:          (133, 153, 0),    // green
            grad_mid:          (181, 137, 0),    // yellow
            grad_high:         (220, 50, 47),    // red
            grad_pending:      (7, 54, 66),      // base02

            range_scale:       (181, 137, 0),
            range_trail:       [180, 120, 70, 35],

            graph_pending:     (7, 54, 66),      // base02
            graph_grid:        (0, 43, 54),      // base03
            graph_avg:         (42, 161, 152),   // cyan
            graph_p95:         (181, 137, 0),    // yellow
            anim_amber:        (50, 35, 0),
            anim_red:          (80, 20, 18),

            targets: [
                (38,  139, 210),  // blue     #268BD2
                (181, 137, 0),    // yellow   #B58900
                (133, 153, 0),    // green    #859900
                (211, 54,  130),  // magenta  #D33682
                (42,  161, 152),  // cyan     #2AA198
                (203, 75,  22),   // orange   #CB4B16
                (108, 113, 196),  // violet   #6C71C4
                (220, 50,  47),   // red      #DC322F
            ],

            mode_icmp:   (38,  139, 210),
            mode_udp:    (181, 137, 0),
            mode_tcp:    (133, 153, 0),
            mode_http:   (203, 75,  22),
            mode_https:  (42,  161, 152),
            mode_dns:    (108, 113, 196),
            mode_tls:    (108, 113, 196),
            mode_other:  (88,  110, 117),

            hostname:          Color::Rgb(38, 139, 210),
            hostname_flash_fg: Color::Rgb(0, 43, 54),
            hostname_flash_bg: Color::Rgb(181, 137, 0),
            resolving:         Color::Rgb(42, 161, 152),
            ip_change:         Color::Rgb(181, 137, 0),
            log_badge:         Color::Rgb(133, 153, 0),

            uptime:      (88, 110, 117),
            col_key_rule:(5, 48, 59),
            xaxis_dim:   (88, 110, 117),
            xaxis_now:   Color::Rgb(147, 161, 161),  // base1
            small_term:  (131, 148, 150),

            dlg_warning:    Color::Rgb(181, 137, 0),
            dlg_timer:      (88, 110, 117),
            dlg_help_title: Color::Rgb(38, 139, 210),
            dlg_help_key:   Color::Rgb(147, 161, 161),
            dlg_help_label: (88, 110, 117),
        }
    }

    /// High-contrast - fully saturated colors for bright rooms or visibility needs.
    pub fn highcontrast() -> Self {
        Self {
            name: "highcontrast",
            rtt_normal:        Color::White,
            rtt_warn:          Color::Rgb(255, 220, 0),    // pure yellow
            rtt_alert_bg:      Color::Rgb(220, 100, 0),
            rtt_flash_bg:      Color::Rgb(255, 30, 30),
            rtt_good:          Color::Rgb(0, 255, 120),    // pure bright green
            rtt_great_bg:      Color::Rgb(0, 180, 0),

            drop_color:        Color::Rgb(255, 30, 30),
            drop_bar_dim:      (130, 15, 15),
            drop_marker:       (255, 30, 30),
            drop_bg_ascii:     (100, 12, 12),
            drop_bg_unicode:   (70, 8, 8),
            drop_sparkline:    (255, 0, 200),               // magenta

            grad_low:          (0, 255, 120),    // bright green
            grad_mid:          (255, 220, 0),    // pure yellow
            grad_high:         (255, 30, 30),    // pure red
            grad_pending:      (40, 40, 40),

            range_scale:       (255, 220, 0),
            range_trail:       [220, 160, 100, 50],

            graph_pending:     (50, 50, 50),
            graph_grid:        (35, 35, 35),
            graph_avg:         (0, 230, 255),    // bright cyan
            graph_p95:         (255, 200, 0),
            anim_amber:        (60, 30, 0),
            anim_red:          (180, 0, 0),

            targets: [
                (0,   220, 255),  // bright cyan
                (255, 220, 0),    // bright yellow
                (0,   255, 120),  // bright green
                (255, 0,   200),  // magenta
                (120, 120, 255),  // bright blue
                (255, 140, 0),    // bright orange
                (200, 0,   255),  // violet
                (255, 255, 255),  // white
            ],

            mode_icmp:   (0,   220, 255),
            mode_udp:    (255, 220, 0),
            mode_tcp:    (0,   255, 120),
            mode_http:   (255, 140, 0),
            mode_https:  (0,   220, 255),
            mode_dns:    (160, 160, 255),
            mode_tls:    (200, 200, 255),
            mode_other:  (180, 180, 180),

            hostname:          Color::Rgb(0, 220, 255),
            hostname_flash_fg: Color::Black,
            hostname_flash_bg: Color::Rgb(255, 220, 0),
            resolving:         Color::Rgb(0, 220, 255),
            ip_change:         Color::Rgb(255, 220, 0),
            log_badge:         Color::Rgb(0, 255, 120),

            uptime:      (90, 90, 90),
            col_key_rule:(43, 43, 43),
            xaxis_dim:   (90, 90, 90),
            xaxis_now:   Color::White,
            small_term:  (180, 180, 180),

            dlg_warning:    Color::Rgb(255, 220, 0),
            dlg_timer:      (130, 130, 130),
            dlg_help_title: Color::Rgb(0, 220, 255),
            dlg_help_key:   Color::White,
            dlg_help_label: (180, 180, 180),
        }
    }

    /// Amber phosphor CRT monitor - warm monochrome.
    pub fn phosphor() -> Self {
        Self {
            name: "phosphor",
            rtt_normal:        Color::Rgb(255, 176, 0),    // amber glow
            rtt_warn:          Color::Rgb(255, 140, 0),    // orange-amber
            rtt_alert_bg:      Color::Rgb(180, 80, 0),
            rtt_flash_bg:      Color::Rgb(220, 110, 0),
            rtt_good:          Color::Rgb(255, 210, 80),   // bright amber
            rtt_great_bg:      Color::Rgb(120, 70, 0),

            drop_color:        Color::Rgb(255, 100, 20),   // reddish amber
            drop_bar_dim:      (100, 55, 5),
            drop_marker:       (220, 100, 20),
            drop_bg_ascii:     (80, 42, 5),
            drop_bg_unicode:   (55, 28, 3),
            drop_sparkline:    (220, 120, 30),

            grad_low:          (255, 220, 100),  // bright golden
            grad_mid:          (220, 150, 0),    // medium amber
            grad_high:         (160, 80, 0),     // dark amber
            grad_pending:      (50, 28, 0),

            range_scale:       (220, 150, 0),
            range_trail:       [220, 150, 80, 40],

            graph_pending:     (55, 30, 0),
            graph_grid:        (40, 22, 0),
            graph_avg:         (255, 200, 60),
            graph_p95:         (200, 130, 0),
            anim_amber:        (60, 32, 0),
            anim_red:          (80, 40, 0),

            targets: [
                (255, 200, 60),   // bright amber
                (255, 140, 0),    // orange-amber
                (220, 220, 80),   // yellowish
                (255, 100, 20),   // reddish amber
                (180, 160, 0),    // golden-green
                (255, 220, 120),  // pale amber
                (160, 100, 0),    // dark amber
                (220, 170, 30),   // warm gold
            ],

            mode_icmp:   (255, 200, 60),
            mode_udp:    (220, 150, 0),
            mode_tcp:    (180, 160, 0),
            mode_http:   (255, 140, 0),
            mode_https:  (200, 180, 40),
            mode_dns:    (220, 190, 60),
            mode_tls:    (180, 140, 0),
            mode_other:  (130, 85,  0),

            hostname:          Color::Rgb(255, 200, 60),
            hostname_flash_fg: Color::Rgb(30, 15, 0),
            hostname_flash_bg: Color::Rgb(255, 176, 0),
            resolving:         Color::Rgb(255, 176, 0),
            ip_change:         Color::Rgb(255, 200, 60),
            log_badge:         Color::Rgb(220, 180, 40),

            uptime:      (100, 55, 0),
            col_key_rule:(46, 25, 0),
            xaxis_dim:   (100, 55, 0),
            xaxis_now:   Color::Rgb(255, 200, 60),
            small_term:  (150, 90, 0),

            dlg_warning:    Color::Rgb(255, 176, 0),
            dlg_timer:      (120, 70, 0),
            dlg_help_title: Color::Rgb(255, 200, 60),
            dlg_help_key:   Color::Rgb(255, 220, 100),
            dlg_help_label: (160, 100, 0),
        }
    }

    /// Retro green phosphor + amber CRT - classic 80s terminal.
    pub fn retro() -> Self {
        Self {
            name: "retro",
            rtt_normal:        Color::Rgb(100, 210, 55),    // phosphor green
            rtt_warn:          Color::Rgb(210, 165, 0),     // amber
            rtt_alert_bg:      Color::Rgb(140, 85, 0),
            rtt_flash_bg:      Color::Rgb(195, 115, 0),
            rtt_good:          Color::Rgb(150, 255, 85),    // bright green glow
            rtt_great_bg:      Color::Rgb(20, 90, 10),

            drop_color:        Color::Rgb(220, 105, 0),     // hot amber
            drop_bar_dim:      (80, 45, 0),
            drop_marker:       (200, 100, 10),
            drop_bg_ascii:     (60, 32, 0),
            drop_bg_unicode:   (40, 20, 0),
            drop_sparkline:    (210, 145, 0),

            grad_low:          (100, 235, 55),   // bright phosphor green
            grad_mid:          (210, 165, 0),    // amber
            grad_high:         (155, 75, 0),     // dark amber
            grad_pending:      (18, 42, 8),

            range_scale:       (195, 148, 0),
            range_trail:       [200, 130, 70, 35],

            graph_pending:     (22, 50, 10),
            graph_grid:        (14, 32, 6),
            graph_avg:         (70, 185, 40),
            graph_p95:         (195, 148, 0),
            anim_amber:        (30, 58, 8),
            anim_red:          (65, 95, 0),

            targets: [
                ( 90, 225, 50),   // bright phosphor green
                (210, 170, 0),    // warm amber
                (155, 255, 85),   // lime glow
                (245, 205, 45),   // golden amber
                ( 45, 175, 40),   // medium green
                (255, 225, 85),   // pale amber/yellow
                ( 55, 135, 28),   // dark green
                (175, 138, 0),    // dark amber
            ],

            mode_icmp:   ( 90, 215, 50),
            mode_udp:    (200, 160, 0),
            mode_tcp:    (120, 230, 60),
            mode_http:   (220, 170, 0),
            mode_https:  ( 80, 200, 45),
            mode_dns:    (150, 220, 70),
            mode_tls:    (100, 190, 50),
            mode_other:  ( 70, 105, 35),

            hostname:          Color::Rgb(100, 228, 55),
            hostname_flash_fg: Color::Rgb(4, 14, 2),
            hostname_flash_bg: Color::Rgb(200, 160, 0),
            resolving:         Color::Rgb(80, 198, 45),
            ip_change:         Color::Rgb(200, 160, 0),
            log_badge:         Color::Rgb(90, 200, 50),

            uptime:      (50, 88, 22),
            col_key_rule:(17, 39, 8),
            xaxis_dim:   (48, 85, 20),
            xaxis_now:   Color::Rgb(125, 242, 65),
            small_term:  (78, 135, 38),

            dlg_warning:    Color::Rgb(210, 162, 0),
            dlg_timer:      (75, 115, 32),
            dlg_help_title: Color::Rgb(100, 228, 55),
            dlg_help_key:   Color::Rgb(145, 255, 82),
            dlg_help_label: (88, 148, 42),
        }
    }

    /// Monochrome theme - no hue, brightness only.
    pub fn nocolor() -> Self {
        Self {
            name: "nocolor",
            rtt_normal:        Color::White,
            rtt_warn:          Color::White,
            rtt_alert_bg:      Color::DarkGray,
            rtt_flash_bg:      Color::DarkGray,
            rtt_good:          Color::White,
            rtt_great_bg:      Color::DarkGray,

            drop_color:        Color::White,
            drop_bar_dim:      (80, 80, 80),
            drop_marker:       (180, 180, 180),
            drop_bg_ascii:     (55, 55, 55),
            drop_bg_unicode:   (38, 38, 38),
            drop_sparkline:    (160, 160, 160),

            grad_low:          (200, 200, 200),
            grad_mid:          (140, 140, 140),
            grad_high:         (80, 80, 80),
            grad_pending:      (30, 30, 30),

            range_scale:       (160, 160, 160),
            range_trail:       [160, 120, 80, 40],

            graph_pending:     (50, 50, 50),
            graph_grid:        (35, 35, 35),
            graph_avg:         (140, 140, 140),
            graph_p95:         (100, 100, 100),
            anim_amber:        (60, 60, 60),
            anim_red:          (90, 90, 90),

            targets: [
                (220, 220, 220),
                (170, 170, 170),
                (130, 130, 130),
                (100, 100, 100),
                (200, 200, 200),
                (155, 155, 155),
                (80,  80,  80),
                (115, 115, 115),
            ],

            mode_icmp:   (140, 140, 140),
            mode_udp:    (140, 140, 140),
            mode_tcp:    (140, 140, 140),
            mode_http:   (140, 140, 140),
            mode_https:  (140, 140, 140),
            mode_dns:    (140, 140, 140),
            mode_tls:    (140, 140, 140),
            mode_other:  (100, 100, 100),

            hostname:          Color::White,
            hostname_flash_fg: Color::Black,
            hostname_flash_bg: Color::White,
            resolving:         Color::White,
            ip_change:         Color::White,
            log_badge:         Color::White,

            uptime:      (70, 70, 70),
            col_key_rule:(40, 40, 40),
            xaxis_dim:   (70, 70, 70),
            xaxis_now:   Color::White,
            small_term:  (110, 110, 110),

            dlg_warning:    Color::White,
            dlg_timer:      (90, 90, 90),
            dlg_help_title: Color::White,
            dlg_help_key:   Color::White,
            dlg_help_label: (110, 110, 110),
        }
    }
}
