//! Placeholder until the backend lands.

use std::sync::Arc;

use super::super::backend::AudioBackend;

pub(crate) fn backend() -> Arc<dyn AudioBackend> {
    super::pending_audio()
}
