use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slopsonic_core::runtime::source::{ServerOperation, ServerStatus};

pub mod login;
pub(super) mod manage;

pub struct SourceState {
    pub configured: RefCell<Option<ServerStatus>>,
    pub operation: RefCell<ServerOperation>,
    pub add_server: RefCell<Option<login::SourceSetupViewHandle>>,
    pub refresh_feedback_generation: Rc<Cell<u64>>,
}
