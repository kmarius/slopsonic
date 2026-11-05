use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rufin_core::runtime::source::{SourceOperation, SourceStatus, SourceSummary};

pub mod login;
pub(super) mod manage;

pub struct SourceState {
    pub configured: RefCell<Option<SourceStatus>>,
    pub operation: RefCell<SourceOperation>,
    pub add_server: RefCell<Option<login::SourceSetupViewHandle>>,
    pub refresh_feedback_generation: Rc<Cell<u64>>,
}

pub fn half_stars_row(shell: &Rc<crate::Preferences>, source: &SourceSummary) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title("Enable partial star ratings".to_string())
        .subtitle("This is not supported natively by this source, but Slopsonic can show partial stars and then round them up for the source".to_string())
        .active(source.half_stars_enabled)
        .build();
    let source_handle = shell.products.source.clone();
    let source_id = source.id.clone();
    row.connect_active_notify(move |row| {
        source_handle.set_half_stars(source_id.clone(), row.is_active());
    });
    row
}
