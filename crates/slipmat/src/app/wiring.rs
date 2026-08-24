// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! What `init` connects once the widgets exist.
//!
//! Split out of `mod.rs` (#78), which CLAUDE.md says should hold three things:
//! the model, the messages, and the `Component` impl carrying `view!` and the
//! reducer. None of this is any of those. It is imperative wiring that the
//! `view!` macro has no form for — stateful menu actions, a breakpoint, a
//! scroll handler, a property the macro cannot set on a `#[local_ref]`.
//!
//! Each piece is a named function rather than one `wire everything` block, so
//! the reason each exists is attached to it. Several of them are load-bearing
//! in ways that are not obvious and cost real debugging when they were absent —
//! `library_list` in particular restores two properties whose loss looked like
//! two unrelated bugs.

use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::{ComponentSender, RelmWidgetExt};

use super::view::SidebarRow;
use super::{AppModel, AppMsg, CatalogFilter, SortBy, View};

/// The widgets `view!` generates, spelled without naming the generated type.
type Widgets = <AppModel as relm4::Component>::Widgets;

/// Connect everything that cannot be expressed in `view!`.
///
/// Order matters in one place only: `sort_menu` installs the action group the
/// button's popover reads, so it must run before anything asks the button to
/// redraw itself.
pub(super) fn connect(
    model: &mut AppModel,
    widgets: &Widgets,
    root: &adw::ApplicationWindow,
    sender: &ComponentSender<AppModel>,
) {
    sidebar_rows(model, widgets, sender);
    sidebar_selection(model, widgets, sender);
    sidebar_headers(
        widgets,
        super::view::section_index(&model.sidebar_rows, View::Playlists).unwrap_or_default(),
    );
    sort_menu(model, widgets, sender);
    catalog_filter_menu(model, widgets, sender);
    open_on_last_section(model, widgets);
    catalog_pagination(widgets, sender);
    window_breakpoints(root, widgets, sender);
    type_to_search(root, sender);
    library_list_properties(model, widgets);
    bottom_bar_inset(widgets);
    // The volume panel sits under the header for the same reason the content
    // clears the bar: that height is not a constant.
    model.volume_osd.sit_below_the_header(&widgets.content_bars);
}

/// Build the sidebar's five rows from [`View::SIDEBAR`].
///
/// They were five near-identical `ListBoxRow`s in `view!` — 126 lines saying
/// one thing five times, each carrying a comment repeating its own index.
/// A loop cannot get the order wrong, and `view.rs` now holds the order and the
/// rows it belongs to in the same array (#100).
///
/// The spinners are the reason this needs anything beyond a loop. `view!` gave
/// them `#[watch]`; a widget built here gets none, so they are kept and driven
/// by `sync_section_spinners`.
fn sidebar_rows(model: &mut AppModel, widgets: &Widgets, sender: &ComponentSender<AppModel>) {
    // A pin's position among the *pins*, which is what a reorder moves — not its
    // position among the rows, which counts the five sections above it.
    let mut pin_index = 0usize;
    for entry in model.sidebar_rows.clone() {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        content.set_margin_all(8);

        let (icon, text) = match &entry {
            SidebarRow::Section(view) => {
                let row = section_row(*view);
                (row.icon, row.label.to_owned())
            }
            // A pin with no name is a pin whose playlist the library has not
            // produced — either still loading, or gone. The id is never shown:
            // `p.EYWrg13SzrKxYBb` tells nobody anything.
            SidebarRow::Pinned(id) => (
                "view-list-symbolic",
                model
                    .pinned_name(id)
                    .unwrap_or(super::pins::UNAVAILABLE)
                    .to_owned(),
            ),
            SidebarRow::PinButton => ("list-add-symbolic", "Pin a playlist".to_owned()),
        };

        content.append(&gtk::Image::from_icon_name(super::icon(icon)));

        let label = gtk::Label::new(Some(&text));
        label.set_hexpand(true);
        label.set_xalign(0.0);
        // A long playlist name must not widen the sidebar out of its clamp.
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        content.append(&label);

        // Apple Music has nothing of its own to load: a catalog search reports
        // through the content pane, not through a sidebar row. A pin has no
        // spinner either — it opens a page, which carries its own.
        if let SidebarRow::Section(view) = entry
            && view != View::Search
        {
            let spinner = adw::Spinner::new();
            spinner.set_size_request(16, 16);
            spinner.set_valign(gtk::Align::Center);
            spinner.set_visible(false);
            content.append(&spinner);
            model.section_spinners.push((view, spinner));
        }

        // Kept so `refresh_pin_names` can fill the name in once there is a
        // library to take it from — see the note there.
        if let SidebarRow::Pinned(id) = &entry {
            model.pin_labels.push((id.clone(), label.clone()));
        }

        let list_row = gtk::ListBoxRow::new();
        // It does something rather than being somewhere, so it must not sit
        // there looking selected once the picker has closed.
        list_row.set_selectable(!matches!(entry, SidebarRow::PinButton));
        list_row.set_child(Some(&content));
        widgets.nav_list.append(&list_row);

        // **Only pins are draggable, and only pins are drop targets.** A section
        // simply has no controller, so a drag over one does nothing and lands
        // nowhere — there is no range to police mid-gesture, which is what made
        // one list cheaper than two here.
        if matches!(entry, SidebarRow::Pinned(_)) {
            super::pins::drag::attach(&list_row, pin_index, sender);
            pin_index += 1;
        }
    }
}

/// Connect `row-selected`, keeping the handler's id.
///
/// Not in `view!` because the macro discards what `connect_*` returns, and the
/// id is the point: `sync_pins` rebuilds these rows, and selecting one during a
/// rebuild would post `SidebarRowChosen` for whatever happened to be at that
/// position — opening a page nobody clicked. Same reasoning, and the same fix,
/// as the volume button in `now_playing::post_view`.
fn sidebar_selection(model: &mut AppModel, widgets: &Widgets, sender: &ComponentSender<AppModel>) {
    let sender = sender.clone();
    let handler = widgets.nav_list.connect_row_selected(move |_, row| {
        if let Some(row) = row {
            sender.input(AppMsg::SidebarRowChosen(row.index()));
        }
    });
    *model.nav_selected.borrow_mut() = Some(handler);
}

/// Rebuild the sidebar's rows after the pins change.
///
/// Every row is thrown away and redrawn, which is affordable — there are seven
/// of them — and simpler than splicing. What it is *not* is free of
/// consequences: clearing a `ListBox` clears its selection, and both the
/// clearing and the reselect emit `row-selected`, so the handler is silenced
/// across the whole operation and the selection is put back by hand.
pub(super) fn rebuild_sidebar(
    model: &mut AppModel,
    widgets: &Widgets,
    sender: &ComponentSender<AppModel>,
) {
    // Taken out rather than borrowed: the rebuild below needs `model` mutably,
    // and a live borrow of one of its fields would stop that.
    let handler = model.nav_selected.borrow_mut().take();
    if let Some(handler) = &handler {
        widgets.nav_list.block_signal(handler);
    }

    widgets.nav_list.remove_all();
    model.section_spinners.clear();
    model.pin_labels.clear();
    sidebar_rows(model, widgets, sender);
    model.refresh_pin_names();

    // Back to the same *row*, wherever it moved to — tracked on the model
    // rather than read off the widget, because by now the widget's positions
    // mean something different. An unpinned row is gone, so its section is the
    // honest answer; an empty selection reads as broken.
    let target = model
        .selected_row
        .clone()
        .and_then(|row| model.sidebar_rows.iter().position(|other| other == &row))
        .or_else(|| {
            model
                .sidebar_rows
                .iter()
                .position(|row| row == &SidebarRow::Section(model.view))
        });
    if let Some(index) = target
        && let Some(row) = i32::try_from(index)
            .ok()
            .and_then(|i| widgets.nav_list.row_at_index(i))
    {
        widgets.nav_list.select_row(Some(&row));
        // **The model has to be told too.** The handler is blocked, so nothing
        // else will say what is now selected — and a model that still names the
        // old row leaves the widget already on the row you are about to click,
        // which emits nothing at all. That is a section that will not open until
        // you visit another one first.
        model.selected_row = model.sidebar_rows.get(index).cloned();
    }

    if let Some(handler) = &handler {
        widgets.nav_list.unblock_signal(handler);
    }
    *model.nav_selected.borrow_mut() = handler;
}

/// The static definition behind a section row — its icon and its label.
fn section_row(view: View) -> &'static super::view::Row {
    View::SIDEBAR
        .iter()
        .find(|row| row.view == view)
        .unwrap_or(&View::SIDEBAR[0])
}

/// The headings drawn above the row that starts each group.
///
/// One `ListBox` with a header func, never one box per group — see the note in
/// `view!` and 285b542. `playlists_row` is passed rather than hard-coded so the
/// heading follows the row if the sections are ever reordered; everything below
/// that row is a pin, which is why Playlists is the last group.
fn sidebar_headers(widgets: &Widgets, playlists_row: i32) {
    widgets.nav_list.set_header_func(move |row, _before| {
        let title = match row.index() {
            0 => "Apple Music",
            1 => "Library",
            index if index == playlists_row => "Playlists",
            _ => return,
        };
        let label = gtk::Label::new(Some(title));
        label.set_xalign(0.0);
        label.set_margin_start(16);
        label.set_margin_top(if row.index() == 0 { 6 } else { 12 });
        label.set_margin_bottom(2);
        label.add_css_class("heading");
        label.add_css_class("dim-label");
        row.set_header(Some(&label));
    });
}

fn sort_menu(model: &mut AppModel, widgets: &Widgets, sender: &ComponentSender<AppModel>) {
    // The sort menu, built imperatively so the radio state can be bound to
    // a stateful action rather than hand-managed across five items.
    {
        // A stateful action gives the popover its radio dots for free, and
        // keeps the checked item honest when the setting is restored.
        let action = gtk::gio::SimpleAction::new_stateful(
            "by",
            Some(&String::static_variant_type()),
            &model.sorts.get(model.view).by.id().to_variant(),
        );
        let sort_sender = sender.clone();
        action.connect_activate(move |action, target| {
            let Some(id) = target.and_then(|t| t.str().map(str::to_owned)) else {
                return;
            };
            action.set_state(&id.to_variant());
            sort_sender.input(AppMsg::SetSort(SortBy::parse(&id)));
        });
        let group = gtk::gio::SimpleActionGroup::new();
        group.add_action(&action);

        // Stateful, so the menu draws its own checkmark rather than us
        // rebuilding the model every time it flips.
        let reverse = gtk::gio::SimpleAction::new_stateful(
            "reverse",
            None,
            &model.sorts.get(model.view).reversed.to_variant(),
        );
        let rev_sender = sender.clone();
        reverse.connect_activate(move |action, _| {
            let now = !action
                .state()
                .and_then(|s| s.get::<bool>())
                .unwrap_or(false);
            action.set_state(&now.to_variant());
            rev_sender.input(AppMsg::ToggleSortDirection);
        });
        group.add_action(&reverse);

        widgets
            .sort_button
            .insert_action_group("sort", Some(&group));
        model.sort_actions = Some((action, reverse));
        // Built here rather than inline, so there is one place that decides
        // what the popover holds — including that Artists get no radio list
        // at all, which an inline version quietly got wrong.
        model.sync_sort_menu(&widgets.sort_button);
    }
}

fn catalog_filter_menu(model: &AppModel, widgets: &Widgets, sender: &ComponentSender<AppModel>) {
    // The catalog type filter, same shape as the sort menu above: a
    // stateful action so the popover draws its own radio dots.
    {
        let menu = gtk::gio::Menu::new();
        for option in CatalogFilter::ALL {
            let item = gtk::gio::MenuItem::new(Some(option.label()), None);
            item.set_action_and_target_value(Some("filter.kind"), Some(&option.id().to_variant()));
            menu.append_item(&item);
        }
        widgets.filter_button.set_menu_model(Some(&menu));

        let action = gtk::gio::SimpleAction::new_stateful(
            "kind",
            Some(&String::static_variant_type()),
            &model.catalog_filter.id().to_variant(),
        );
        let filter_sender = sender.clone();
        action.connect_activate(move |action, target| {
            let Some(id) = target.and_then(|t| t.str().map(str::to_owned)) else {
                return;
            };
            action.set_state(&id.to_variant());
            filter_sender.input(AppMsg::SetCatalogFilter(CatalogFilter::parse(&id)));
        });
        let group = gtk::gio::SimpleActionGroup::new();
        group.add_action(&action);
        widgets
            .filter_button
            .insert_action_group("filter", Some(&group));
    }
}

fn open_on_last_section(model: &AppModel, widgets: &Widgets) {
    // Open on the section we were last in. Selecting fires `row-selected`,
    // which posts SetView — harmless, since the model is already on that
    // view and SetView returns early when unchanged.
    let Some(index) = super::view::section_index(&model.sidebar_rows, model.view) else {
        return;
    };
    if let Some(row) = widgets.nav_list.row_at_index(index) {
        widgets.nav_list.select_row(Some(&row));
    }
}

fn catalog_pagination(widgets: &Widgets, sender: &ComponentSender<AppModel>) {
    // Fetch the next page of catalog results as the list nears its end.
    // Read-only on the adjustment — it never sets a value, so it cannot
    // fight the scrolling it is watching.
    {
        let sender = sender.clone();
        widgets
            .library_scroller
            .vadjustment()
            .connect_value_changed(move |adj| {
                let remaining = adj.upper() - (adj.value() + adj.page_size());
                if remaining < adj.page_size() {
                    sender.input(AppMsg::LoadMoreCatalog);
                }
            });
    }
}

/// Start searching by typing, the way every list-shaped GNOME app does.
///
/// `GtkSearchBar` would give this for free through `key-capture-widget`, but
/// the entry is a header title rather than a revealer strip, so the capture is
/// ours. What it costs is the four guards below, each of which is a way to
/// break something that already works.
fn type_to_search(root: &adw::ApplicationWindow, sender: &ComponentSender<AppModel>) {
    let controller = gtk::EventControllerKey::new();
    // Capture, so it is seen on the way *down* — a list or a button would
    // otherwise consume Space and the arrow keys before this ever ran. The
    // guards are what stop that being a problem.
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let sender = sender.clone();
    controller.connect_key_pressed(move |controller, key, _code, state| {
        // Anything with a modifier is somebody's accelerator, including every
        // one this app registers. Shift is the exception: it is how capitals
        // are typed.
        if state.difference(gtk::gdk::ModifierType::SHIFT_MASK) != gtk::gdk::ModifierType::empty() {
            return gtk::glib::Propagation::Proceed;
        }
        // Only characters. `to_unicode` rejects Escape, Tab, the arrows and
        // the function keys on its own; Space is rejected here as well,
        // because it is how a focused row is activated and a search that
        // begins with a space is nobody's intent.
        let Some(ch) = key.to_unicode().filter(|c| !c.is_control() && *c != ' ') else {
            return gtk::glib::Propagation::Proceed;
        };
        let Some(window) = controller.widget().and_downcast::<adw::ApplicationWindow>() else {
            return gtk::glib::Propagation::Proceed;
        };
        // An `AdwDialog` is presented *into* this window rather than into one
        // of its own, so this controller sees its keystrokes too. Preferences,
        // About, the sign-out confirmation and the first-run gate would each
        // otherwise be typing into a search box behind them.
        if window.visible_dialog().is_some() {
            return gtk::glib::Propagation::Proceed;
        }
        // Somewhere that already wants the key — the search entry itself most
        // of all, which is where the caret lands the moment this fires once.
        if GtkWindowExt::focus(&window)
            .is_some_and(|widget| widget.is::<gtk::Editable>() || widget.is::<gtk::Text>())
        {
            return gtk::glib::Propagation::Proceed;
        }
        sender.input(AppMsg::TypeAhead(ch.to_string()));
        // Stopped, or the character arrives twice — once here and once in the
        // entry that is about to take the focus.
        gtk::glib::Propagation::Stop
    });
    root.add_controller(controller);
}

/// The two widths the window changes shape at.
///
/// **They are cumulative, and they have to be.** `AdwApplicationWindow` applies
/// exactly one breakpoint at a time — `get_current_breakpoint` is singular — and
/// it picks the last-added one whose condition holds, unapplying the rest. So
/// the narrow one repeats the wide one's setter rather than adding to it.
/// Written as two independent functions, this went wrong immediately: below
/// 600px the header adapted and the sidebar *un*-collapsed, because the 700px
/// breakpoint that had collapsed it was no longer the one in force.
///
/// The order of `add_breakpoint` is therefore the priority order, widest first.
///
/// Why two widths rather than one: a single number would change two things at
/// once while the window is being dragged, where 100px apart they read as two
/// deliberate steps. And only this way round works — the sidebar has to
/// collapse *before* the header gives up its title, because that title is
/// standing in for the sidebar row that would otherwise say which section you
/// are in.
///
/// Neither number comes from a measurement. 700px is where the sidebar stops
/// leaving room for a list; 600px is where GNOME's own adaptive guidance puts
/// the threshold, and the entry still *fits* below it — what it stops fitting
/// with is the room to read it.
fn window_breakpoints(
    root: &adw::ApplicationWindow,
    widgets: &Widgets,
    sender: &ComponentSender<AppModel>,
) {
    // **The window has to be able to get narrow.** Without this the navigation
    // sidebar holds 200px open at all times and the app cannot be tiled to half
    // a screen — which is how it is actually used. `AdwOverlaySplitView`
    // already knows how to be a summonable overlay rather than a fixed pane; it
    // just has to be told when.
    let collapse_sidebar = |breakpoint: &adw::Breakpoint| {
        breakpoint.add_setter(&widgets.nav_split, "collapsed", Some(&true.to_value()));
    };

    match adw::BreakpointCondition::parse("max-width: 700px") {
        Ok(condition) => {
            let breakpoint = adw::Breakpoint::new(condition);
            collapse_sidebar(&breakpoint);
            root.add_breakpoint(breakpoint);
        }
        Err(_) => tracing::warn!("unparsable window breakpoint; the sidebar will not collapse"),
    }

    match adw::BreakpointCondition::parse("max-width: 600px") {
        Ok(condition) => {
            let breakpoint = adw::Breakpoint::new(condition);
            // Everything the wider one does, because this one replaces it.
            collapse_sidebar(&breakpoint);
            // Signals rather than a setter for the header, because what changes
            // is model state — which widget is the title, and whether the
            // button is on. A setter can only write a property, and writing the
            // stack's child directly would put the decision somewhere the
            // reducer cannot see.
            let entering = sender.clone();
            breakpoint.connect_apply(move |_| entering.input(AppMsg::NarrowHeader(true)));
            let leaving = sender.clone();
            breakpoint.connect_unapply(move |_| leaving.input(AppMsg::NarrowHeader(false)));
            root.add_breakpoint(breakpoint);
        }
        // Wide is the honest fallback: the entry stays in the title, which is
        // what every window did before this existed.
        Err(_) => tracing::warn!("unparsable header breakpoint; search stays in the header"),
    }
}

fn library_list_properties(model: &AppModel, widgets: &Widgets) {
    // The clamp takes the list here rather than in `view!`: the macro has
    // no form for `set_child` on a `#[local_ref]`, and the list is owned by
    // the model.
    // The clamp takes the list here rather than in `view!`, because the
    // macro has no form for `set_child` on a `#[local_ref]` — so the two
    // properties the list used to carry inline have to be set here too.
    //
    // **They were dropped when this moved, and both symptoms followed.**
    // `navigation-sidebar` is what makes a `GtkListView` transparent, so
    // without it the rows painted the `view` background and read as darker
    // than the window; and without `single-click-activate` a row needed two
    // clicks to play. Neither is decoration: losing them looked like two
    // unrelated bugs in a layout change.
    let list = &model.library.view;
    widgets.library_clamp.set_child(Some(list));
    list.set_single_click_activate(true);
    list.add_css_class("navigation-sidebar");
}

fn bottom_bar_inset(widgets: &Widgets) {
    // **Keep the content clear of the Now Playing bar.**
    //
    // The bar is `AdwBottomSheet`'s `bottom_bar`, and a bottom bar is drawn
    // *over* the content rather than beside it. So the last row of any
    // scrollable sat behind it: reachable by GTK's reckoning — the scroller
    // had already run to its end — and invisible, which is the worst
    // combination, because nothing suggests there is more to see.
    //
    // Maximising appeared to fix it, which sent the first diagnosis after a
    // ten-pixel measurement discrepancy in the detail page's layout. That
    // was real and irrelevant: a taller window simply put the last row above
    // the bar.
    //
    // `bottom-bar-height` is the property libadwaita exposes for exactly
    // this, and it notifies, so the inset follows the bar rather than
    // guessing at its height — which changes with the theme and the text
    // scale.
    {
        let content = widgets.nav_split.clone();
        let sheet = widgets.player_sheet.clone();
        let apply = move |sheet: &adw::BottomSheet| {
            content.set_margin_bottom(sheet.bottom_bar_height());
        };
        apply(&sheet);
        sheet.connect_bottom_bar_height_notify(apply);
    }
}
