// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The app's accent colour, and the two rules that need CSS.
//!
//! CLAUDE.md says not to reach for CSS where a libadwaita widget would do. This
//! is the exception it allows: an **accent colour** is not a widget. libadwaita
//! 1.6 exposes it as CSS variables (`--accent-bg-color` and friends) and there
//! is no API to set an app-specific one, so a provider is the only route.
//!
//! Two providers, deliberately:
//!
//! - a **base** one, replaced only when the accent preference changes;
//! - a **tint** one for the Now Playing bar, replaced on every track.
//!
//! Keeping them apart means recolouring the bar for a new cover does not
//! reparse the accent rules, and a bad tint can be dropped without taking the
//! accent with it.

use relm4::gtk::{self, gdk};

/// Accent choices offered in Preferences.
///
/// Apple Music red is the default: it is the app's own subject, and GNOME's
/// blue says nothing about what this program is for. Following the system
/// accent stays available for anyone who wants their desktop to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Accent {
    #[default]
    AppleRed,
    System,
    Blue,
    Purple,
    Green,
    Orange,
}

impl Accent {
    pub const ALL: [Self; 6] = [
        Self::AppleRed,
        Self::System,
        Self::Blue,
        Self::Purple,
        Self::Green,
        Self::Orange,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::AppleRed => "Apple Music Red",
            Self::System => "Follow System",
            Self::Blue => "Blue",
            Self::Purple => "Purple",
            Self::Green => "Green",
            Self::Orange => "Orange",
        }
    }

    /// What lands in the ini file.
    pub fn id(self) -> &'static str {
        match self {
            Self::AppleRed => "apple-red",
            Self::System => "system",
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Green => "green",
            Self::Orange => "orange",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "system" => Self::System,
            "blue" => Self::Blue,
            "purple" => Self::Purple,
            "green" => Self::Green,
            "orange" => Self::Orange,
            _ => Self::AppleRed,
        }
    }

    pub fn index(self) -> u32 {
        Self::ALL.iter().position(|a| *a == self).unwrap_or(0) as u32
    }

    pub fn from_index(i: u32) -> Self {
        Self::ALL.get(i as usize).copied().unwrap_or_default()
    }

    /// `(background, foreground)`. `None` means "leave libadwaita alone", which
    /// is how Follow System works — the desktop's own accent is already in
    /// those variables and the right move is to write nothing over it.
    fn colors(self) -> Option<(&'static str, &'static str)> {
        match self {
            // Apple Music's own red. Paired with white text: at this lightness
            // black would not meet contrast on a filled button.
            Self::AppleRed => Some(("#fa243c", "#ffffff")),
            Self::System => None,
            Self::Blue => Some(("#3584e4", "#ffffff")),
            Self::Purple => Some(("#9141ac", "#ffffff")),
            Self::Green => Some(("#2ec27e", "#ffffff")),
            Self::Orange => Some(("#e66100", "#ffffff")),
        }
    }
}

thread_local! {
    static BASE: gtk::CssProvider = gtk::CssProvider::new();
    static TINT: gtk::CssProvider = gtk::CssProvider::new();
}

/// Install the providers. Called once, before the window is shown.
pub fn init(accent: Accent) {
    let Some(display) = gdk::Display::default() else {
        // No display means no styling to do, and certainly nothing to fail on.
        return;
    };
    BASE.with(|p| {
        gtk::style_context_add_provider_for_display(
            &display,
            p,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        )
    });
    TINT.with(|p| {
        // Above the base: the bar's tint must win over the accent rules, and
        // it is the more specific of the two.
        gtk::style_context_add_provider_for_display(
            &display,
            p,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        )
    });
    set_accent(accent);
}

/// Apply an accent, and the handful of rules that go with it.
pub fn set_accent(accent: Accent) {
    let accent_rules = match accent.colors() {
        Some((bg, fg)) => format!(
            ":root {{
                 --accent-bg-color: {bg};
                 --accent-fg-color: {fg};
                 --accent-color: {bg};
             }}"
        ),
        None => String::new(),
    };

    // A favourite is yellow everywhere else it appears — Apple's own star, the
    // one on your phone — so it does not follow the accent. Hard-coded to
    // Adwaita's yellow rather than a `.warning`, which means something else.
    let css = format!(
        "{accent_rules}
         .favorite-star {{ color: #f5c211; }}

         /* Padding rather than a margin on the widget: the tint is a
            background, and a margin would leave an untinted frame around it. */
         .np-bar {{ padding: 10px; }}

         /* Same reason. A GridView draws its own background, so insetting it
            with a margin shows a band of the window around every grid. */
         .tile-grid {{ padding: 12px; }}

         /* A button that has to be exactly as big as the 16px spinner it
            swaps with. GTK's own button metrics are built for a hit target,
            not for sitting inside a sidebar row, and the default padding
            alone made every library row taller. */
         .row-action {{
             padding: 0;
             min-width: 16px;
             min-height: 16px;
         }}"
    );

    BASE.with(|p| p.load_from_string(&css));
}

/// Tint the Now Playing bar with a colour taken from the cover.
///
/// A **tonal scrim**: one flat, heavily desaturated wash of the sleeve's colour
/// across the whole bar, rather than a gradient fading out to one side. It is
/// what Apple Music and the better third-party players do, and it reads as *the
/// surface being tinted* rather than as a decoration laid on top of it.
///
/// Still a wash over the normal background rather than a repaint: every label,
/// icon and slider in that bar already has a colour chosen for contrast against
/// the theme, and filling the background with a colour taken from artwork
/// nobody has seen would mean recolouring all of them and guessing at contrast.
/// Muting the colour first is what keeps this true — a vivid fill at this
/// coverage would not be legible.
pub fn set_bar_tint(rgb: Option<(u8, u8, u8)>) {
    let css = match rgb.map(muted) {
        Some((r, g, b)) => format!(
            ".np-bar {{
                 /* Two stops a hair apart, not one flat colour: it is still
                    read as a single tone, but the surface has some depth to it
                    rather than looking like a painted rectangle. */
                 background-image: linear-gradient(
                     to bottom,
                     rgba({r}, {g}, {b}, 0.30),
                     rgba({r}, {g}, {b}, 0.22)
                 );
                 transition: background-image 500ms ease;
             }}"
        ),
        // Nothing playing, or a cover we could not read: back to the plain bar.
        None => ".np-bar { background-image: none; }".into(),
    };
    TINT.with(|p| p.load_from_string(&css));
}

/// Pull a sleeve's colour towards something that can be a *surface*.
///
/// The colour `artwork::dominant` returns is deliberately vivid — it answers
/// "what colour is this record". A background has the opposite job: it has to
/// stay behind text. So saturation comes right down and lightness lands in a
/// narrow band, which also stops a very dark or very bright sleeve from
/// producing a bar that is nearly black or nearly white.
fn muted((r, g, b): (u8, u8, u8)) -> (u8, u8, u8) {
    let (hue, sat, light) = crate::components::artwork::hsl(r, g, b);
    crate::components::artwork::rgb(hue, (sat * 0.5).min(0.38), light.clamp(0.52, 0.66))
}
