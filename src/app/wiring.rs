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

use relm4::ComponentSender;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;

use super::{AppModel, AppMsg, CatalogFilter, SortBy};

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
    sidebar_headers(widgets);
    sort_menu(model, widgets, sender);
    catalog_filter_menu(model, widgets, sender);
    open_on_last_section(model, widgets);
    catalog_pagination(widgets, sender);
    window_breakpoint(root, widgets);
    library_list_properties(model, widgets);
    bottom_bar_inset(widgets);
}

fn sidebar_headers(widgets: &Widgets) {
    // Sidebar rows, added imperatively so each section is its own ListBox
    // and the two behave as one selection: picking a row in either clears
    // the other, which a single ListBox would do for free but two will not.
    // Section headings, drawn above the row that starts each section.
    widgets.nav_list.set_header_func(|row, _before| {
        let title = match row.index() {
            0 => "Apple Music",
            1 => "Library",
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
    if let Some(row) = widgets.nav_list.row_at_index(model.view.row()) {
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

fn window_breakpoint(root: &adw::ApplicationWindow, widgets: &Widgets) {
    // **The window has to be able to get narrow.** Without this the
    // navigation sidebar holds 200px open at all times and the app cannot
    // be tiled to half a screen — which is how it is actually used.
    //
    // `AdwOverlaySplitView` already knows how to be a summonable overlay
    // rather than a fixed pane; it just has to be told when. This is the
    // standard adaptive pattern and the app simply never had one.
    if let Ok(condition) = adw::BreakpointCondition::parse("max-width: 700px") {
        let breakpoint = adw::Breakpoint::new(condition);
        breakpoint.add_setter(&widgets.nav_split, "collapsed", Some(&true.to_value()));
        root.add_breakpoint(breakpoint);
    } else {
        tracing::warn!("unparsable window breakpoint; the sidebar will not collapse");
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
