// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Where Slipmat keeps things on disk.
//!
//! Read straight from the environment rather than through a toolkit: every
//! frontend and the daemon have to agree on these paths, and a GTK dependency
//! for two `std::env::var` calls would put the engine behind a display server.

use std::path::PathBuf;

fn xdg(var: &str, fallback: &str) -> Option<PathBuf> {
    match std::env::var(var) {
        Ok(x) if !x.is_empty() => Some(PathBuf::from(x)),
        _ => Some(PathBuf::from(std::env::var("HOME").ok()?).join(fallback)),
    }
}

/// `$XDG_CACHE_HOME/slipmat`, else `~/.cache/slipmat`.
pub fn cache_dir() -> Option<PathBuf> {
    Some(xdg("XDG_CACHE_HOME", ".cache")?.join("slipmat"))
}

/// Cached cover art. A subdirectory so `prune` can clear it without touching
/// the library cache beside it.
pub fn artwork_dir() -> Option<PathBuf> {
    Some(cache_dir()?.join("artwork"))
}

/// `$XDG_STATE_HOME/slipmat`, else `~/.local/state/slipmat`.
pub fn state_dir() -> Option<PathBuf> {
    Some(xdg("XDG_STATE_HOME", ".local/state")?.join("slipmat"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_directory_sits_under_its_own_xdg_base() {
        // The three are separate on purpose: a cache may be deleted at any time,
        // state may not. Sharing a base would make clearing one clear the other.
        let (cache, state) = (cache_dir().unwrap(), state_dir().unwrap());
        assert!(cache.ends_with("slipmat"));
        assert!(state.ends_with("slipmat"));
        assert_ne!(cache, state);
        assert!(artwork_dir().unwrap().starts_with(&cache));
    }
}
