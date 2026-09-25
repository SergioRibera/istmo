//! Minimal Secret Service client (`org.freedesktop.secrets` on the
//! session bus — GNOME Keyring, `KWallet`, `KeePassXC`, …) backing the
//! opt-in UI-gated vault.
//!
//! Secrets travel over the session bus with the `plain` algorithm, like
//! `secret-tool` does by default; the bus is private to the user's
//! session.

use std::collections::HashMap;

use futures_lite::StreamExt;
use zbus::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use super::super::app_namespace;
use crate::{BiometricError, SecretAlias};

const SCHEMA: &str = "dev.istmo.biometric";
/// Path the Secret Service returns when no prompt is needed.
const NO_PROMPT: &str = "/";

/// `(session, parameters, value, content_type)` — the spec's `Secret`
/// struct, signature `(oayays)`.
type SecretStruct = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

#[zbus::proxy(
    interface = "org.freedesktop.Secret.Service",
    default_service = "org.freedesktop.secrets",
    default_path = "/org/freedesktop/secrets",
    gen_blocking = false
)]
trait Service {
    fn open_session(
        &self,
        algorithm: &str,
        input: &Value<'_>,
    ) -> zbus::Result<(OwnedValue, OwnedObjectPath)>;

    fn search_items(
        &self,
        attributes: HashMap<&str, &str>,
    ) -> zbus::Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>)>;

    fn unlock(
        &self,
        objects: &[ObjectPath<'_>],
    ) -> zbus::Result<(Vec<OwnedObjectPath>, OwnedObjectPath)>;
}

#[zbus::proxy(
    interface = "org.freedesktop.Secret.Collection",
    default_service = "org.freedesktop.secrets",
    default_path = "/org/freedesktop/secrets/aliases/default",
    gen_blocking = false
)]
trait Collection {
    fn create_item(
        &self,
        properties: HashMap<&str, Value<'_>>,
        secret: &(ObjectPath<'_>, Vec<u8>, Vec<u8>, &str),
        replace: bool,
    ) -> zbus::Result<(OwnedObjectPath, OwnedObjectPath)>;
}

#[zbus::proxy(
    interface = "org.freedesktop.Secret.Item",
    default_service = "org.freedesktop.secrets",
    gen_blocking = false
)]
trait Item {
    fn get_secret(&self, session: &ObjectPath<'_>) -> zbus::Result<SecretStruct>;

    fn delete(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.Secret.Prompt",
    default_service = "org.freedesktop.secrets",
    gen_blocking = false
)]
trait Prompt {
    fn prompt(&self, window_id: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn completed(&self, dismissed: bool, result: Value<'_>) -> zbus::Result<()>;
}

/// An open `plain` session on the user's Secret Service.
pub(super) struct Keyring {
    connection: Connection,
    service: ServiceProxy<'static>,
    session: OwnedObjectPath,
    app: String,
}

impl Keyring {
    pub(super) async fn open() -> Result<Self, BiometricError> {
        let connection = Connection::session().await.map_err(unreachable)?;
        let service = ServiceProxy::new(&connection).await.map_err(unreachable)?;
        let (_, session) = service
            .open_session("plain", &Value::from(""))
            .await
            .map_err(unreachable)?;
        Ok(Self {
            connection,
            service,
            session,
            app: app_namespace(),
        })
    }

    pub(super) async fn store(
        &self,
        alias: &SecretAlias,
        secret: Vec<u8>,
    ) -> Result<(), BiometricError> {
        let collection = CollectionProxy::new(&self.connection)
            .await
            .map_err(backend)?;
        let label = format!("istmo biometric secret ({}/{alias})", self.app);
        let properties = HashMap::from([
            (
                "org.freedesktop.Secret.Item.Label",
                Value::from(label.as_str()),
            ),
            (
                "org.freedesktop.Secret.Item.Attributes",
                Value::from(self.attributes(alias)),
            ),
        ]);
        let secret = (
            self.session.as_ref(),
            Vec::new(),
            secret,
            "application/octet-stream",
        );
        let (_, prompt) = collection
            .create_item(properties, &secret, true)
            .await
            .map_err(backend)?;
        self.complete(prompt).await
    }

    pub(super) async fn read(&self, alias: &SecretAlias) -> Result<Vec<u8>, BiometricError> {
        let item = self
            .find(alias)
            .await?
            .ok_or(BiometricError::SecretNotFound)?;
        let item = ItemProxy::builder(&self.connection)
            .path(item)
            .map_err(backend)?
            .build()
            .await
            .map_err(backend)?;
        let (_, _, value, _) = item
            .get_secret(&self.session.as_ref())
            .await
            .map_err(backend)?;
        Ok(value)
    }

    pub(super) async fn delete(&self, alias: &SecretAlias) -> Result<(), BiometricError> {
        while let Some(path) = self.find(alias).await? {
            let item = ItemProxy::builder(&self.connection)
                .path(path)
                .map_err(backend)?
                .build()
                .await
                .map_err(backend)?;
            let prompt = item.delete().await.map_err(backend)?;
            self.complete(prompt).await?;
        }
        Ok(())
    }

    pub(super) async fn contains(&self, alias: &SecretAlias) -> Result<bool, BiometricError> {
        let (unlocked, locked) = self.search(alias).await?;
        Ok(!unlocked.is_empty() || !locked.is_empty())
    }

    /// The item stored for `alias`, unlocking its collection first when
    /// needed.
    async fn find(&self, alias: &SecretAlias) -> Result<Option<OwnedObjectPath>, BiometricError> {
        let (unlocked, locked) = self.search(alias).await?;
        if let Some(item) = unlocked.into_iter().next() {
            return Ok(Some(item));
        }
        let Some(item) = locked.into_iter().next() else {
            return Ok(None);
        };
        let (_, prompt) = self
            .service
            .unlock(&[item.as_ref()])
            .await
            .map_err(backend)?;
        self.complete(prompt).await?;
        Ok(Some(item))
    }

    async fn search(
        &self,
        alias: &SecretAlias,
    ) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>), BiometricError> {
        let attributes = self.attributes(alias);
        let query = attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.service.search_items(query).await.map_err(backend)
    }

    fn attributes(&self, alias: &SecretAlias) -> HashMap<&'static str, String> {
        HashMap::from([
            ("xdg:schema", SCHEMA.to_owned()),
            ("app", self.app.clone()),
            ("alias", alias.to_string()),
        ])
    }

    /// Run the unlock / confirm prompt the service asked for, if any.
    async fn complete(&self, prompt: OwnedObjectPath) -> Result<(), BiometricError> {
        if prompt.as_str() == NO_PROMPT {
            return Ok(());
        }
        let prompt = PromptProxy::builder(&self.connection)
            .path(prompt)
            .map_err(backend)?
            .build()
            .await
            .map_err(backend)?;
        let mut completed = prompt.receive_completed().await.map_err(backend)?;
        prompt.prompt("").await.map_err(backend)?;
        let signal = completed
            .next()
            .await
            .ok_or_else(|| BiometricError::Backend("Secret Service prompt vanished".to_owned()))?;
        let args = signal.args().map_err(backend)?;
        if args.dismissed {
            Err(BiometricError::UserCancelled)
        } else {
            Ok(())
        }
    }
}

#[allow(clippy::needless_pass_by_value)] // Used as `map_err(unreachable)`.
fn unreachable(err: zbus::Error) -> BiometricError {
    BiometricError::UnsupportedOperation(format!("no Secret Service keyring reachable: {err}"))
}

#[allow(clippy::needless_pass_by_value)] // Used as `map_err(backend)`.
fn backend(err: zbus::Error) -> BiometricError {
    BiometricError::Backend(format!("Secret Service: {err}"))
}
