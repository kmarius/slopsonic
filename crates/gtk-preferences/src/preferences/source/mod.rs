use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rufin_core::runtime::source::{ConfiguredSources, SourceOperation, SourceSummary};

pub(super) mod local_access;
pub mod login;

pub struct SourceState {
    pub configured: RefCell<ConfiguredSources>,
    pub operation: RefCell<SourceOperation>,
    pub add_server: RefCell<Option<login::SourceSetupViewHandle>>,
    pub refresh_feedback_generation: Rc<Cell<u64>>,
}

pub(super) fn preference_source_choices(
    shell: &Rc<crate::Preferences>,
    button: &gtk::MenuButton,
) -> (Rc<[SourceSummary]>, gtk::SingleSelection) {
    let configured = shell.source.configured.borrow();
    let sources: Rc<[SourceSummary]> = configured.sources.iter().cloned().collect();
    let selected = shell
        .state
        .source_choice
        .borrow()
        .clone()
        .or_else(|| configured.selected_source_id.clone());
    let index = sources
        .iter()
        .position(|source| Some(&source.id) == selected.as_ref())
        .unwrap_or(0) as u32;
    let selection =
        gtk_widgets::source_labels::bind_source_picker(button, sources.clone(), index, false);
    let weak = Rc::downgrade(shell);
    let choices = sources.clone();
    selection.connect_selected_notify(move |selection| {
        if let (Some(shell), Some(source)) =
            (weak.upgrade(), choices.get(selection.selected() as usize))
        {
            *shell.state.source_choice.borrow_mut() = Some(source.id.clone());
        }
    });
    (sources, selection)
}

pub fn configured_source_kind_display_name(kind: &str) -> String {
    gtk_widgets::source_labels::source_kind_title(kind).map_or_else(|| kind.to_string(), String::from)
}

pub fn half_stars_row(
    shell: &Rc<crate::Preferences>,
    source: &SourceSummary,
) -> Option<adw::SwitchRow> {
    if !matches!(source.kind.as_str(), "navidrome" | "subsonic") {
        return None;
    }
    let row = adw::SwitchRow::builder()
        .title("Enable partial star ratings".to_string())
        .subtitle("This is not supported natively by this source, but Rufin can show partial stars and then round them up for the source".to_string())
        .active(source.half_stars_enabled)
        .build();
    let source_handle = shell.products.source.clone();
    let source_id = source.id.clone();
    row.connect_active_notify(move |row| {
        source_handle.set_half_stars(source_id.clone(), row.is_active());
    });
    Some(row)
}
