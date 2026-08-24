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

use relm4::adw;
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
    /// Whether that cover is painted at all — the Preferences toggle (#145).
    ///
    /// Here rather than gated at the call site because this module already owns
    /// *what is on screen*. `SHOWN_ART` is what the theme-flip handler repaints
    /// from, so a caller-side gate would put the cover back on the next
    /// light/dark flip with nothing having asked for it.
    static BACKDROP_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Install the providers. Called once, before the window is shown.
pub fn init(accent: Accent, backdrop: bool) {
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
    BACKDROP_ON.with(|c| c.set(backdrop));
    set_accent(accent);

    // **Repaint the backdrop when the theme flips.** The veil's alphas differ
    // per theme (see `Veil`), so a cover painted while dark stays painted with
    // dark's numbers until the next track — which on a paused player is never.
    // Switching to light then left the drawer wearing an 0.86 white veil, which
    // is the washed-out state this pair of numbers exists to avoid.
    //
    // `SHOWN_ART` already holds what is on screen, so repainting is just
    // re-emitting the same image through the current theme's numbers.
    adw::StyleManager::default().connect_dark_notify(|_| {
        let shown = SHOWN_ART.with(|c| c.borrow().clone());
        paint_backdrop(shown.as_deref().map(image_of));
    });
}

/// How the cover is laid out on each surface. Static, so it lives here rather
/// than in the provider that is replaced per track.
///
/// The bar takes `cover`: it is a wide strip, and a square scaled to its width
/// already overflows its height many times over, so it needs nothing more.
///
/// Both are centred explicitly: that used to come from the drift's keyframes,
/// and the CSS default is `0% 0%` — the top-left corner.
///
/// The drawer asks for 150% because it is closer to square and would otherwise
/// have none — but **on both axes, not one**. A single `150%` sets the width
/// and lets the height follow the aspect ratio, so a square covered exactly
/// 1.5w × 1.5w and stopped reaching the moment the drawer was taller than that.
/// A narrow window makes it exactly that, and the cover sat in a band with bare
/// surface above and below it.
///
/// Naming both stretches the square rather than fitting it, which would matter
/// on a photograph and cannot be seen on a 256px image blurred and then put
/// behind a veil.
const COVER_LAYOUT: &str = ".np-bar { background-size: cover, cover; }
         .np-sheet { background-size: cover, 150% 150%; }
         .np-bar, .np-sheet { background-position: center, center; }";

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

         /* Padding rather than a margin: the backdrop is a background, and a
            margin would leave an untinted frame around it.

            On the *row*, not on `.np-bar` itself, so the progress line above
            it can reach both edges. */
         .np-row {{ padding: 10px; }}

         /* The bar's progress line. Thin, square, and edge to edge — it reads
            as the bar filling up rather than as a widget sitting on it, which
            is the whole reason it stopped being a scale.

            GTK draws a progressbar as trough > progress, and Adwaita gives
            both a radius and the trough a margin that would inset the line
            from the ends. Every one of these is undoing that. */
         .np-progress,
         .np-progress > trough,
         .np-progress > trough > progress {{
             min-height: 3px;
             border-radius: 0;
             margin: 0;
             padding: 0;
         }}
         .np-progress > trough {{ background-color: alpha(currentColor, 0.13); }}

         {COVER_LAYOUT}

         /* A scroller wrapping a `view` widget paints the `view` background,
            which is a shade darker than the window. That was invisible while
            this one was clamped to 800px and centred — it read as the list's
            own surface — and became a dark band across the whole window the
            moment the scroller spanned it, which is what moving the clamp
            *inside* (as `AdwClampScrollable`) did.

            Cleared on the scroller and on the viewport GTK may put inside it,
            because which of the two paints depends on whether the child
            implements `GtkScrollable`. The grid below needs the same thing for
            the same reason. */
         .plain-scroller,
         .plain-scroller > viewport {{
             background: none;
         }}

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

         /* Dragging a row in a reorderable list — the queue, and the sidebar's
            pinned playlists. The argument CLAUDE.md asks for: libadwaita has no
            reorderable list, and a drop has to say where it will land before it
            happens. An inset shadow rather than a border, because a border would
            change the row's height and shove the list down 2px as the pointer
            crossed it.

            Named for what they do rather than for the queue: two lists reorder
            now, and a second copy of these three rules would be two places to
            change the colour. */
         .drop-above {{ box-shadow: inset 0 2px 0 var(--accent-bg-color); }}
         .drop-below {{ box-shadow: inset 0 -2px 0 var(--accent-bg-color); }}
         .dragging {{ opacity: 0.35; }}

         /* The volume panel's shape. GTK's `.osd` brings the colours — the
            translucent slab the Shell uses — and stops there, so the corners
            are square and it reads as a dialog rather than a readout.

            A pill is what every GNOME surface that shows a level uses, and
            there is no libadwaita widget for an OSD panel to inherit it from,
            which is the argument this rule needs. Two properties, no colours:
            whatever `.osd` resolves to in either theme still applies. */
         .volume-osd {{
             border-radius: 9999px;
             padding: 10px 18px;
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

    if !BACKDROP_ON.with(std::cell::Cell::get) {
        // Nothing to paint, and no fade to run for a third of a second at 60fps
        // to arrive at the same nothing. The provider is already empty — either
        // it was never loaded, or `set_backdrop_enabled(false)` cleared it.
        //
        // `SHOWN_ART` is current above this, deliberately: it is what turning
        // the preference back on paints from, and skipping it here would land
        // on whatever was showing when it was switched off.
        return;
    }

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
/// How opaque the veil is, top and bottom, for each surface — **per theme**.
///
/// One set of numbers cannot serve both, and that is not a matter of taste.
/// The veil is `@window_bg_color`, so at 0.86 the cover contributes the
/// remaining 14% either way; but 14% of a photograph over a *dark* window reads
/// as a coloured glow, and over a near-white one it is pastel mush. Rendered
/// side by side at the real 48px-upscaled blur, light needed roughly 0.60–0.70
/// where dark wants 0.86.
///
/// The floor is set by text, not by looks. Both surfaces carry labels in the
/// theme's own foreground colour — dark text in a light theme — so a veil thin
/// enough to show a sleeve's dark half is a veil thin enough to lose the words
/// on top of it. These are the strongest values that keep the type legible on
/// the covers this was checked against, not the prettiest ones available.
struct Veil {
    bar: (f32, f32),
    sheet: (f32, f32),
}

const DARK_VEIL: Veil = Veil {
    bar: (0.78, 0.72),
    sheet: (0.86, 0.78),
};

const LIGHT_VEIL: Veil = Veil {
    bar: (0.70, 0.64),
    sheet: (0.68, 0.60),
};

fn backdrop_css(image: Option<&str>, dark: bool) -> String {
    let Some(image) = image else {
        return ".np-bar, .np-sheet { background-image: none; }".into();
    };
    let veil = if dark { DARK_VEIL } else { LIGHT_VEIL };
    let layers = |(top, bottom): (f32, f32)| {
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
        layers(veil.bar),
        layers(veil.sheet)
    )
}

/// Whether libadwaita is currently painting dark.
///
/// Asked at paint time rather than cached: the answer changes when the user
/// flips the preference *and* when the system does it under `ColorScheme::
/// Default`, and the second one never passes through our settings.
fn painting_dark() -> bool {
    adw::StyleManager::default().is_dark()
}

/// Show the cover behind the player, or stop showing it (#145). Live, from
/// Preferences — the same surfaces and the same provider, so turning it back on
/// picks up the track that is playing now rather than the one that was.
pub fn set_backdrop_enabled(on: bool) {
    BACKDROP_ON.with(|c| c.set(on));
    // Snapped, not faded. Fading a preference would say the app was thinking
    // about it; a switch should land the moment it is flipped.
    let shown = SHOWN_ART.with(|c| c.borrow().clone());
    paint_backdrop(shown.as_deref().map(image_of));
}

fn paint_backdrop(image: Option<String>) {
    // The one place every path funnels through — a track change, the fade's
    // per-frame repaint, the theme-flip handler — so the preference holds by
    // construction rather than at three call sites that could each forget it.
    let image = image.filter(|_| BACKDROP_ON.with(std::cell::Cell::get));
    let css = backdrop_css(image.as_deref(), painting_dark());
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
    fn the_drawer_backdrop_covers_both_axes() {
        // A single percentage is a *width*; the height follows the aspect
        // ratio. The cover is square, so one number meant it reached 1.5w down
        // the drawer and no further — bare surface above and below it as soon
        // as the window was narrow enough to make the drawer taller than that.
        let sheet = COVER_LAYOUT
            .lines()
            .find(|l| l.contains(".np-sheet"))
            .expect("the drawer must have a layout rule");
        let image = sheet
            .split("background-size:")
            .nth(1)
            .and_then(|s| {
                s.trim_end()
                    .trim_end_matches(&['}', ';', ' '][..])
                    .rsplit(',')
                    .next()
            })
            .expect("a background-size with a layer for the image")
            .trim()
            .to_owned();
        assert!(
            image == "cover" || image.split_whitespace().count() == 2,
            "the drawer's cover needs both axes or `cover`, got {image:?}"
        );
    }

    #[test]
    fn the_backdrop_is_static_and_centred() {
        // **The one that cost 20% of a core.** An `infinite` CSS animation
        // never lets GTK's frame clock stop — 119 fps on an idle window, #126.
        // The cross-fade between covers survives because it is a timer in
        // `set_backdrop` that ends; nothing in the stylesheet may animate.
        //
        // And the position has to be stated, because it used to fall out of
        // that animation's keyframes: the CSS default is the top-left corner.
        let css = format!(
            "{COVER_LAYOUT}{}",
            backdrop_css(Some("url(\"file:///tmp/x.png\")"), true)
        );
        assert!(
            !css.contains("animation") && !css.contains("keyframes"),
            "an animation here pins the frame clock open (#126): {css}"
        );
        assert!(css.contains("background-position"), "uncentred: {css}");
    }

    #[test]
    fn both_surfaces_get_the_cover() {
        let css = backdrop_css(Some("url(\"file:///tmp/x.png\")"), true);
        assert!(css.contains(".np-bar"), "the bar was left out: {css}");
        assert!(css.contains(".np-sheet"), "the drawer was left out: {css}");
        assert_eq!(
            css.matches("url(\"file:///tmp/x.png\")").count(),
            2,
            "each surface needs its own copy of the image"
        );
        // Clearing has to reach both too, or a stopped player keeps the last
        // cover on whichever one was forgotten.
        let cleared = backdrop_css(None, true);
        assert!(cleared.contains(".np-bar") && cleared.contains(".np-sheet"));
    }

    #[test]
    fn the_bar_shows_more_of_the_record_than_the_drawer() {
        // Small type on a thin strip is the harder read, but a veil heavy
        // enough for the drawer left the bar a flat grey — which is the bug
        // this pair of numbers exists to prevent regressing.
        let css = backdrop_css(Some("url(\"a\")"), true);
        let bar = &css[css.find(".np-bar").unwrap()..css.find(".np-sheet").unwrap()];
        assert!(bar.contains("0.78"), "bar scrim changed: {bar}");
        assert!(
            !bar.contains("0.86"),
            "bar is using the drawer's veil: {bar}"
        );
    }

    #[test]
    fn a_light_theme_gets_a_thinner_veil_than_a_dark_one() {
        // The bug this fixes: one set of numbers for both. The veil is
        // `@window_bg_color`, so 0.86 leaves the cover 14% either way — and 14%
        // of a photograph reads as a coloured glow over a dark window and as
        // pastel mush over a near-white one.
        let dark = backdrop_css(Some("url(\"a\")"), true);
        let light = backdrop_css(Some("url(\"a\")"), false);
        assert_ne!(dark, light, "both themes got the same veil");

        for (top, bottom) in [DARK_VEIL.bar, DARK_VEIL.sheet] {
            assert!(top > bottom, "the veil must thin downwards");
        }
        for (top, bottom) in [LIGHT_VEIL.bar, LIGHT_VEIL.sheet] {
            assert!(top > bottom, "the veil must thin downwards");
        }
        assert!(
            LIGHT_VEIL.sheet.0 < DARK_VEIL.sheet.0 && LIGHT_VEIL.bar.0 < DARK_VEIL.bar.0,
            "light must let more of the cover through, not less"
        );
    }

    #[test]
    fn a_veil_never_gets_thin_enough_to_lose_the_words() {
        // The floor is set by text, not by looks: both surfaces carry labels in
        // the theme's own foreground colour, and a veil thin enough to show a
        // sleeve's dark half is thin enough to lose them. Rendered against real
        // covers, below about 0.5 the type stops being readable.
        for (top, bottom) in [
            DARK_VEIL.bar,
            DARK_VEIL.sheet,
            LIGHT_VEIL.bar,
            LIGHT_VEIL.sheet,
        ] {
            assert!(bottom >= 0.5, "veil too thin for text: {bottom}");
            assert!(top <= 0.9, "veil so heavy the cover is invisible: {top}");
        }
    }

    #[test]
    fn a_cover_is_a_file_url() {
        let css = image_of(std::path::Path::new("/tmp/a-b.backdrop.png"));
        assert_eq!(css, "url(\"file:///tmp/a-b.backdrop.png\")");
    }
}
