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

use crate::time::Instant;
use ratatui::{Frame, layout::Rect, style::{Color, Style}, text::{Line, Span}, widgets::{Clear, Paragraph}};
use super::Theme;

// Draw-in: one glyph at a time, letter-by-letter, row-by-row within each letter.
// 4 letters × 3 rows × 3 cols = 36 glyphs total.
const CHAR_MS:   u64   = 60;
const COLS:      usize = 3;
const ROWS:      usize = 3;
const NUM_LETTERS: usize = 4;
const CHARS_PER_LETTER: usize = ROWS * COLS;            // 9
const TOTAL_CHARS:      usize = NUM_LETTERS * CHARS_PER_LETTER; // 36
const DRAW_IN_MS: u64 = CHAR_MS * TOTAL_CHARS as u64;  // 2160 ms

// Draw-out: one whole letter erased at a time (right→left), fast wipe.
const LETTER_MS:   u64 = 150;
const DRAW_OUT_MS: u64 = LETTER_MS * NUM_LETTERS as u64; // 600 ms

const V: [&str; ROWS] = ["╻ ╻", "┃┏┛", "┗┛ "];
const L: [&str; ROWS] = ["╻  ", "┃  ", "┗━╸"];
const A: [&str; ROWS] = ["┏━┓", "┣━┫", "╹ ╹"];
const T: [&str; ROWS] = ["╺┳╸", " ┃ ", " ╹ "];
const LETTERS: [[&str; ROWS]; NUM_LETTERS] = [V, L, A, T];

#[derive(PartialEq, Clone)]
pub enum LogoPhase { Waiting, DrawIn, Idle, DrawOut, Done }

#[derive(Clone)]
pub struct LogoAnim {
    pub phase:    LogoPhase,
    phase_start:  Instant,
    /// How long to stay in Idle before DrawOut (None = wait for start_draw_out())
    idle_ms: Option<u64>,
    /// How long to wait after DrawOut before cycling (None = Done)
    wait_ms: Option<u64>,
    /// If Some, used as the wait duration for the very first Waiting phase instead of wait_ms
    initial_wait_ms: Option<u64>,
}

impl PartialEq for LogoAnim {
    fn eq(&self, other: &Self) -> bool { self.phase == other.phase }
}

impl LogoAnim {
    pub const WIDTH:  u16 = 12;
    pub const HEIGHT: u16 = 3;

    /// One-shot: DrawIn → Idle (until start_draw_out()) → DrawOut → Done.
    pub fn new() -> Self {
        Self { phase: LogoPhase::DrawIn, phase_start: Instant::now(), idle_ms: None, wait_ms: None, initial_wait_ms: None }
    }

    /// Cycling: Waiting → DrawIn → Idle → DrawOut → Waiting → …
    /// `initial_wait_secs`: delay before the very first appearance.
    /// `cycle_wait_secs`: delay between subsequent appearances.
    pub fn new_cycling(idle_secs: u64, initial_wait_secs: u64, cycle_wait_secs: u64) -> Self {
        Self {
            phase: LogoPhase::Waiting,
            phase_start: Instant::now(),
            idle_ms: Some(idle_secs * 1000),
            wait_ms: Some(cycle_wait_secs * 1000),
            initial_wait_ms: Some(initial_wait_secs * 1000),
        }
    }

    pub fn start_draw_out(&mut self) {
        if !matches!(self.phase, LogoPhase::Done | LogoPhase::DrawOut) {
            self.phase = LogoPhase::DrawOut;
            self.phase_start = Instant::now();
        }
    }

    pub fn is_done(&self) -> bool { self.phase == LogoPhase::Done }

    /// True while visually animating - drives fast_ticker wakeup.
    pub fn is_active(&self) -> bool {
        matches!(self.phase, LogoPhase::DrawIn | LogoPhase::Idle | LogoPhase::DrawOut)
    }

    pub fn tick(&mut self) {
        let elapsed = self.phase_start.elapsed().as_millis() as u64;
        match self.phase {
            LogoPhase::Waiting => {
                let threshold = self.initial_wait_ms.or(self.wait_ms);
                if let Some(wait) = threshold {
                    if elapsed >= wait {
                        self.initial_wait_ms = None; // first wait consumed; use wait_ms from here on
                        self.phase = LogoPhase::DrawIn;
                        self.phase_start = Instant::now();
                    }
                }
            }
            LogoPhase::DrawIn if elapsed >= DRAW_IN_MS => {
                self.phase = LogoPhase::Idle;
                self.phase_start = Instant::now();
            }
            LogoPhase::Idle => {
                if let Some(idle) = self.idle_ms {
                    if elapsed >= idle {
                        self.phase = LogoPhase::DrawOut;
                        self.phase_start = Instant::now();
                    }
                }
            }
            LogoPhase::DrawOut if elapsed >= DRAW_OUT_MS => {
                self.phase = match self.wait_ms {
                    Some(_) => LogoPhase::Waiting,
                    None    => LogoPhase::Done,
                };
                self.phase_start = Instant::now();
            }
            _ => {}
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        if matches!(self.phase, LogoPhase::Waiting | LogoPhase::Done) { return; }

        let elapsed = self.phase_start.elapsed().as_millis() as u64;

        // For Idle: slow wave offset drifts across individual characters
        let wave_offset = if self.phase == LogoPhase::Idle {
            (elapsed as f64 * 0.00010) % 1.0
        } else {
            0.0
        };

        // How many glyphs are revealed (DrawIn) or how many letters are hidden (DrawOut)
        let draw_in_revealed = if self.phase == LogoPhase::DrawIn {
            (elapsed / CHAR_MS).min(TOTAL_CHARS as u64) as usize
        } else {
            TOTAL_CHARS  // Idle / DrawOut: all glyphs "revealed" before DrawOut logic kicks in
        };

        let draw_out_hidden = if self.phase == LogoPhase::DrawOut {
            (elapsed / LETTER_MS).min(NUM_LETTERS as u64) as usize
        } else {
            0
        };

        let lines: Vec<Line> = (0..ROWS).map(|row| {
            let mut spans = Vec::with_capacity(NUM_LETTERS * COLS);
            for (i, letter) in LETTERS.iter().enumerate() {
                // DrawOut erases whole letters right-to-left
                if NUM_LETTERS - i <= draw_out_hidden {
                    spans.push(Span::raw("   "));
                    continue;
                }

                if self.phase == LogoPhase::Idle {
                    // Idle: each character gets its own color in the wave
                    for (col, ch) in letter[row].chars().enumerate() {
                        let char_idx = i * CHARS_PER_LETTER + row * COLS + col;
                        let base_t = char_idx as f64 / (TOTAL_CHARS as f64 - 1.0);
                        let t = (base_t + wave_offset) % 1.0;
                        let (r, g, b) = theme.gradient_color(t);
                        spans.push(Span::styled(ch.to_string(), Style::default().fg(Color::Rgb(r, g, b))));
                    }
                } else {
                    // DrawIn: reveal char-by-char with per-letter color
                    let letter_row_start = i * CHARS_PER_LETTER + row * COLS;
                    let chars_shown = draw_in_revealed.saturating_sub(letter_row_start).min(COLS);
                    let row_content: String = letter[row].chars().take(chars_shown).collect();
                    let padding = COLS - chars_shown;
                    let base_t = i as f64 / (NUM_LETTERS as f64 - 1.0);
                    let (r, g, b) = theme.gradient_color(base_t);
                    let style = Style::default().fg(Color::Rgb(r, g, b));
                    if chars_shown > 0 {
                        spans.push(Span::styled(row_content, style));
                    }
                    if padding > 0 {
                        spans.push(Span::raw(" ".repeat(padding)));
                    }
                }
            }
            Line::from(spans)
        }).collect();

        f.render_widget(Clear, area);
        f.render_widget(Paragraph::new(lines), area);
    }
}
