use adw::prelude::*;
use slopsonic_core::settings::right_panel::RightPanelSettings;
use std::cell::RefCell;
use std::rc::Rc;

pub struct RightPanelWidgets {
    pub queue_loading: adw::Spinner,
    pub root: gtk::Box,
    pub queue_panel: gtk::Box,
    pub(crate) queue_header: gtk_widgets::table_header::ColumnViewHeader,
    pub queue_search: gtk::SearchEntry,
    pub queue_clear_button: gtk::Button,
    pub queue_playlist_button: gtk::Button,
    pub queue_customize_button: gtk::Button,
    pub(crate) queue_width_fit: RefCell<Option<gtk_widgets::table_sizing::ColumnViewWidthFit>>,
    queue_region: gtk::Overlay,
}

pub fn build_right_panel() -> RightPanelWidgets {
    let resource = crate::ui_resource::RIGHT_PANEL_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        queue_loading: adw::Spinner,
        root: gtk::Box,
        queue_region: gtk::Overlay,
        queue_panel: gtk::Box,
        queue_search: gtk::SearchEntry,
        queue_clear_button: gtk::Button,
        queue_playlist_button: gtk::Button,
        queue_customize_button: gtk::Button,
    });
    root.append(&queue_region);
    gtk_widgets::controls::configure_search_entry(&queue_search);
    queue_search.connect_text_notify(|entry| {
        if entry.text().is_empty() {
            entry.remove_css_class("has-query");
        } else {
            entry.add_css_class("has-query");
        }
    });
    queue_search.connect_stop_search(move |entry| {
        entry.set_text("");
        if let Some(root) = entry.root() {
            root.set_focus(gtk::Widget::NONE);
        }
    });
    RightPanelWidgets {
        queue_loading,
        root,
        queue_panel,
        queue_header: gtk_widgets::table_header::ColumnViewHeader::new(),
        queue_search,
        queue_clear_button,
        queue_playlist_button,
        queue_customize_button,
        queue_width_fit: RefCell::new(None),
        queue_region,
    }
}

impl RightPanelWidgets {
    fn rebuild(&self, settings: &RightPanelSettings) {
        self.queue_region.set_visible(settings.queue_visible);
    }
}

impl crate::PlayerUi {
    pub fn update_right_panel_button(&self) {
        let visible = self.right_sidebar_visible();
        let label = if visible {
            "Hide sidebar".to_string()
        } else {
            "Show sidebar".to_string()
        };
        crate::icons::set_queue_sidebar_icon(&self.views.player_controls.queue_icon, visible);
        self.views
            .player_controls
            .queue_button
            .set_tooltip_text(Some(&label));
        self.views
            .player_controls
            .queue_button
            .update_property(&[gtk::accessible::Property::Label(&label)]);
    }

    pub fn set_sidebar_queue_visible(&self, visible: bool) {
        self.settings
            .set_app_setting("sidebar queue visibility", visible, |settings| {
                &mut settings.right_panel.queue_visible
            });
        self.arrange_sidebar_panels();
    }

    fn arrange_sidebar_panels(&self) {
        let settings = self.settings.current.borrow().right_panel.clone();
        self.right_panel.rebuild(&settings);
    }
}

pub fn apply_sidebar_queue_visibility(shell: &Rc<crate::PlayerUi>) {
    shell.arrange_sidebar_panels();
}
