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

//! Shared XDG base directory resolution, used by both the config file and
//! the session store. Always follows the XDG spec regardless of OS (unlike
//! `etcetera::choose_base_strategy()`, which switches to native Windows
//! paths on Windows) so behavior matches the project's existing convention
//! of `~/.config/vlat` / `~/.local/state/vlat` everywhere.

use etcetera::base_strategy::{BaseStrategy, Xdg};
use std::path::{Path, PathBuf};

fn xdg() -> Option<Xdg> {
    Xdg::new().ok()
}

/// Rebuild a path from its components so every separator is the platform's
/// own. etcetera's `Xdg` joins literal `".config/"` / `".local/state/"`, which
/// on Windows yields mixed paths like `C:\Users\x\.config/vlat\defaults`.
/// A no-op on Unix.
fn normalize(p: &Path) -> PathBuf {
    p.components().collect()
}

/// `$XDG_CONFIG_HOME` (if absolute), else `~/.config`, else `./.config` if
/// the home directory can't be located.
pub fn xdg_config_dir() -> PathBuf {
    normalize(&xdg().map(|x| x.config_dir()).unwrap_or_else(|| PathBuf::from(".config")))
}

/// `$XDG_STATE_HOME` (if absolute), else `~/.local/state`, else
/// `./.local/state` if the home directory can't be located.
pub fn xdg_state_dir() -> PathBuf {
    normalize(&xdg().and_then(|x| x.state_dir()).unwrap_or_else(|| PathBuf::from(".local/state")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirs_have_no_trailing_or_foreign_separators() {
        for dir in [xdg_config_dir(), xdg_state_dir()] {
            let s = dir.display().to_string();
            assert!(!s.ends_with(['/', '\\']), "{s}");
            #[cfg(windows)]
            assert!(!s.contains('/'), "{s}");
        }
    }

    #[test]
    fn normalize_keeps_unix_paths_intact() {
        assert_eq!(normalize(Path::new("/home/x/.config/")), PathBuf::from("/home/x/.config"));
        assert_eq!(normalize(Path::new(".local/state")), PathBuf::from(".local/state"));
    }
}
