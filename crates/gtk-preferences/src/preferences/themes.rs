use crate::{AccentPreference, Preferences, ThemePreference};
use adw::prelude::*;
use std::rc::Rc;

const SCHEMES: [ThemePreference; 3] = [
    ThemePreference::System,
    ThemePreference::Light,
    ThemePreference::Dark,
];

pub(super) fn page(shell: &Rc<Preferences>) -> adw::PreferencesPage {
    let resource = crate::ui_resource::THEME_PREFERENCES_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(&builder, resource, {
        scheme_row: adw::ComboRow, accent_row: adw::ComboRow,
    });
    let appearance = shell.settings.current.borrow().appearance.clone();
    scheme_row.set_selected(
        SCHEMES
            .iter()
            .position(|scheme| *scheme == appearance.theme_preference)
            .unwrap_or_default() as u32,
    );
    accent_row.set_selected(
        AccentPreference::ALL
            .iter()
            .position(|accent| *accent == appearance.accent_preference)
            .unwrap_or_default() as u32,
    );
    let weak = Rc::downgrade(shell);
    scheme_row.connect_selected_notify(move |row| {
        let (Some(shell), Some(scheme)) = (weak.upgrade(), SCHEMES.get(row.selected() as usize))
        else {
            return;
        };
        if let Some(settings) =
            shell
                .settings
                .set_app_setting("theme setting", *scheme, |settings| {
                    &mut settings.appearance.theme_preference
                })
        {
            shell.appearance.apply(&settings);
        }
    });
    let weak = Rc::downgrade(shell);
    accent_row.connect_selected_notify(move |row| {
        let (Some(shell), Some(accent)) = (
            weak.upgrade(),
            AccentPreference::ALL.get(row.selected() as usize),
        ) else {
            return;
        };
        if let Some(settings) =
            shell
                .settings
                .set_app_setting("accent setting", *accent, |settings| {
                    &mut settings.appearance.accent_preference
                })
        {
            shell.appearance.apply(&settings);
        }
    });
    gtk_widgets::ui_resource::object(&builder, resource, "theme_page")
}
