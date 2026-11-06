use crate::Preferences;
use slopsonic_core::runtime::ScrobblingPreferences;
use slopsonic_core::settings::HomeBlockKind;
use std::rc::Rc;
use tracing::warn;

impl Preferences {
    pub(super) fn update_scrobbling_settings(
        self: &Rc<Self>,
        warning_action: &'static str,
        update: impl FnOnce(&mut ScrobblingPreferences) -> bool,
    ) {
        let mut preferences = self.products.scrobbling.preferences();
        if !update(&mut preferences) {
            return;
        }
        if let Err(error) = self.products.scrobbling.save(preferences) {
            warn!(%error, action = warning_action, "failed to save scrobbling settings");
        }
    }

    pub(super) fn save_scrobbling_credential(
        self: &Rc<Self>,
        warning_action: &'static str,
        edit: slopsonic_core::scrobbling::ScrobblingCredentialEdit,
    ) {
        let shell = Rc::clone(self);
        gtk::glib::spawn_future_local(async move {
            if let Err(error) = shell.products.scrobbling.save_credential(edit).await {
                warn!(%error, action = warning_action, "failed to save scrobbling settings");
            }
        });
    }

    pub(super) fn set_home_blocks(self: &Rc<Self>, blocks: Vec<HomeBlockKind>) {
        let previous = self
            .settings
            .current
            .borrow()
            .application
            .home_blocks
            .clone();
        let added = blocks.iter().any(|block| !previous.contains(block));
        if self
            .settings
            .set_app_setting("home block settings", blocks, |settings| {
                &mut settings.application.home_blocks
            })
            .is_none()
        {
            return;
        }
        (self.effects)(crate::Effect::CatalogChanged);
        if added {
            (self.effects)(crate::Effect::RefreshHome);
        }
    }
}
