use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rufin_core::runtime::source::{SourceOperation, SourceStatus};

pub mod login;
pub(super) mod manage;

pub struct SourceState {
    pub configured: RefCell<Option<SourceStatus>>,
    pub operation: RefCell<SourceOperation>,
    pub add_server: RefCell<Option<login::SourceSetupViewHandle>>,
    pub refresh_feedback_generation: Rc<Cell<u64>>,
}
