use crate::{Settings, ThemePreference};

pub struct ApplicationAppearance {
    override_provider: gtk::CssProvider,
    pub system_style: adw::StyleManager,
}

impl ApplicationAppearance {
    pub fn install() -> Self {
        let appearance = Self {
            override_provider: gtk::CssProvider::new(),
            // A manager without a display observes the OS without forcing its scheme on the app.
            system_style: gtk::glib::Object::new(),
        };
        let override_provider = &appearance.override_provider;
        let Some(display) = gtk::gdk::Display::default() else {
            return appearance;
        };

        let base_provider = gtk::CssProvider::new();
        base_provider.load_from_resource(crate::ui_resource::BASE_CSS_RESOURCE);
        gtk::style_context_add_provider_for_display(
            &display,
            &base_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        gtk::style_context_add_provider_for_display(
            &display,
            override_provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
        );
        let motion_provider = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(
            &display,
            &motion_provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER + 2,
        );
        let gtk_settings = gtk::Settings::for_display(&display);
        let apply_motion = move |settings: &gtk::Settings| {
            motion_provider.load_from_string(if settings.is_gtk_enable_animations() {
                ""
            } else {
                "* { transition: none; animation: none; }"
            });
        };
        apply_motion(&gtk_settings);
        gtk_settings.connect_gtk_enable_animations_notify(apply_motion);

        appearance
    }

    pub fn apply(&self, settings: &Settings) {
        if let Some(gtk_settings) = gtk::Settings::default() {
            if settings.application.reduce_motion {
                gtk_settings.set_gtk_enable_animations(false);
            } else {
                gtk_settings.reset_property("gtk-enable-animations");
            }
        }
        adw::StyleManager::default().set_color_scheme(match settings.appearance.theme_preference {
            ThemePreference::System => adw::ColorScheme::PreferLight,
            ThemePreference::Light => adw::ColorScheme::ForceLight,
            ThemePreference::Dark => adw::ColorScheme::ForceDark,
        });
        self.override_provider
            .load_from_string(&appearance_override_css(settings));
    }
}

fn appearance_override_css(settings: &Settings) -> String {
    let mut css = String::from(":root {\n");
    let palette = match settings.appearance.theme_preference {
        ThemePreference::System => None,
        ThemePreference::Light => Some(rufin_core::themes::colors(false)),
        ThemePreference::Dark => Some(rufin_core::themes::colors(true)),
    };
    for (key, value) in palette.iter().flatten() {
        css.push_str(&format!("  --{key}: {value};\n"));
    }
    if let Some(color) = settings.appearance.accent_preference.color() {
        css.push_str(&format!("  --accent-bg-color: {color};\n  --accent-fg-color: #ffffff;\n  --accent-color: oklab(from var(--accent-bg-color) var(--standalone-color-oklab));\n"));
    }
    css.push_str("}\n");
    css
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AccentPreference;

    fn css(theme: ThemePreference, accent: AccentPreference) -> String {
        let settings = Settings {
            appearance: rufin_core::settings::AppearancePreferences {
                theme_preference: theme,
                accent_preference: accent,
                ..Default::default()
            },
            ..Settings::default()
        };
        appearance_override_css(&settings)
    }

    #[test]
    fn system_appearance_overrides_no_tokens() {
        let css = css(ThemePreference::System, AccentPreference::System);
        assert!(!css.contains("--window-bg-color"));
        assert!(!css.contains("--accent-bg-color"));
    }

    #[test]
    fn explicit_color_schemes_override_surface_tokens_only() {
        let light = css(ThemePreference::Light, AccentPreference::System);
        assert!(light.contains("--window-bg-color: #fafafb"));
        assert!(light.contains("--view-bg-color: #ffffff"));
        assert!(!light.contains("--accent-bg-color"));

        let dark = css(ThemePreference::Dark, AccentPreference::System);
        assert!(dark.contains("--window-bg-color: #222226"));
        assert!(dark.contains("--view-bg-color: #1d1d20"));
        assert!(!dark.contains("--accent-bg-color"));
    }

    #[test]
    fn every_explicit_accent_overrides_the_accent_tokens() {
        let expected = [
            (AccentPreference::Blue, "#3584e4"),
            (AccentPreference::Teal, "#2190a4"),
            (AccentPreference::Green, "#3a944a"),
            (AccentPreference::Yellow, "#c88800"),
            (AccentPreference::Orange, "#ed5b00"),
            (AccentPreference::Red, "#e62d42"),
            (AccentPreference::Pink, "#d56199"),
            (AccentPreference::Purple, "#9141ac"),
            (AccentPreference::Slate, "#6f8396"),
        ];
        for (preference, color) in expected {
            let css = css(ThemePreference::System, preference);
            assert!(css.contains(&format!("--accent-bg-color: {color}")));
            assert!(css.contains("--accent-fg-color: #ffffff"));
            assert!(css.contains("--accent-color: oklab("));
            assert!(!css.contains("--window-bg-color"));
        }
    }
}
