use crate::shell::Shell;
use gtk_preferences::Effect;
use std::rc::Rc;

impl Shell {
    pub(crate) fn apply_preferences_effect(self: &Rc<Self>, effect: Effect) {
        match effect {
            Effect::LayoutChanged => self.update_layout(),
            Effect::SidebarChanged => self.rebuild_sidebar_navigation(),
            Effect::CatalogChanged => self.reconcile_mounted_route(),
            Effect::RefreshHome => self.refresh_mounted_home(),
        }
    }
}
