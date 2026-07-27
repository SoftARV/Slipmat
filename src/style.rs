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
    /// The drawer's backdrop. A third provider rather than a second rule in
    /// `TINT`, for the reason the other two are separate: `TINT` is rewritten
    /// on every frame of the cross-fade, and re-declaring a `url()` twenty
    /// times a track is asking GTK to reconsider an image that has not
    /// changed. This one is replaced once, when the cover does.
    static BACKDROP: gtk::CssProvider = gtk::CssProvider::new();
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
    BACKDROP.with(|p| {
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
         .tile-grid {{
             padding: 12px;
             /* A GridView paints the `view` background, which is a shade
                darker than the window. The results list next door carries
                `navigation-sidebar` and is transparent, so the two sections
                did not match. */
             background: none;
         }}

         /* A button that has to be exactly as big as the 16px spinner it
            swaps with. GTK's own button metrics are built for a hit target,
            not for sitting inside a sidebar row, and the default padding
            alone made every library row taller. */
         .row-action {{
             padding: 0;
             min-width: 16px;
             min-height: 16px;
         }}

         /* The empty artwork slot, drawn as a case rather than left as a
            floating icon: with nothing playing the bar should still read as
            having a place the cover goes. The left edge is a touch lighter,
            which is enough to suggest a spine. */
         .np-cover-empty {{
             border-radius: 6px;
             background-image: linear-gradient(
                 to right,
                 alpha(currentColor, 0.16) 0%,
                 alpha(currentColor, 0.16) 3px,
                 alpha(currentColor, 0.07) 3px,
                 alpha(currentColor, 0.10) 100%
             );
             box-shadow: inset 0 0 0 1px alpha(currentColor, 0.12);
             color: alpha(currentColor, 0.45);
         }}

         /* The drawer's backdrop drifts, slowly enough that you never catch
            it moving — you only notice that it is not a still image. Kept
            here, in the provider parsed once, rather than beside the `url()`
            that changes per track: a restarted animation on every track
            change would be a jump, which is the opposite of the point.

            Only the second layer moves. The first is the scrim, and a scrim
            that slid would stop being one. */
         @keyframes np-drift {{
             from {{ background-position: center, 34% 38%; }}
             to   {{ background-position: center, 66% 62%; }}
         }}
         .np-sheet {{
             animation: np-drift 54s ease-in-out infinite alternate;
         }}

         /* Two grey bars where the title and artist go. Static, not pulsing:
            a pulsing skeleton would say something is loading, and nothing is. */
         .np-skeleton {{
             border-radius: 4px;
             background-color: alpha(currentColor, 0.13);
         }}"
    );

    BASE.with(|p| p.load_from_string(&css));
}

/// How long a tint takes to cross-fade to the next track's, and how often it
/// repaints while doing so. 60fps for a third of a second — long enough to read
/// as a change of mood, short enough not to lag behind the track.
const FADE_MS: u64 = 340;
const FRAME_MS: u64 = 16;

thread_local! {
    /// The colour currently painted, so the next track has something to fade
    /// *from*.
    static SHOWN: std::cell::Cell<Option<(u8, u8, u8)>> = const { std::cell::Cell::new(None) };
    /// The fade in flight, if any.
    static FADE: std::cell::RefCell<Option<gtk::glib::SourceId>> =
        const { std::cell::RefCell::new(None) };
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
///
/// Cross-faded here rather than with a CSS `transition`. The rule lives in a
/// provider that is *replaced* on every track, and reloading a stylesheet is
/// not a state change GTK will animate between — the declaration was there and
/// did nothing. Interpolating the colour ourselves and repainting is a few
/// dozen reparses of one small rule, and it actually moves.
pub fn set_bar_tint(rgb: Option<(u8, u8, u8)>) {
    let target = rgb.map(muted);

    // Whatever was in flight is now aimed at the wrong colour.
    FADE.with(|f| {
        if let Some(id) = f.borrow_mut().take() {
            id.remove();
        }
    });

    let from = SHOWN.with(|c| c.get());
    let (Some(from), Some(to)) = (from, target) else {
        // Nothing to fade between — the first track of a session, or playback
        // stopping. Snap.
        SHOWN.with(|c| c.set(target));
        paint(target);
        return;
    };
    if from == to {
        return;
    }

    let start = std::time::Instant::now();
    let id = gtk::glib::timeout_add_local(std::time::Duration::from_millis(FRAME_MS), move || {
        let t = (start.elapsed().as_millis() as f32 / FADE_MS as f32).min(1.0);
        let now = lerp(from, to, ease(t));
        SHOWN.with(|c| c.set(Some(now)));
        paint(Some(now));

        if t >= 1.0 {
            // Cleared here, not by the canceller: removing a source that has
            // already finished logs a GLib critical.
            FADE.with(|f| *f.borrow_mut() = None);
            gtk::glib::ControlFlow::Break
        } else {
            gtk::glib::ControlFlow::Continue
        }
    });
    FADE.with(|f| *f.borrow_mut() = Some(id));
}

/// Put a cover behind the expanded player, or take it away.
///
/// Two layers, and the order matters: the artwork underneath, a scrim of the
/// window's own background over it. The scrim is why this is legible — every
/// label and icon in the drawer has a colour chosen for contrast against the
/// theme, exactly as in the bar, and a photograph behind them would be
/// guessing. Taking the scrim from `@window_bg_color` rather than from black
/// is what makes it work in the light theme too.
///
/// The image is sized past `cover` on purpose. `artwork::backdrop` hands over
/// forty-eight pixels, so it is being stretched either way; the extra gives
/// the drift somewhere to go without ever exposing an edge.
pub fn set_sheet_backdrop(path: Option<&std::path::Path>) {
    let css = match path {
        Some(path) => format!(
            ".np-sheet {{
                 background-image:
                     linear-gradient(
                         alpha(@window_bg_color, 0.86),
                         alpha(@window_bg_color, 0.78)
                     ),
                     url(\"file://{}\");
                 background-size: cover, 150%;
                 background-repeat: no-repeat, no-repeat;
             }}",
            path.display()
        ),
        None => ".np-sheet { background-image: none; }".into(),
    };
    BACKDROP.with(|p| p.load_from_string(&css));
}

/// Ease in and out, so the fade does not start and stop abruptly.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn lerp(from: (u8, u8, u8), to: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    (mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
}

/// Write the rule. Already-muted colour in, CSS out.
fn paint(rgb: Option<(u8, u8, u8)>) {
    let css = match rgb {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fade_starts_where_it_was_and_ends_where_it_is_going() {
        let from = (200, 40, 60);
        let to = (40, 80, 200);
        assert_eq!(lerp(from, to, 0.0), from);
        assert_eq!(lerp(from, to, 1.0), to);
    }

    #[test]
    fn the_middle_of_a_fade_is_between_the_two() {
        let mid = lerp((0, 0, 0), (100, 200, 40), 0.5);
        assert_eq!(mid, (50, 100, 20));
    }

    #[test]
    fn easing_is_pinned_at_both_ends() {
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        // Out of range cannot overshoot: a late frame must not produce a
        // colour outside the two it is fading between.
        assert_eq!(ease(-0.5), 0.0);
        assert_eq!(ease(2.0), 1.0);
    }

    #[test]
    fn muting_lands_a_sleeve_colour_in_the_surface_band() {
        // Both a near-black and a near-white sleeve have to come out as
        // something that can sit behind text.
        for vivid in [(10, 0, 0), (255, 250, 250), (250, 20, 140)] {
            let (r, g, b) = muted(vivid);
            let (_, sat, light) = crate::components::artwork::hsl(r, g, b);
            assert!(
                (0.50..=0.68).contains(&light),
                "lightness {light} for {vivid:?}"
            );
            assert!(sat <= 0.40, "saturation {sat} for {vivid:?}");
        }
    }
}
