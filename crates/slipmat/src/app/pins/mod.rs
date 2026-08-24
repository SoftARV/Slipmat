// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Playlists pinned to the sidebar — what they mean, and what clicking one does.
//!
//! Getting to a playlist you actually listen to is two navigations every time,
//! and they are the thing most likely to be opened daily. A pin makes it one
//! click from anywhere (#133).
//!
//! **A pin is an id, not a copy.** The name is looked up against the library
//! every time the rows are built, so renaming a playlist on your phone renames
//! the row, and nothing here can go quietly out of date. The cost is that a pin
//! whose playlist has been deleted resolves to nothing — see `stale_pins`.

pub(in crate::app) mod drag;
mod menu;

use relm4::adw::prelude::*;
use relm4::gtk::glib;
use relm4::{ComponentSender, adw, gtk};

use super::View;
use super::pages::Arrival;
use super::view::{SidebarRow, sidebar_rows};
use super::{AppModel, AppMsg};
use crate::components::detail_page::PageKind;

/// What a pin says when the library has never produced its playlist.
///
/// Never the id: `p.EYWrg13SzrKxYBb` tells nobody anything. Seen briefly at
/// startup before the cache is read, and permanently for a pin whose playlist
/// was deleted elsewhere — which is what phase five prunes.
pub(super) const UNAVAILABLE: &str = "Unavailable";

/// Which pins point at playlists the library does not have.
///
/// A free function so the rule can be tested without a library or a window: it
/// is the whole of the pruning decision, and the rest is when to trust it.
pub(super) fn stale<'a>(pins: &'a [String], have: &[String]) -> Vec<&'a str> {
    pins.iter()
        .filter(|id| !have.iter().any(|known| known == *id))
        .map(String::as_str)
        .collect()
}

/// Where a drop on `row` lands, in the list's *original* coordinates.
///
/// Dropping on the lower half of a row means "after this one", which is the
/// slot one past it. Expressed as a slot rather than an index so it can name
/// the end of the list, which no index can.
pub(super) fn drop_slot(row: usize, below: bool) -> usize {
    row + usize::from(below)
}

/// Move a pin to `slot`, where `slot` is in the coordinates of the list as it
/// was *before* the move.
///
/// The subtraction is the whole subtlety: removing the dragged pin shifts
/// everything after it down one, so a slot past the original position is one
/// too far by the time the insert happens. Getting this wrong moves a row one
/// place further than the line said it would, which is the kind of bug that
/// only shows when dragging downward.
pub(super) fn move_pin(pins: &mut Vec<String>, from: usize, slot: usize) {
    if from >= pins.len() {
        return;
    }
    let pin = pins.remove(from);
    let at = if slot > from { slot - 1 } else { slot };
    pins.insert(at.min(pins.len()), pin);
}

impl AppModel {
    /// What a sidebar row selection means.
    ///
    /// The sidebar reports a *position*; this is the only place that turns one
    /// back into an action, because a pin and a section sit in the same list and
    /// do entirely different things — one changes what the pane shows, the other
    /// pushes a page on top of it.
    pub(super) fn sidebar_row_chosen(&mut self, index: i32, sender: &ComponentSender<Self>) {
        // A position with no row is not an error worth reporting: `ListBox`
        // reports a selection while rows are being rebuilt underneath it.
        let Some(row) = usize::try_from(index)
            .ok()
            .and_then(|i| self.sidebar_rows.get(i))
            .cloned()
        else {
            return;
        };

        self.selected_row = Some(row.clone());
        match row {
            SidebarRow::Section(view) => {
                // **Popped here, not in `SetView`.** That arm returns early when
                // the section has not changed, which is right for its other
                // callers and wrong for this one: choosing "All" while a pinned
                // playlist is open changes no section, so nothing popped and the
                // grid stayed hidden behind the page until you pressed Back.
                // Choosing a section always means "show me that", pushed page or
                // not.
                self.pop_to_results();
                sender.input(AppMsg::SetView(view));
            }
            // A destination, not a drill-down: replaces whatever was open rather
            // than stacking on top of it, and draws no back button.
            SidebarRow::Pinned(id) => {
                // **Where you are is Playlists, even though the row is a pin.**
                // Only sections are persisted, and a pin is not one — so
                // closing the app on a pinned playlist used to reopen on
                // whatever section you were in before you clicked it.
                //
                // Recording the group rather than the pin is deliberate. Pushed
                // pages are not restored anywhere in this app — close on an
                // album and you reopen on Albums — and restoring one here would
                // need the page opened before tokens exist, plus something to
                // say what to do when the playlist was deleted on another
                // device. All is the honest answer, and it is the same answer
                // albums already give.
                self.settings.section = crate::settings::Section::from(View::Playlists);
                self.settings.save();

                self.pop_to_results();
                self.open_page(
                    PageKind::LibraryPlaylist(id),
                    sender,
                    Arrival::FromTheSidebar,
                );
            }
            // Activation opens the picker, not selection — the row is not
            // selectable, so this arm is only reachable while the rows and the
            // widgets disagree. Opening the picker from both would open it
            // twice, and it would have to be closed twice.
            SidebarRow::PinButton => {}
        }
    }

    /// Put the right name on every pinned row.
    ///
    /// **Labels, not rows.** The sidebar is built before the library cache is
    /// read, so pins are drawn nameless and have to be filled in afterwards —
    /// and again whenever the library reloads, so renaming a playlist elsewhere
    /// renames the row. Rebuilding the rows would do it too, and would clear the
    /// sidebar's selection on every library load: the failure 285b542 removed.
    pub(super) fn refresh_pin_names(&self) {
        for (id, label) in &self.pin_labels {
            label.set_label(self.pinned_name(id).unwrap_or(UNAVAILABLE));
        }
    }

    /// The picker: every library playlist, with the pinned ones ticked.
    ///
    /// **One place to both pin and unpin.** A dialog that only added would leave
    /// somebody hunting for how to remove one, and the row it would have to live
    /// on is the pin itself — which is a destination, not a menu.
    ///
    /// Toggling acts immediately rather than on a Done button: there is nothing
    /// to validate, nothing to cancel, and the sidebar behind the dialog shows
    /// the result as you go.
    pub(super) fn show_pin_picker(
        &mut self,
        sender: &ComponentSender<Self>,
        parent: &adw::ApplicationWindow,
    ) {
        let group = adw::PreferencesGroup::new();

        if self.playlists.is_empty() {
            // Not an error, and not a dialog worth opening empty either — but a
            // toast would vanish behind the question it failed to answer.
            group.set_description(Some(
                "Your library has no playlists yet, or it has not finished loading.",
            ));
        }

        let mut switches = Vec::with_capacity(self.playlists.len());
        for playlist in &self.playlists {
            let row = adw::SwitchRow::new();
            row.set_title(&glib::markup_escape_text(&playlist.name));
            row.set_active(self.settings.pinned_playlists.contains(&playlist.id));
            group.add(&row);
            switches.push((playlist.id.clone(), row));
        }
        let switches = std::rc::Rc::new(switches);

        // One action, two meanings, and the label is the only thing saying
        // which: everything pinned means the useful move is to clear them.
        let toggle_all = gtk::Button::new();
        toggle_all.set_visible(!switches.is_empty());
        let relabel: std::rc::Rc<dyn Fn()> = {
            let switches = switches.clone();
            let button = toggle_all.clone();
            std::rc::Rc::new(move || {
                let all_on = switches.iter().all(|(_, row)| row.is_active());
                button.set_label(if all_on { "Unpin All" } else { "Pin All" });
            })
        };
        relabel();

        for (id, row) in switches.iter() {
            let id = id.clone();
            let sender = sender.clone();
            let relabel = relabel.clone();
            row.connect_active_notify(move |row| {
                sender.input(AppMsg::SetPinned {
                    id: id.clone(),
                    pinned: row.is_active(),
                });
                relabel();
            });
        }

        {
            let switches = switches.clone();
            let relabel = relabel.clone();
            let sender = sender.clone();
            toggle_all.connect_clicked(move |_| {
                let target = !switches.iter().all(|(_, row)| row.is_active());
                // **The whole list in one message, then the switches.** Setting
                // the switches first would fire a `SetPinned` each, and every
                // one of those rebuilds the sidebar — eight rebuilds to say one
                // thing. Sent first, each switch's echo finds the pin already in
                // the state it is asking for and `set_pinned` returns early.
                sender.input(AppMsg::SetAllPinned(target));
                for (_, row) in switches.iter() {
                    row.set_active(target);
                }
                relabel();
            });
        }

        let page = adw::PreferencesPage::new();
        page.add(&group);

        let header = adw::HeaderBar::new();
        header.pack_start(&toggle_all);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&page));

        let dialog = adw::Dialog::builder()
            .title("Pin Playlists")
            .content_width(420)
            .content_height(520)
            .child(&toolbar)
            .build();
        dialog.present(Some(parent));
    }

    /// Redraw the sidebar's rows if the pins changed.
    ///
    /// Runs on the way out of every message, like `sync_animated`, and does
    /// nothing unless something actually changed — a rebuild throws away seven
    /// widgets and the selection with them.
    pub(super) fn sync_pins(
        &mut self,
        widgets: &<Self as relm4::Component>::Widgets,
        sender: &ComponentSender<Self>,
    ) {
        if !self.pins_dirty {
            return;
        }
        self.pins_dirty = false;
        super::wiring::rebuild_sidebar(self, widgets, sender);
    }

    /// A click on a sidebar row, as opposed to a selection.
    ///
    /// Only the pin button acts here — every other row has already done its work
    /// through `SidebarRowChosen`. This is also where an overlay sidebar gets
    /// out of the way, which is the end of what an overlay sidebar is for.
    pub(super) fn sidebar_row_activated(&mut self, index: i32, sender: &ComponentSender<Self>) {
        let row = usize::try_from(index)
            .ok()
            .and_then(|i| self.sidebar_rows.get(i));

        if matches!(row, Some(SidebarRow::PinButton)) {
            sender.input(AppMsg::ShowPinPicker);
            // The picker is what you asked for; closing the sidebar under it
            // would be answering a different question.
            return;
        }
        if self.sidebar_collapsed {
            self.show_sidebar = false;
        }
    }

    /// Drop pins whose playlist the library no longer has.
    ///
    /// **Only from a load that actually succeeded**, which is the entire
    /// difficulty. A pin pointing at nothing and a library that has not arrived
    /// look identical from here, and the difference matters enormously: pruning
    /// on a 403 or a half-finished fetch would delete somebody's sidebar because
    /// the network hiccupped. The caller is the `Ok` arm of the playlists load,
    /// and nowhere else.
    ///
    /// The toast counts rather than names. A pin is an id and the name was only
    /// ever borrowed from the library — which is exactly what just stopped
    /// having it — so there is no honest name left to say.
    pub(super) fn prune_stale_pins(&mut self, sender: &ComponentSender<Self>) {
        // **An empty library prunes nothing, even on a success.** A successful
        // response carrying zero playlists is far more likely to be Apple having
        // an odd moment than somebody having deleted every playlist they own —
        // and the two are indistinguishable from here. Guessing wrong in one
        // direction leaves stale rows saying "Unavailable", which is visible and
        // fixable by hand; guessing wrong in the other silently deletes a
        // sidebar somebody arranged, which is neither.
        if self.playlists.is_empty() {
            return;
        }

        let have: Vec<String> = self.playlists.iter().map(|p| p.id.clone()).collect();
        let gone: Vec<String> = stale(&self.settings.pinned_playlists, &have)
            .into_iter()
            .map(str::to_owned)
            .collect();
        if gone.is_empty() {
            return;
        }

        tracing::info!(
            count = gone.len(),
            "dropping pins the library no longer has"
        );
        self.settings
            .pinned_playlists
            .retain(|id| !gone.contains(id));
        self.settings.save();

        // Looking at one that just went is the unpin case, and it ends the same
        // way — see `set_pinned`.
        if let Some(SidebarRow::Pinned(open)) = &self.selected_row
            && gone.contains(open)
        {
            self.pop_to_results();
            self.selected_row = Some(SidebarRow::Section(View::Playlists));
            sender.input(AppMsg::SetView(View::Playlists));
        }

        self.sidebar_rows = sidebar_rows(&self.settings.pinned_playlists);
        self.pins_dirty = true;
        // Reported, not commanded. "Unpinned a playlist…" is the imperative and
        // reads as an instruction to the person who did not do it — the app did,
        // and the toast exists to say so rather than to ask for anything.
        self.toast(&if gone.len() == 1 {
            "A pinned playlist was removed — it is no longer in your library".to_owned()
        } else {
            format!(
                "{} pinned playlists were removed — they are no longer in your library",
                gone.len()
            )
        });
    }

    /// Reorder the pins after a drag.
    ///
    /// Pin order *is* what the sidebar draws, so this is the whole of the
    /// feature — there is no separate order to keep in step.
    pub(super) fn move_pinned(&mut self, from: usize, slot: usize) {
        let before = self.settings.pinned_playlists.clone();
        move_pin(&mut self.settings.pinned_playlists, from, slot);
        if self.settings.pinned_playlists == before {
            // Dropping a row on its own edge is a drag that meant nothing, and
            // rewriting settings for it is a save nobody asked for.
            return;
        }
        self.settings.save();
        self.sidebar_rows = sidebar_rows(&self.settings.pinned_playlists);
        self.pins_dirty = true;
    }

    /// Pin or unpin every library playlist at once.
    ///
    /// Pinning keeps the pins already there in the order they were put there and
    /// appends the rest, so "Pin All" does not reshuffle a sidebar somebody
    /// arranged — it only fills in what was missing.
    pub(super) fn set_all_pinned(&mut self, pinned: bool, sender: &ComponentSender<Self>) {
        if pinned {
            for playlist in &self.playlists {
                if !self.settings.pinned_playlists.contains(&playlist.id) {
                    self.settings.pinned_playlists.push(playlist.id.clone());
                }
            }
        } else {
            self.settings.pinned_playlists.clear();
        }
        self.settings.save();

        // Unpinning everything takes with it whatever pin you were looking at,
        // for the same reason unpinning one does. See `set_pinned`.
        if !pinned && matches!(self.selected_row, Some(SidebarRow::Pinned(_))) {
            self.pop_to_results();
            self.selected_row = Some(SidebarRow::Section(View::Playlists));
            sender.input(AppMsg::SetView(View::Playlists));
        }

        self.sidebar_rows = sidebar_rows(&self.settings.pinned_playlists);
        self.pins_dirty = true;
    }

    /// Pin or unpin one playlist.
    ///
    /// Pins are appended, never sorted: pin order is what the sidebar draws, and
    /// somebody who pinned three things in an order meant that order.
    pub(super) fn set_pinned(&mut self, id: &str, pinned: bool, sender: &ComponentSender<Self>) {
        let already = self.settings.pinned_playlists.iter().any(|p| p == id);
        if already == pinned {
            return;
        }
        if pinned {
            self.settings.pinned_playlists.push(id.to_owned());
        } else {
            self.settings.pinned_playlists.retain(|p| p != id);
        }
        self.settings.save();

        // **Unpinning what you are looking at closes it, and lands on All.**
        // The row is the only way back to that page, so leaving it open strands
        // you on a destination that no longer exists.
        //
        // All rather than the section you were in before: a pin belongs to the
        // Playlists group, so losing one leaves you among the playlists. Going
        // back to Artists because that is where you happened to be twenty
        // minutes ago answers a question nobody asked.
        if !pinned && self.selected_row.as_ref() == Some(&SidebarRow::Pinned(id.to_owned())) {
            self.pop_to_results();
            // Both, and in this order: the row is what `rebuild_sidebar` looks
            // for on the way out, and the view is what the pane shows. Setting
            // only one leaves the sidebar and the content disagreeing.
            self.selected_row = Some(SidebarRow::Section(View::Playlists));
            sender.input(AppMsg::SetView(View::Playlists));
        }

        self.sidebar_rows = sidebar_rows(&self.settings.pinned_playlists);
        // The widgets are `wiring`'s and cannot be reached from here — the
        // rebuild happens on the way out, in `sync_pins`.
        self.pins_dirty = true;
    }

    /// The name to draw for a pin, or `None` if the library has never heard of
    /// it.
    ///
    /// `None` covers two different situations that look identical here and must
    /// not be conflated: a playlist deleted elsewhere, and a library that has
    /// not finished loading. Only the first is a stale pin, and only a *loaded*
    /// library can tell them apart — which is why nothing is pruned from here.
    pub(super) fn pinned_name(&self, id: &str) -> Option<&str> {
        self.playlists
            .iter()
            .find(|playlist| playlist.id == id)
            .map(|playlist| playlist.name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pins() -> Vec<String> {
        ["a", "b", "c"].iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_drop_lands_between_the_two_rows_the_line_was_drawn_between() {
        // The lower half of row 1 is the slot after it, which is 2 — the same
        // slot as the upper half of row 2. One line, one answer.
        assert_eq!(drop_slot(1, true), 2);
        assert_eq!(drop_slot(2, false), 2);
        assert_eq!(drop_slot(0, false), 0);
    }

    #[test]
    fn dragging_downward_lands_where_the_line_was() {
        // The case the shift correction exists for: without it `a` would end up
        // one place further than the line promised.
        let mut p = pins();
        move_pin(&mut p, 0, 2);
        assert_eq!(p, ["b", "a", "c"]);
    }

    #[test]
    fn dragging_upward_lands_where_the_line_was() {
        let mut p = pins();
        move_pin(&mut p, 2, 0);
        assert_eq!(p, ["c", "a", "b"]);
    }

    #[test]
    fn dropping_a_row_on_itself_changes_nothing() {
        // Both edges of the dragged row are the same place it already is, and a
        // reorder that rewrites settings for no change is a save nobody asked
        // for.
        for slot in [1, 2] {
            let mut p = pins();
            move_pin(&mut p, 1, slot);
            assert_eq!(p, ["a", "b", "c"], "slot {slot} moved something");
        }
    }

    #[test]
    fn a_drop_past_the_end_lands_at_the_end() {
        let mut p = pins();
        move_pin(&mut p, 0, 3);
        assert_eq!(p, ["b", "c", "a"]);
    }

    #[test]
    fn a_pin_the_library_still_has_is_left_alone() {
        let pins = pins();
        assert!(stale(&pins, &pins).is_empty());
        // Order is irrelevant to the question — a pin is present or it is not.
        let shuffled: Vec<String> = ["c", "a", "b"].iter().map(|s| (*s).to_owned()).collect();
        assert!(stale(&pins, &shuffled).is_empty());
    }

    #[test]
    fn a_pin_the_library_lost_is_named() {
        let have: Vec<String> = ["a", "c"].iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(stale(&pins(), &have), ["b"]);
    }

    #[test]
    fn extra_playlists_are_not_pins() {
        // The library having more than you pinned is the normal case, not a
        // signal about anything.
        let have: Vec<String> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        assert!(stale(&pins(), &have).is_empty());
    }

    #[test]
    fn a_position_that_no_longer_exists_is_ignored() {
        // The list can change under a drag — the picker is one dialog away.
        let mut p = pins();
        move_pin(&mut p, 9, 0);
        assert_eq!(p, ["a", "b", "c"]);
    }
}
