//! Integration test: scripted `Biometric` backend hosted in-process,
//! exercising the full wire round-trip (encode → dispatch → decode) for
//! every method, the typed error surface and call cancellation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use istmo_biometric::{
    AuthMethod, AuthPolicy, AuthPrompt, Availability, Biometric, BiometricClient, BiometricError,
    BiometricHost, BiometricKind, BiometricStatus, SecretAlias,
};
use istmo_core::{CancelToken, Runtime};

/// What the scripted backend answers to the next `authenticate`.
#[derive(Debug, Clone)]
enum Script {
    Succeed(AuthMethod),
    Fail(BiometricError),
    /// Park until the call is cancelled, then report it on the channel.
    WaitForCancel(flume::Sender<()>),
}

#[derive(Debug)]
struct ScriptedBackend {
    availability: Availability,
    script: Mutex<Script>,
    prompts: Mutex<Vec<AuthPrompt>>,
    vault: Mutex<HashMap<String, Vec<u8>>>,
}

impl ScriptedBackend {
    fn new(script: Script) -> Self {
        Self {
            availability: Availability {
                status: BiometricStatus::Available,
                kinds: vec![BiometricKind::Fingerprint, BiometricKind::Face],
                device_credential_available: true,
            },
            script: Mutex::new(script),
            prompts: Mutex::new(Vec::new()),
            vault: Mutex::new(HashMap::new()),
        }
    }
}

/// Shared handle so the test keeps inspecting the backend after moving
/// it into the host.
#[derive(Debug, Clone)]
struct Shared(Arc<ScriptedBackend>);

impl std::ops::Deref for Shared {
    type Target = ScriptedBackend;

    fn deref(&self) -> &ScriptedBackend {
        &self.0
    }
}

impl Biometric for Shared {
    async fn availability(&self, policy: AuthPolicy) -> Result<Availability, BiometricError> {
        match policy {
            AuthPolicy::BiometricStrong | AuthPolicy::BiometricOrDeviceCredential => {
                Ok(self.availability.clone())
            }
            AuthPolicy::BiometricWeak => Ok(Availability {
                status: BiometricStatus::NoHardware,
                kinds: Vec::new(),
                device_credential_available: true,
            }),
        }
    }

    async fn authenticate(
        &self,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<AuthMethod, BiometricError> {
        self.prompts.lock().expect("prompts").push(prompt);
        let script = self.script.lock().expect("script").clone();
        match script {
            Script::Succeed(method) => Ok(method),
            Script::Fail(err) => Err(err),
            Script::WaitForCancel(done) => {
                cancel.cancelled().await;
                done.send(()).expect("test still listening");
                Err(BiometricError::SystemCancelled)
            }
        }
    }

    async fn store_secret(
        &self,
        alias: String,
        secret: Vec<u8>,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<(), BiometricError> {
        let alias = SecretAlias::try_from(alias)?;
        self.authenticate(prompt, cancel).await?;
        self.vault
            .lock()
            .expect("vault")
            .insert(alias.into(), secret);
        Ok(())
    }

    async fn read_secret(
        &self,
        alias: String,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<Vec<u8>, BiometricError> {
        let alias = SecretAlias::try_from(alias)?;
        self.authenticate(prompt, cancel).await?;
        self.vault
            .lock()
            .expect("vault")
            .get(alias.as_str())
            .cloned()
            .ok_or(BiometricError::SecretNotFound)
    }

    async fn delete_secret(&self, alias: String) -> Result<(), BiometricError> {
        let alias = SecretAlias::try_from(alias)?;
        self.vault.lock().expect("vault").remove(alias.as_str());
        Ok(())
    }

    async fn has_secret(&self, alias: String) -> Result<bool, BiometricError> {
        let alias = SecretAlias::try_from(alias)?;
        Ok(self
            .vault
            .lock()
            .expect("vault")
            .contains_key(alias.as_str()))
    }
}

fn build(script: Script) -> (Arc<Runtime>, BiometricClient, Arc<ScriptedBackend>) {
    let backend = Arc::new(ScriptedBackend::new(script));
    let init = Runtime::mock()
        .expects::<BiometricClient>()
        .host(BiometricHost::new(Shared(Arc::clone(&backend))))
        .finish();
    let client = BiometricClient::from_runtime(&init.runtime).expect("client declared");
    (init.runtime, client, backend)
}

#[test]
fn availability_round_trips_per_policy() {
    let (_rt, client, _backend) = build(Script::Succeed(AuthMethod::Biometric));

    let strong =
        pollster::block_on(client.availability(AuthPolicy::BiometricStrong)).expect("availability");
    assert!(strong.is_available());
    assert_eq!(
        strong.kinds,
        vec![BiometricKind::Fingerprint, BiometricKind::Face]
    );
    assert!(strong.device_credential_available);

    let weak =
        pollster::block_on(client.availability(AuthPolicy::BiometricWeak)).expect("availability");
    assert!(!weak.is_available());
    assert_eq!(weak.status, BiometricStatus::NoHardware);
}

#[test]
fn authenticate_forwards_the_prompt_and_returns_the_method() {
    let (_rt, client, backend) = build(Script::Succeed(AuthMethod::DeviceCredential));

    let prompt = AuthPrompt::new("Unlock", "Open your vault")
        .subtitle("Vault")
        .cancel_label("Not now")
        .fallback_label("")
        .policy(AuthPolicy::BiometricOrDeviceCredential)
        .confirmation_required(false);
    let method = pollster::block_on(client.authenticate(prompt.clone())).expect("authenticate");
    assert_eq!(method, AuthMethod::DeviceCredential);

    let seen = backend.prompts.lock().expect("prompts").clone();
    assert_eq!(seen, vec![prompt]);
}

#[test]
fn authenticate_surfaces_typed_errors() {
    for expected in [
        BiometricError::UserCancelled,
        BiometricError::LockedOutPermanent,
        BiometricError::NotAvailable(BiometricStatus::NoneEnrolled),
        BiometricError::InvalidPrompt("empty reason".to_owned()),
    ] {
        let (_rt, client, _backend) = build(Script::Fail(expected.clone()));
        let err = pollster::block_on(client.authenticate(AuthPrompt::new("t", "r")))
            .expect_err("backend fails");
        let domain = BiometricError::try_from(err).expect("domain error");
        assert_eq!(domain, expected);
    }
}

#[test]
fn secrets_round_trip_through_the_vault() {
    let (_rt, client, backend) = build(Script::Succeed(AuthMethod::Biometric));
    let prompt = AuthPrompt::new("Unlock", "Read your token");
    let has = |alias: &str| pollster::block_on(client.has_secret(alias.to_owned())).expect("has");

    assert!(!has("api.token"));
    pollster::block_on(client.store_secret(
        "api.token".to_owned(),
        b"s3cr3t".to_vec(),
        prompt.clone(),
    ))
    .expect("store");
    assert!(has("api.token"));

    let secret = pollster::block_on(client.read_secret("api.token".to_owned(), prompt.clone()))
        .expect("read");
    assert_eq!(secret, b"s3cr3t");
    assert_eq!(backend.prompts.lock().expect("prompts").len(), 2);

    pollster::block_on(client.delete_secret("api.token".to_owned())).expect("delete");
    pollster::block_on(client.delete_secret("api.token".to_owned())).expect("delete is idempotent");
    assert!(!has("api.token"));

    let err = pollster::block_on(client.read_secret("api.token".to_owned(), prompt))
        .expect_err("deleted");
    assert_eq!(
        BiometricError::try_from(err).expect("domain error"),
        BiometricError::SecretNotFound
    );
}

#[test]
fn secret_reads_fail_when_verification_fails() {
    let (_rt, client, _backend) = build(Script::Fail(BiometricError::KeyInvalidated));
    let err = pollster::block_on(client.read_secret(
        "api.token".to_owned(),
        AuthPrompt::new("Unlock", "Read your token"),
    ))
    .expect_err("key invalidated");
    assert_eq!(
        BiometricError::try_from(err).expect("domain error"),
        BiometricError::KeyInvalidated
    );
}

#[test]
fn secret_aliases_are_validated() {
    assert_eq!(
        SecretAlias::try_from("user-42_refresh.token")
            .expect("valid")
            .as_str(),
        "user-42_refresh.token"
    );
    for bad in ["", "has space", "../escape", "ñandú", &"x".repeat(65)] {
        assert_eq!(
            SecretAlias::try_from(bad),
            Err(BiometricError::InvalidAlias(bad.to_owned())),
        );
    }

    let (_rt, client, _backend) = build(Script::Succeed(AuthMethod::Biometric));
    let err = pollster::block_on(client.has_secret("../escape".to_owned())).expect_err("invalid");
    assert_eq!(
        BiometricError::try_from(err).expect("domain error"),
        BiometricError::InvalidAlias("../escape".to_owned())
    );
}

#[test]
fn transport_errors_are_not_mistaken_for_domain_errors() {
    let err = BiometricError::try_from(istmo_core::IstmoError::ChannelClosed);
    assert!(matches!(err, Err(istmo_core::IstmoError::ChannelClosed)));
}

#[test]
fn dropping_the_call_cancels_the_backend() {
    let (done_tx, done_rx) = flume::bounded(1);
    let (_rt, client, _backend) = build(Script::WaitForCancel(done_tx));

    let mut call = Box::pin(client.authenticate(AuthPrompt::new("t", "r")));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(matches!(call.as_mut().poll(&mut cx), Poll::Pending));
    drop(call);

    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("backend observed the cancellation");
}
