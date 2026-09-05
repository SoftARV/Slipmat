// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

use std::os::unix::fs::symlink;

use slipmat_core::session::Session;

#[test]
fn persisted_snapshots_replace_the_destination() {
    const CHILD: &str = "SLIPMAT_PERSISTENCE_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let library = slipmat_core::paths::cache_dir()
            .expect("test cache directory")
            .join("library.json");
        let session = slipmat_core::paths::state_dir()
            .expect("test state directory")
            .join("session.json");

        for path in [&library, &session] {
            std::fs::create_dir_all(path.parent().expect("persistence directory")).unwrap();
            let previous = path.with_extension("previous");
            std::fs::write(&previous, "previous complete snapshot").unwrap();
            symlink(&previous, path).unwrap();
        }

        slipmat_core::library_cache::save(&[], &[], &[], &[]);
        let saved = Session {
            songs: vec!["1440857781".into()],
            start: 0,
            position_ms: 42_000,
        };
        slipmat_core::session::save(&saved);

        for path in [&library, &session] {
            assert_eq!(
                std::fs::read_to_string(path.with_extension("previous")).unwrap(),
                "previous complete snapshot"
            );
            assert!(!std::fs::symlink_metadata(path).unwrap().is_symlink());
        }
        assert_eq!(
            slipmat_core::session::load()
                .expect("saved session")
                .position_ms,
            42_000
        );
        return;
    }

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "slipmat-persistence-{}-{unique}",
        std::process::id()
    ));
    let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "persisted_snapshots_replace_the_destination",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("HOME", root.join("home"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_STATE_HOME", root.join("state"))
        .status()
        .expect("run isolated persistence test");
    let _ = std::fs::remove_dir_all(root);
    assert!(status.success());
}
