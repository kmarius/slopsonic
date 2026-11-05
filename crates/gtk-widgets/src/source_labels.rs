pub use rufin_core::runtime::source::{source_kind_icon_name, source_kind_title};

use adw::prelude::*;
use app_identity::{DISPLAY_NAME, STABLE_APP_ID};
use rufin_core::runtime::source::SourceSummary;
use rufin_core::text::fill;

pub fn configured_source_display_name(source: &SourceSummary) -> String {
    let name = source.name.trim();
    if name.is_empty() {
        source_kind_title(source.kind).to_string()
    } else {
        name.to_string()
    }
}
pub fn configured_source_icon_name(source: &SourceSummary) -> &'static str {
    source_kind_icon_name(source.kind)
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
