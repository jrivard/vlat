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

// Platform-independent: rendering, view state, CLI types, shared constants.
// These have no networking/OS dependencies and compile for wasm32 as well
// as native targets, so the browser demo (src/bin/vlat_web.rs) can reuse
// the exact same view code as the native TUI.
pub mod cli;
pub mod constants;
pub mod demo;
pub mod state;
pub mod time;
pub mod types;
pub mod ui;

// Native-only: real probing, DNS resolution, session persistence, log
// files, and the crossterm/tokio event loop. None of this targets wasm32.
#[cfg(feature = "native")]
pub mod app;
#[cfg(feature = "native")]
pub mod logfile;
#[cfg(feature = "native")]
pub mod output;
#[cfg(feature = "native")]
pub mod paths;
#[cfg(feature = "native")]
pub mod probe;
#[cfg(feature = "native")]
pub mod resolver;
#[cfg(feature = "native")]
pub mod session;
