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
relm4::new_stateless_action!(PlayPauseAction, AppMenuActionGroup, "play-pause");
relm4::new_stateless_action!(NextAction, AppMenuActionGroup, "next");
relm4::new_stateless_action!(PreviousAction, AppMenuActionGroup, "previous");
relm4::new_stateless_action!(VolumeUpAction, AppMenuActionGroup, "volume-up");
relm4::new_stateless_action!(VolumeDownAction, AppMenuActionGroup, "volume-down");
relm4::new_stateless_action!(ToggleQueueAction, AppMenuActionGroup, "toggle-queue");
relm4::new_stateless_action!(ToggleSidebarAction, AppMenuActionGroup, "toggle-sidebar");
relm4::new_stateless_action!(SignOutAction, AppMenuActionGroup, "sign-out");

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
    group.add_action(RelmAction::<SignOutAction>::new_stateless(move |_| {
        s.input(AppMsg::SignOut)
    }));
    let s = sender.clone();
    group.add_action(RelmAction::<AboutAction>::new_stateless(move |_| {
        s.input(AppMsg::ShowAbout)
    }));
    // **Application-scoped, not window-scoped.** A `win.` action resolves
    // through whatever currently holds focus, and the first-run gate is an
    // `adw::Dialog` presented into the window's own dialog host — so the one
    // moment a user most needs a way out is the moment that scope is least
    // certain. `app.quit` is reachable from any focus scope, and is the GNOME
    // convention besides.
    //
    // It matters more than it looks: an `adw::Dialog` with `can_close(false)`
    // also blocks the window's close request, so while the gate is up the title
    // bar button does nothing either. Between that and Quit missing from the
    // primary menu, a signed-out app had no visible way to exit at all.
    let app = relm4::main_application();
    let quit = gtk::gio::SimpleAction::new("quit", None);
    quit.connect_activate(|_, _| relm4::main_application().quit());
    app.add_action(&quit);

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
    group.add_action(RelmAction::<VolumeUpAction>::new_stateless(move |_| {
        s.input(AppMsg::VolumeUp)
    }));
    let s = sender.clone();
    group.add_action(RelmAction::<VolumeDownAction>::new_stateless(move |_| {
        s.input(AppMsg::VolumeDown)
    }));
    let s = sender.clone();
    group.add_action(RelmAction::<ToggleQueueAction>::new_stateless(move |_| {
        s.input(AppMsg::ToggleQueue)
    }));
    let s = sender.clone();
    group.add_action(RelmAction::<ToggleSidebarAction>::new_stateless(
        move |_| s.input(AppMsg::ToggleSidebar),
    ));

    app.set_accelerators_for_action::<PreferencesAction>(&["<Control>comma"]);
    app.set_accelerators_for_action::<ShortcutsAction>(&["<Control>question"]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accelerators_for_action::<PlayPauseAction>(&["<Control>k"]);
    app.set_accelerators_for_action::<NextAction>(&["<Control>Right"]);
    app.set_accelerators_for_action::<PreviousAction>(&["<Control>Left"]);
    app.set_accelerators_for_action::<VolumeUpAction>(&["<Control>Up"]);
    app.set_accelerators_for_action::<VolumeDownAction>(&["<Control>Down"]);
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
        ("Play or pause", "<Control>k"),
        ("Next track", "<Control>Right"),
        ("Previous track", "<Control>Left"),
        ("Volume up", "<Control>Up"),
        ("Volume down", "<Control>Down"),
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
    /// The first-run gate.
    ///
    /// A modal that cannot be dismissed, rather than a page behind a usable
    /// window. Signed out, every control in the app is a control that cannot
    /// work: the sidebar sections fire library loads, the search box queries a
    /// catalog that answers 403, and the transport talks to a player with no
    /// session. Leaving them reachable meant a 403 per second against Apple —
    /// blocking is not a nicety here, it is the correct behaviour.
    ///
    /// Dismissed from `update` the moment the sidecar reports an authorized
    /// session, never by the user.
    pub(super) fn present_onboarding(
        &self,
        sender: &ComponentSender<Self>,
        parent: &adw::ApplicationWindow,
    ) -> adw::Dialog {
        let page = adw::StatusPage::builder()
            .icon_name(crate::APP_ID)
            .title("Welcome to Tonearm")
            .description(
                "Tonearm plays your Apple Music library natively on GNOME. \
                 It needs an active Apple Music subscription.",
            )
            .build();

        let button = gtk::Button::builder()
            .label("Sign In to Apple Music")
            .halign(gtk::Align::Center)
            .css_classes(["suggested-action", "pill"])
            .build();
        {
            let sender = sender.clone();
            button.connect_clicked(move |_| sender.input(AppMsg::SignIn));
        }

        // Said before the button is pressed, not after. A browser window
        // opening out of a native app is alarming when it is a surprise, and
        // this is the one moment Tonearm cannot hide the web engine.
        let note = gtk::Label::builder()
            .label(
                "Apple's own sign-in page opens in a separate window, including \
                 two-factor if your account uses it. It closes for good once you're in.",
            )
            .justify(gtk::Justification::Center)
            .wrap(true)
            .max_width_chars(46)
            .css_classes(["caption", "dim-label"])
            .build();

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .halign(gtk::Align::Center)
            .spacing(18)
            .build();
        column.append(&button);
        column.append(&note);
        page.set_child(Some(&column));

        // A way out of the app, on the one screen that otherwise has none:
        // `can_close(false)` stops the window's own close button too, so
        // without this the gate is a dead end for anyone who does not want to
        // sign in right now.
        //
        // In the corner rather than under the call to action. Below Sign In it
        // sat in the reading order as if it were the second step, and it is not
        // a step at all — it is the way out. Flat, and not destructive:
        // quitting is ordinary, and red would imply it discards something.
        let quit = gtk::Button::builder()
            .label("Quit")
            .css_classes(["flat"])
            .build();
        quit.connect_clicked(|_| relm4::main_application().quit());

        // The bar exists only to hold that button — the dialog cannot be
        // closed, so there are no window controls to show and no title to
        // repeat above the status page's own.
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .css_classes(["flat"])
            .build();
        header.set_title_widget(Some(&gtk::Label::new(None)));
        header.pack_end(&quit);

        let view = adw::ToolbarView::builder().content(&page).build();
        view.add_top_bar(&header);

        // **Width only.** A fixed `content_height` was what made this scroll:
        // `adw::StatusPage` puts its content in a scrolled window, so any
        // height smaller than the natural one produces a scrollbar — and 420
        // was smaller, on a dialog with a heading, two short paragraphs and a
        // button. Left unset, the dialog takes the height its content asks for
        // and there is nothing to scroll.
        let dialog = adw::Dialog::builder()
            .child(&view)
            .content_width(480)
            // No escape, no click-outside: there is nothing behind this worth
            // reaching until there is a session.
            .can_close(false)
            .build();
        dialog.present(Some(parent));
        dialog
    }

    /// Ask before signing out.
    ///
    /// Destructive and not obviously reversible from the user's side: it drops
    /// Apple's session, so getting back in means the login window again, with
    /// whatever two-factor prompt that involves. Worth a question.
    pub(super) fn confirm_sign_out(
        &self,
        sender: &ComponentSender<Self>,
        parent: &adw::ApplicationWindow,
    ) {
        let dialog = adw::AlertDialog::new(
            Some("Sign out of Apple Music?"),
            Some(
                "Tonearm will forget this session. Signing back in opens Apple's \
                 login window again.",
            ),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("sign-out", "Sign Out");
        dialog.set_response_appearance("sign-out", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let sender = sender.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "sign-out" {
                sender.input(AppMsg::SignOutConfirmed);
            }
        });
        dialog.present(Some(parent));
    }

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
