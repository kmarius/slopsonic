pub use rufin_core::runtime::source::{source_kind_icon_name, source_kind_title};

use adw::prelude::*;
use app_identity::{DISPLAY_NAME, STABLE_APP_ID};
use gtk::gio;
use rufin_core::text::{fill};
use rufin_core::runtime::source::SourceSummary;
use std::rc::Rc;

pub fn configured_source_display_name(source: &SourceSummary) -> String {
    let name = source.name.trim();
    if name.is_empty() {
        source_kind_title(&source.kind).map_or_else(|| source.kind.clone(), String::from)
    } else {
        name.to_string()
    }
}
pub fn configured_source_icon_name(source: &SourceSummary) -> &'static str {
    source_kind_icon_name(&source.kind).unwrap_or("rufin-network-server-symbolic")
}
pub fn configure_ownership_toggle(
    button: &gtk::ToggleButton,
    source: Option<&SourceSummary>,
    belongs_to_source: bool,
) {
    let source_icon = source.map(configured_source_icon_name);
    let source_name = source.map(configured_source_display_name);
    button.set_sensitive(source.is_some());
    button.set_active(belongs_to_source && source.is_some());
    let update = move |button: &gtk::ToggleButton| {
        let current = button.is_active() && source_icon.is_some();
        button.set_icon_name(if current {
            source_icon.unwrap_or(STABLE_APP_ID)
        } else {
            STABLE_APP_ID
        });
        if current {
            button.remove_css_class("rufin-ownership-icon");
        } else {
            button.add_css_class("rufin-ownership-icon");
        }
        let tooltip = if current {
            fill(
                "This is owned by {source}. Regular playlists are synced to your remote.",
                &[("source", source_name.as_deref().unwrap_or(DISPLAY_NAME))],
            )
        } else {
            "This is owned by Slopsonic. It can include entries from all configured sources but it is not synced to remotes.".to_string()
        };
        button.set_tooltip_text(Some(&tooltip));
    };
    update(button);
    button.connect_toggled(update);
}

pub fn choice_action(name: &str, active: bool, select: impl Fn() + 'static) -> gio::SimpleAction {
    let action = gio::SimpleAction::new_stateful(name, None, &active.to_variant());
    action.connect_activate(move |action, _| {
        action.set_state(&true.to_variant());
        select();
    });
    action
}

pub fn bind_source_picker(
    button: &gtk::MenuButton,
    sources: Rc<[SourceSummary]>,
    selected: u32,
    include_all: bool,
) -> gtk::SingleSelection {
    let resource = crate::ui_resource::SOURCE_PICKER_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, { content: gtk::Box, label: gtk::Label });
    button.set_child(Some(&content));
    let mut titles = Vec::new();
    if include_all {
        titles.push("All".to_string());
    }
    titles.extend(sources.iter().map(configured_source_display_name));
    let selection = gtk::SingleSelection::new(Some(gtk::StringList::new(
        &titles.iter().map(String::as_str).collect::<Vec<_>>(),
    )));
    selection.set_selected(selected);
    button.set_sensitive(!sources.is_empty());
    let update = {
        let sources = sources.clone();
        let label = label.downgrade();
        move |selection: &gtk::SingleSelection| {
            let Some(label) = label.upgrade() else {
                return;
            };
            if include_all && selection.selected() == 0 {
                label.set_label(&"All".to_string());
            } else if let Some(source) = selection
                .selected()
                .checked_sub(u32::from(include_all))
                .and_then(|index| sources.get(index as usize))
            {
                label.set_label(&configured_source_display_name(source));
            }
        }
    };
    update(&selection);
    selection.connect_selected_notify(update);
    let available = !sources.is_empty();
    let selected = selection.clone();
    button.set_create_popup_func(move |button| {
        let menu = gio::Menu::new();
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        let actions = gio::SimpleActionGroup::new();
        let current = selected.selected();
        let count = sources.len() + usize::from(include_all);
        let order = (0..count)
            .filter(|index| *index as u32 == current)
            .chain((0..count).filter(|index| *index as u32 != current));
        for index in order {
            let title = if include_all && index == 0 {
                "All".to_string()
            } else {
                configured_source_display_name(&sources[index - usize::from(include_all)])
            };
            let name = format!("source-{index}");
            let selection = selected.clone();
            let close = popover.downgrade();
            let action = choice_action(&name, index as u32 == current, move || {
                selection.set_selected(index as u32);
                if let Some(popover) = close.upgrade() {
                    popover.popdown();
                }
            });
            actions.add_action(&action);
            menu.append(Some(&title), Some(&format!("sources.{name}")));
        }
        popover.insert_action_group("sources", Some(&actions));
        button.set_popover(Some(&popover));
        crate::interactions::replace_native_menu_checkmarks(&popover);
    });
    button.set_sensitive(available);
    selection
}
