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
//! - a **backdrop** one carrying the cover behind the player, replaced on
//!   every track.
//!
//! Keeping them apart means a new cover does not reparse the accent rules, and
//! a bad backdrop can be dropped without taking the accent with it.

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
    /// The cover behind the bar and the drawer. Separate from `BASE` for the
    /// reason the module opens with: this one is replaced on every track, and
    /// recolouring the player should not reparse the accent rules.
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
    BACKDROP.with(|p| {
        // Above the base: the cover must win over the accent rules, and it is
        // the more specific of the two.
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

         /* Padding rather than a margin on the widget: the backdrop is a
            background, and a margin would leave an untinted frame around it. */
         .np-bar {{ padding: 10px; }}

         /* How the cover is laid out on each surface. Static, so it lives here
            rather than in the provider that is replaced per track.

            The bar takes `cover` and the drawer takes 150%. Both are square
            images, but the bar is a wide strip — scaled to its width, a square
            already overflows its height many times over, so there is room to
            drift without asking for more. The drawer is closer to square and
            would have none. */
         .np-bar {{ background-size: cover, cover; }}
         .np-sheet {{ background-size: cover, 150%; }}

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

         /* The backdrop drifts, slowly enough that you never catch
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
         .np-bar, .np-sheet {{
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

thread_local! {
    /// The cover currently behind the player, so the next one has something to
    /// fade *from*.
    static SHOWN_ART: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
    /// The fade in flight, if any.
    static ART_FADE: std::cell::RefCell<Option<gtk::glib::SourceId>> =
        const { std::cell::RefCell::new(None) };
}

/// How long a cover takes to cross-fade to the next track's, and how often it
/// repaints while doing so. 60fps for a third of a second — long enough to read
/// as a change of mood, short enough not to lag behind the track.
const FADE_MS: u64 = 340;
const FRAME_MS: u64 = 16;

/// Put a cover behind the player — the bar and the drawer both — or take it
/// away.
///
/// Two layers, and the order matters: the artwork underneath, a scrim of the
/// window's own background over it. The scrim is why this is legible — every
/// label and icon on both surfaces has a colour chosen for contrast against
/// the theme, and a photograph behind them would be guessing. Taking the scrim
/// from `@window_bg_color` rather than from black is what makes the light
/// theme work too.
///
/// This replaced a **tonal scrim**: one flat, heavily desaturated wash of the
/// sleeve's dominant colour. That existed because a bar cannot show a cover
/// legibly — except it can, once the cover is forty-eight pixels stretched
/// wide and put behind a scrim, which is the same trick that made the drawer
/// work. One surface treatment for both is worth more than the colour was.
pub fn set_backdrop(path: Option<&std::path::Path>) {
    // Whatever was in flight is now heading for the wrong cover.
    ART_FADE.with(|f| {
        if let Some(id) = f.borrow_mut().take() {
            id.remove();
        }
    });

    let from = SHOWN_ART.with(|c| c.borrow().clone());
    let to = path.map(std::path::Path::to_path_buf);
    SHOWN_ART.with(|c| *c.borrow_mut() = to.clone());

    let (Some(from), Some(to)) = (from, to) else {
        // Nothing to fade between — the first cover of a session, or playback
        // stopping. Snap, exactly as the colour does.
        paint_backdrop(path.map(image_of));
        return;
    };
    if from == to {
        return;
    }

    // Same clock as the colour, deliberately. They are two readings of one
    // cover, and finishing apart would be more noticeable than either alone.
    let start = std::time::Instant::now();
    let id = gtk::glib::timeout_add_local(std::time::Duration::from_millis(FRAME_MS), move || {
        let t = (start.elapsed().as_millis() as f32 / FADE_MS as f32).min(1.0);
        if t >= 1.0 {
            // Painted plainly at the end rather than as a 100% cross-fade, so
            // the settled state is one image and one url — and so a wrong guess
            // about which way `cross-fade` reads its percentage could only ever
            // be a fade in the wrong direction, never a wrong final frame.
            paint_backdrop(Some(image_of(&to)));
            // Cleared here, not by the canceller: removing an already-finished
            // source logs a GLib critical.
            ART_FADE.with(|f| *f.borrow_mut() = None);
            return gtk::glib::ControlFlow::Break;
        }
        let pct = (ease(t) * 100.0).round();
        paint_backdrop(Some(format!(
            "cross-fade({pct}% {}, {})",
            image_of(&to),
            image_of(&from)
        )));
        gtk::glib::ControlFlow::Continue
    });
    ART_FADE.with(|f| *f.borrow_mut() = Some(id));
}

/// One cover as a CSS image.
fn image_of(path: &std::path::Path) -> String {
    format!("url(\"file://{}\")", path.display())
}

/// The backdrop rule. A CSS *image* in, CSS out — so the caller can hand over
/// one cover or a cross-fade of two and this does not care which.
///
/// Both surfaces, in one rule each, because the two scrims are **not** the
/// same number. The drawer is a large surface with big type on it and can take
/// a heavy veil; the bar is a thin strip whose type is small, and at the
/// drawer's weight the cover behind it was invisible — a flat grey, which is
/// exactly what the tonal scrim it replaced would never have been. The bar's
/// veil is set to leave about as much of the record showing as that wash did.
///
/// Pure, and separate from the provider it is loaded into, so a test can read
/// what this actually emits. The first attempt at this feature changed the
/// doc comment and the base rules and left the selector here saying `.np-sheet`
/// alone — a mistake nothing could catch, because the CSS was valid and the
/// drawer went on working.
fn backdrop_css(image: Option<&str>) -> String {
    let Some(image) = image else {
        return ".np-bar, .np-sheet { background-image: none; }".into();
    };
    let layers = |top: f32, bottom: f32| {
        format!(
            "background-image:
                 linear-gradient(
                     alpha(@window_bg_color, {top}),
                     alpha(@window_bg_color, {bottom})
                 ),
                 {image};
             background-repeat: no-repeat, no-repeat;"
        )
    };
    format!(
        ".np-bar {{ {} }}
         .np-sheet {{ {} }}",
        layers(0.78, 0.72),
        layers(0.86, 0.78)
    )
}

fn paint_backdrop(image: Option<String>) {
    let css = backdrop_css(image.as_deref());
    BACKDROP.with(|p| p.load_from_string(&css));
}

/// Ease in and out, so the fade does not start and stop abruptly.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_is_pinned_at_both_ends() {
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        // Out of range cannot overshoot: a late frame must not ask for a
        // cross-fade percentage outside the two covers it is between.
        assert_eq!(ease(-0.5), 0.0);
        assert_eq!(ease(2.0), 1.0);
    }

    #[test]
    fn both_surfaces_get_the_cover() {
        let css = backdrop_css(Some("url(\"file:///tmp/x.png\")"));
        assert!(css.contains(".np-bar"), "the bar was left out: {css}");
        assert!(css.contains(".np-sheet"), "the drawer was left out: {css}");
        assert_eq!(
            css.matches("url(\"file:///tmp/x.png\")").count(),
            2,
            "each surface needs its own copy of the image"
        );
        // Clearing has to reach both too, or a stopped player keeps the last
        // cover on whichever one was forgotten.
        let cleared = backdrop_css(None);
        assert!(cleared.contains(".np-bar") && cleared.contains(".np-sheet"));
    }

    #[test]
    fn the_bar_shows_more_of_the_record_than_the_drawer() {
        // Small type on a thin strip is the harder read, but a veil heavy
        // enough for the drawer left the bar a flat grey — which is the bug
        // this pair of numbers exists to prevent regressing.
        let css = backdrop_css(Some("url(\"a\")"));
        let bar = &css[css.find(".np-bar").unwrap()..css.find(".np-sheet").unwrap()];
        assert!(bar.contains("0.78"), "bar scrim changed: {bar}");
        assert!(
            !bar.contains("0.86"),
            "bar is using the drawer's veil: {bar}"
        );
    }

    #[test]
    fn a_cover_is_a_file_url() {
        let css = image_of(std::path::Path::new("/tmp/a-b.backdrop.png"));
        assert_eq!(css, "url(\"file:///tmp/a-b.backdrop.png\")");
    }
}
