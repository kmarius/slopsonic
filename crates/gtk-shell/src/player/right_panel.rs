use std::rc::Rc;

use crate::RightSidebarMode;
use adw::prelude::*;

use crate::shell::Shell;
use crate::shell::layout::{ActiveLayoutProfile, resolve_layout};

pub(crate) struct RightPanelWidgets {
    pub(crate) right_split: gtk::Paned,
    pub(crate) right_panel_slot: gtk::ScrolledWindow,
    pub(crate) right_resize_handle: gtk::Box,
}

impl Shell {
    pub(crate) fn toggle_right_panel(self: &Rc<Self>) {
        let visible = self.right_sidebar_visible();
        self.set_right_sidebar_visible(!visible);
    }

    pub(crate) fn set_right_sidebar_visible(self: &Rc<Self>, visible: bool) {
        let active_profile =
            resolve_layout(&self.settings.current.borrow().layout, self.layout_width()).profile;
        self.settings
            .update_app_settings("right sidebar setting", |settings| {
                let profile = match active_profile {
                    ActiveLayoutProfile::Default => &mut settings.layout.default_profile,
                    ActiveLayoutProfile::Narrow => &mut settings.layout.narrow_profile,
                };
                if visible {
                    if profile.right_sidebar.is_visible() {
                        return false;
                    }
                    profile.right_sidebar = RightSidebarMode::Visible;
                } else {
                    if !profile.right_sidebar.is_visible() {
                        return false;
                    }
                    profile.right_sidebar = RightSidebarMode::Hidden;
                }
                settings.layout.sanitize();
                true
            });
        self.update_layout();
        self.chrome.window.queue_resize();
    }
}
