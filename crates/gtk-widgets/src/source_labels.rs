pub use slopsonic_core::runtime::source::{source_kind_icon_name, source_kind_title};

use slopsonic_core::runtime::source::SourceSummary;

pub fn configured_source_display_name(source: &SourceSummary) -> String {
    let name = source.name.trim();
    if name.is_empty() {
        source_kind_title(source.kind).to_string()
    } else {
        name.to_string()
    }
}
