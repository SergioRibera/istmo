//! Placeholder until the backend lands.

use std::sync::Arc;

use crate::types::BackendId;

use super::super::backend::Backend;

pub(crate) fn backend() -> Arc<dyn Backend> {
    super::pending(BackendId::LinuxWlrScreencopy)
}
