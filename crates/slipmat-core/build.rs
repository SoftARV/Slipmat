// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Emit the workspace root, for `sidecar::locate`'s dev-tree fallback.
//!
//! `CARGO_MANIFEST_DIR` is *this crate's* directory in a workspace, so using it
//! there would look for `crates/slipmat-core/sidecar`, miss, and fall through to
//! an installed sidecar — fresh Rust against stale JavaScript, silently. That is
//! the failure `warn_if_shadowing_a_build_tree` exists to catch.

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this");
    let root = std::path::Path::new(&manifest)
        .ancestors()
        .nth(2)
        .expect("crates/<name>/ is two levels below the workspace root");
    println!("cargo::rustc-env=SLIPMAT_WORKSPACE_ROOT={}", root.display());
    println!("cargo::rerun-if-changed=build.rs");
}
