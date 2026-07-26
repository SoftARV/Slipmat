// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The window's furniture: the primary menu's actions and accelerators, and the
//! three dialogs behind them.
//!
//! All built imperatively rather than in `view!`, because they are presented on
//! demand and own no state of their own — every change one of them makes goes
//! straight back through an `AppMsg`, so the reducer stays the only writer.

use relm4::adw::prelude::*;
use relm4::{ComponentSender, adw, gtk};

use super::{AppModel, AppMsg};
use crate::style::Accent;

// The primary menu's action group. GTK menu items invoke `GAction`s by name;
// each of these bridges to an `AppMsg` so the reducer stays the only place
// state changes.
relm4::new_action_group!(AppMenuActionGroup, "win");
relm4::new_stateless_action!(PreferencesAction, AppMenuActionGroup, "preferences");
relm4::new_stateless_action!(ShortcutsAction, AppMenuActionGroup, "shortcuts");
relm4::new_stateless_action!(AboutAction, AppMenuActionGroup, "about");
relm4::new_stateless_action!(QuitAction, AppMenuActionGroup, "quit");
relm4::new_stateless_action!(PlayPauseAction, AppMenuActionGroup, "play-pause");
relm4::new_stateless_action!(NextAction, AppMenuActionGroup, "next");
relm4::new_stateless_action!(PreviousAction, AppMenuActionGroup, "previous");
relm4::new_stateless_action!(ToggleQueueAction, AppMenuActionGroup, "toggle-queue");
relm4::new_stateless_action!(ToggleSidebarAction, AppMenuActionGroup, "toggle-sidebar");

/// Wire the primary menu's actions to messages, with their accelerators.
pub(super) fn register_actions(
    window: &adw::ApplicationWindow,
    sender: &ComponentSender<AppModel>,
) {
    use relm4::actions::{AccelsPlus, RelmAction, RelmActionGroup};

    let mut group = RelmActionGroup::<AppMenuActionGroup>::new();

    let s = sender.clone();
    group.add_action(RelmAction::<PreferencesAction>::new_stateless(move |_| {
        s.input(AppMsg::ShowPreferences)
    }));
    let s = sender.clone();
    group.add_action(RelmAction::<ShortcutsAction>::new_stateless(move |_| {
        s.input(AppMsg::ShowShortcuts)
    }));
    let s = sender.clone();
    group.add_action(RelmAction::<AboutAction>::new_stateless(move |_| {
        s.input(AppMsg::ShowAbout)
    }));
    group.add_action(RelmAction::<QuitAction>::new_stateless(move |_| {
        relm4::main_application().quit()
    }));

    // Transport, so the app answers the keyboard even when the bar does not
    // have focus. Media keys already arrive over MPRIS; these are the
    // in-window equivalents.
    let s = sender.clone();
    group.add_action(RelmAction::<PlayPauseAction>::new_stateless(move |_| {
        s.input(AppMsg::PlayPause)
    }));
    let s = sender.clone();
    group.add_action(RelmAction::<NextAction>::new_stateless(move |_| {
        s.input(AppMsg::Next)
    }));
    let s = sender.clone();
    group.add_action(RelmAction::<PreviousAction>::new_stateless(move |_| {
        s.input(AppMsg::Previous)
    }));
    let s = sender.clone();
    group.add_action(RelmAction::<ToggleQueueAction>::new_stateless(move |_| {
        s.input(AppMsg::ToggleQueue)
    }));
    let s = sender.clone();
    group.add_action(RelmAction::<ToggleSidebarAction>::new_stateless(
        move |_| s.input(AppMsg::ToggleSidebar),
    ));

    let app = relm4::main_application();
    app.set_accelerators_for_action::<PreferencesAction>(&["<Control>comma"]);
    app.set_accelerators_for_action::<ShortcutsAction>(&["<Control>question"]);
    app.set_accelerators_for_action::<QuitAction>(&["<Control>q"]);
    app.set_accelerators_for_action::<PlayPauseAction>(&["<Control>space"]);
    app.set_accelerators_for_action::<NextAction>(&["<Control>Right"]);
    app.set_accelerators_for_action::<PreviousAction>(&["<Control>Left"]);
    app.set_accelerators_for_action::<ToggleQueueAction>(&["<Control>u"]);
    // F9 is the GNOME convention for showing and hiding a sidebar.
    app.set_accelerators_for_action::<ToggleSidebarAction>(&["F9"]);

    group.register_for_widget(window);
}

/// Check an icon name against the theme, falling back if it is missing.
///
/// A name that does not exist renders as nothing at all — silently, with no
/// warning — which is how `music-note-single-symbolic` shipped as an invisible
/// icon.
pub(super) fn icon(name: &'static str) -> &'static str {
    let present = gtk::gdk::Display::default()
        .map(|display| gtk::IconTheme::for_display(&display))
        .is_some_and(|theme| theme.has_icon(name));
    if present {
        name
    } else {
        tracing::warn!(icon = name, "icon missing from the theme; falling back");
        "audio-x-generic-symbolic"
    }
}

pub(super) fn show_about(parent: &adw::ApplicationWindow) {
    let about = adw::AboutDialog::builder()
        .application_name("Tonearm")
        .application_icon(crate::APP_ID)
        .developer_name("Miguel Rincon")
        .version(env!("CARGO_PKG_VERSION"))
        .license_type(gtk::License::Gpl30)
        .website("https://github.com/SoftARV/Tonearm")
        .issue_url("https://github.com/SoftARV/Tonearm/issues")
        .comments(
            "A native GNOME client for Apple Music.\n\n\
             Playback runs through Apple's own MusicKit player using Google's \
             Widevine CDM, in a hidden helper process. Tonearm is a native \
             front-end for a licensed session — it requires an active Apple \
             Music subscription and an internet connection.",
        )
        .build();
    about.present(Some(parent));
}

pub(super) fn show_shortcuts(parent: &adw::ApplicationWindow) {
    // Built by hand rather than from a .ui file: it is a dozen lines either
    // way, and this keeps the strings next to the code that implements them.
    let dialog = adw::ShortcutsDialog::new();

    let playback = adw::ShortcutsSection::new(Some("Playback"));
    for (title, accel) in [
        ("Play or pause", "<Control>space"),
        ("Next track", "<Control>Right"),
        ("Previous track", "<Control>Left"),
    ] {
        playback.add(adw::ShortcutsItem::new(title, accel));
    }

    let general = adw::ShortcutsSection::new(Some("General"));
    for (title, accel) in [
        ("Toggle the sidebar", "F9"),
        ("Toggle the queue", "<Control>u"),
        ("Preferences", "<Control>comma"),
        ("Keyboard shortcuts", "<Control>question"),
        ("Quit", "<Control>q"),
    ] {
        general.add(adw::ShortcutsItem::new(title, accel));
    }

    dialog.add(playback);
    dialog.add(general);
    dialog.present(Some(parent));
}

impl AppModel {
    /// Preferences: theme and the track-change notification.
    ///
    /// Built imperatively rather than in `view!` because it is presented on
    /// demand and owns no state of its own — every change goes straight back
    /// through `AppMsg` so the reducer stays the only writer.
    pub(super) fn show_preferences(
        &self,
        sender: &ComponentSender<Self>,
        parent: &adw::ApplicationWindow,
    ) {
        let dialog = adw::PreferencesDialog::new();
        let page = adw::PreferencesPage::new();

        let appearance = adw::PreferencesGroup::builder().title("Appearance").build();
        let theme = adw::ComboRow::builder()
            .title("Theme")
            .model(&gtk::StringList::new(&["Follow System", "Light", "Dark"]))
            .selected(self.settings.theme.index())
            .build();
        {
            let sender = sender.clone();
            theme.connect_selected_notify(move |row| {
                sender.input(AppMsg::SetTheme(row.selected()));
            });
        }
        appearance.add(&theme);

        let names: Vec<&str> = Accent::ALL.iter().map(|a| a.label()).collect();
        let accent = adw::ComboRow::builder()
            .title("Accent Colour")
            .model(&gtk::StringList::new(&names))
            .selected(Accent::parse(&self.settings.accent).index())
            .build();
        {
            let sender = sender.clone();
            accent.connect_selected_notify(move |row| {
                sender.input(AppMsg::SetAccent(Accent::from_index(row.selected())));
            });
        }
        appearance.add(&accent);

        let notifications = adw::PreferencesGroup::builder()
            .title("Notifications")
            .description("Notifications only appear once Tonearm is installed — see the README.")
            .build();
        let notify = adw::SwitchRow::builder()
            .title("Notify on track change")
            .subtitle("Show a notification when a new song starts")
            .active(self.settings.notify_track_change)
            .build();
        {
            let sender = sender.clone();
            notify.connect_active_notify(move |row| {
                sender.input(AppMsg::SetNotifyTrackChange(row.is_active()));
            });
        }
        notifications.add(&notify);

        page.add(&appearance);
        page.add(&notifications);
        dialog.add(&page);
        dialog.present(Some(parent));
    }
}
