//! `org.freedesktop.Application` on the app id's bus name: the portal
//! calls `ActivateAction` here for exported (`app.`) notification
//! actions, D-Bus activating the app when it is not running.

use std::collections::HashMap;
use std::sync::Arc;

use zbus::interface;
use zbus::zvariant::OwnedValue;

use super::portal::{ACTION, Shared};

/// The served object.
#[derive(Debug)]
pub(super) struct Application {
    shared: Arc<Shared>,
}

impl Application {
    pub(super) const fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

#[allow(
    clippy::unused_self,
    clippy::needless_pass_by_value,
    clippy::used_underscore_binding,
    reason = "D-Bus method shapes: `&self` receivers and owned arguments the interface macro requires; the macro also expands the `_` bindings"
)]
#[interface(name = "org.freedesktop.Application")]
impl Application {
    /// Plain activation (the launcher, `gapplication launch`): only the
    /// activation token is kept; the app raises itself.
    fn activate(&self, platform_data: HashMap<String, OwnedValue>) {
        Shared::platform_data(&platform_data);
    }

    /// Files and URIs: not this plugin's (`istmo.deeplinks` reads argv).
    fn open(&self, uris: Vec<String>, platform_data: HashMap<String, OwnedValue>) {
        tracing::debug!(count = uris.len(), "notifications: Open ignored");
        Shared::platform_data(&platform_data);
    }

    /// An exported notification action: the parameter is the target,
    /// then (v2) the reply text.
    fn activate_action(
        &self,
        action_name: String,
        parameter: Vec<OwnedValue>,
        platform_data: HashMap<String, OwnedValue>,
    ) {
        Shared::platform_data(&platform_data);
        if action_name == ACTION {
            self.shared.activate(&parameter);
        }
    }
}
