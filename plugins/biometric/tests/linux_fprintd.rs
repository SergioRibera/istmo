//! The Linux desktop backend against a fake fprintd and a fake Secret
//! Service served on a private `dbus-daemon`, pointed to through
//! `DBUS_SYSTEM_BUS_ADDRESS` and `DBUS_SESSION_BUS_ADDRESS`. Skips
//! (passes) when `dbus-daemon` is not installed.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use istmo_biometric::{
    AuthMethod, AuthPolicy, AuthPrompt, BiometricClient, BiometricError, BiometricHost,
    BiometricKind, BiometricStatus, DesktopBiometric,
};
use istmo_core::Runtime;
use zbus::object_server::{ObjectServer, SignalEmitter};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const DEVICE_PATH: &str = "/net/reactivated/Fprint/Device/0";

/// How the fake reader answers `VerifyStart`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    MatchAfterRetry,
    NoMatch,
    /// Never answers; the client has to cancel.
    Silent,
}

#[derive(Debug, Default)]
struct Log {
    calls: Vec<String>,
}

struct FakeManager;

#[zbus::interface(name = "net.reactivated.Fprint.Manager")]
#[allow(clippy::unused_self)] // D-Bus methods are dispatched on `&self`.
impl FakeManager {
    fn get_default_device(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(DEVICE_PATH).expect("valid path")
    }
}

struct FakeDevice {
    scenario: Arc<Mutex<Scenario>>,
    log: Arc<Mutex<Log>>,
}

impl FakeDevice {
    fn record(&self, call: impl Into<String>) {
        self.log.lock().expect("log").calls.push(call.into());
    }
}

#[zbus::interface(name = "net.reactivated.Fprint.Device")]
// zbus dispatches D-Bus methods on `&self` and hands the emitter by value.
#[allow(clippy::unused_self, clippy::needless_pass_by_value)]
impl FakeDevice {
    fn list_enrolled_fingers(&self, username: &str) -> Vec<String> {
        self.record(format!("ListEnrolledFingers({username})"));
        vec!["right-index-finger".to_owned()]
    }

    fn claim(&self, username: &str) {
        self.record(format!("Claim({username})"));
    }

    fn release(&self) {
        self.record("Release");
    }

    fn verify_start(&self, finger_name: &str, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        self.record(format!("VerifyStart({finger_name})"));
        let scenario = *self.scenario.lock().expect("scenario");
        let statuses: &[(&str, bool)] = match scenario {
            Scenario::MatchAfterRetry => &[("verify-retry-scan", false), ("verify-match", true)],
            Scenario::NoMatch => &[("verify-no-match", true)],
            Scenario::Silent => &[],
        };
        let emitter = emitter.to_owned();
        let statuses = statuses.to_vec();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            for (result, done) in statuses {
                pollster::block_on(Self::verify_status(&emitter, result, done))
                    .expect("emit VerifyStatus");
            }
        });
    }

    fn verify_stop(&self) {
        self.record("VerifyStop");
    }

    #[zbus(signal)]
    async fn verify_status(
        emitter: &SignalEmitter<'_>,
        result: &str,
        done: bool,
    ) -> zbus::Result<()>;
}

/// Items of the fake Secret Service: path → (attributes, secret).
type Store = Arc<Mutex<HashMap<String, (HashMap<String, String>, Vec<u8>)>>>;

const SESSION_PATH: &str = "/org/freedesktop/secrets/session/1";

fn path(raw: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(raw).expect("valid path")
}

struct FakeSecretService {
    store: Store,
}

#[zbus::interface(name = "org.freedesktop.Secret.Service")]
// zbus dispatches D-Bus methods on `&self` and hands arguments by value.
#[allow(clippy::unused_self, clippy::needless_pass_by_value)]
impl FakeSecretService {
    fn open_session(&self, algorithm: &str, input: Value<'_>) -> (OwnedValue, OwnedObjectPath) {
        assert_eq!((algorithm, input), ("plain", Value::from("")));
        (OwnedValue::from(0u8), path(SESSION_PATH))
    }

    fn search_items(
        &self,
        attributes: HashMap<String, String>,
    ) -> (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) {
        let unlocked = self
            .store
            .lock()
            .expect("store")
            .iter()
            .filter(|(_, (attrs, _))| attributes.iter().all(|(k, v)| attrs.get(k) == Some(v)))
            .map(|(p, _)| path(p))
            .collect();
        (unlocked, Vec::new())
    }

    fn unlock(&self, objects: Vec<OwnedObjectPath>) -> (Vec<OwnedObjectPath>, OwnedObjectPath) {
        (objects, path("/"))
    }
}

struct FakeCollection {
    store: Store,
    next: Mutex<u32>,
}

#[zbus::interface(name = "org.freedesktop.Secret.Collection")]
#[allow(clippy::needless_pass_by_value)] // zbus hands arguments by value.
impl FakeCollection {
    async fn create_item(
        &self,
        properties: HashMap<String, OwnedValue>,
        secret: (OwnedObjectPath, Vec<u8>, Vec<u8>, String),
        replace: bool,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> (OwnedObjectPath, OwnedObjectPath) {
        assert!(replace);
        assert_eq!(secret.0.as_str(), SESSION_PATH);
        let attributes = HashMap::<String, String>::try_from(
            properties["org.freedesktop.Secret.Item.Attributes"]
                .try_clone()
                .expect("clone"),
        )
        .expect("attributes dict");
        let id = {
            let mut next = self.next.lock().expect("counter");
            *next += 1;
            *next
        };
        let item = format!("/org/freedesktop/secrets/collection/login/{id}");
        {
            let mut store = self.store.lock().expect("store");
            store.retain(|_, (attrs, _)| *attrs != attributes);
            store.insert(item.clone(), (attributes, secret.2));
        }
        server
            .at(
                item.as_str(),
                FakeItem {
                    path: item.clone(),
                    store: Arc::clone(&self.store),
                },
            )
            .await
            .expect("serve item");
        (path(&item), path("/"))
    }
}

struct FakeItem {
    path: String,
    store: Store,
}

#[zbus::interface(name = "org.freedesktop.Secret.Item")]
#[allow(clippy::needless_pass_by_value)] // zbus hands arguments by value.
impl FakeItem {
    fn get_secret(&self, session: OwnedObjectPath) -> (OwnedObjectPath, Vec<u8>, Vec<u8>, String) {
        let value = self.store.lock().expect("store")[&self.path].1.clone();
        (
            session,
            Vec::new(),
            value,
            "application/octet-stream".to_owned(),
        )
    }

    fn delete(&self) -> OwnedObjectPath {
        self.store.lock().expect("store").remove(&self.path);
        path("/")
    }
}

/// Serve the fake fprintd and Secret Service; the connections must stay
/// alive for the whole test.
fn serve_fakes(
    address: &str,
    scenario: &Arc<Mutex<Scenario>>,
    log: &Arc<Mutex<Log>>,
) -> (zbus::Connection, zbus::Connection) {
    let fprintd = pollster::block_on(async {
        zbus::connection::Builder::address(address)?
            .name("net.reactivated.Fprint")?
            .serve_at("/net/reactivated/Fprint/Manager", FakeManager)?
            .serve_at(
                DEVICE_PATH,
                FakeDevice {
                    scenario: Arc::clone(scenario),
                    log: Arc::clone(log),
                },
            )?
            .build()
            .await
    })
    .expect("serve fake fprintd");
    let store = Store::default();
    let secrets = pollster::block_on(async {
        zbus::connection::Builder::address(address)?
            .name("org.freedesktop.secrets")?
            .serve_at(
                "/org/freedesktop/secrets",
                FakeSecretService {
                    store: Arc::clone(&store),
                },
            )?
            .serve_at(
                "/org/freedesktop/secrets/aliases/default",
                FakeCollection {
                    store: Arc::clone(&store),
                    next: Mutex::new(0),
                },
            )?
            .build()
            .await
    })
    .expect("serve fake Secret Service");

    (fprintd, secrets)
}

/// The opt-in keyring vault: every store and read passes through a
/// (matching) fprintd verification.
fn ui_gated_vault_round_trips(prompt: &AuthPrompt) {
    let init = Runtime::mock()
        .expects::<BiometricClient>()
        .host(BiometricHost::new(
            DesktopBiometric::default().with_ui_gated_vault(),
        ))
        .finish();
    let vault = BiometricClient::from_runtime(&init.runtime).expect("client declared");

    let availability =
        pollster::block_on(vault.availability(AuthPolicy::BiometricStrong)).expect("availability");
    assert!(availability.vault_available);
    assert!(!pollster::block_on(vault.has_secret("token".to_owned())).expect("has"));
    let err = pollster::block_on(vault.read_secret("token".to_owned(), prompt.clone()))
        .expect_err("missing");
    assert_eq!(
        BiometricError::try_from(err).expect("domain error"),
        BiometricError::SecretNotFound
    );

    pollster::block_on(vault.store_secret("token".to_owned(), b"s3cr3t".to_vec(), prompt.clone()))
        .expect("store");
    pollster::block_on(vault.store_secret("token".to_owned(), b"rotated".to_vec(), prompt.clone()))
        .expect("replace");
    assert!(pollster::block_on(vault.has_secret("token".to_owned())).expect("has"));
    let secret =
        pollster::block_on(vault.read_secret("token".to_owned(), prompt.clone())).expect("read");
    assert_eq!(secret, b"rotated");

    pollster::block_on(vault.delete_secret("token".to_owned())).expect("delete");
    assert!(!pollster::block_on(vault.has_secret("token".to_owned())).expect("has"));
}

struct PrivateBus {
    daemon: Child,
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn start_bus() -> Option<(PrivateBus, String)> {
    let mut daemon = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = daemon.stdout.take()?;
    let mut address = String::new();
    BufReader::new(stdout).read_line(&mut address).ok()?;
    Some((PrivateBus { daemon }, address.trim().to_owned()))
}

#[test]
fn fprintd_backend_round_trips() {
    let Some((_bus, address)) = start_bus() else {
        eprintln!("dbus-daemon not available; skipping");
        return;
    };
    // SAFETY: this test binary holds a single test and sets the variable
    // before starting any thread that could read the environment.
    unsafe {
        std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &address);
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address);
    }

    let scenario = Arc::new(Mutex::new(Scenario::MatchAfterRetry));
    let log = Arc::new(Mutex::new(Log::default()));
    let _services = serve_fakes(&address, &scenario, &log);

    let init = Runtime::mock()
        .expects::<BiometricClient>()
        .host(BiometricHost::new(DesktopBiometric::default()))
        .finish();
    let client = BiometricClient::from_runtime(&init.runtime).expect("client declared");
    let prompt = AuthPrompt::new("Unlock", "Touch the fingerprint reader");

    let availability =
        pollster::block_on(client.availability(AuthPolicy::BiometricStrong)).expect("availability");
    assert_eq!(availability.status, BiometricStatus::Available);
    assert_eq!(availability.kinds, vec![BiometricKind::Fingerprint]);
    assert!(!availability.device_credential_available);

    let method = pollster::block_on(client.authenticate(prompt.clone())).expect("match");
    assert_eq!(method, AuthMethod::Biometric);
    assert_eq!(
        std::mem::take(&mut log.lock().expect("log").calls),
        [
            "ListEnrolledFingers()",
            "Claim()",
            "VerifyStart(any)",
            "VerifyStop",
            "Release"
        ]
    );

    let enrollment = pollster::block_on(client.enrollment_state()).expect("enrollment state");
    assert_eq!(enrollment.as_deref(), Some(&b"right-index-finger"[..]));
    assert!(!availability.vault_available, "vault is opt-in");
    let err = pollster::block_on(client.store_secret(
        "token".to_owned(),
        b"s3cr3t".to_vec(),
        prompt.clone(),
    ))
    .expect_err("vault disabled");
    assert!(matches!(
        BiometricError::try_from(err),
        Ok(BiometricError::UnsupportedOperation(_))
    ));
    ui_gated_vault_round_trips(&prompt);
    log.lock().expect("log").calls.clear();

    *scenario.lock().expect("scenario") = Scenario::NoMatch;
    let err = pollster::block_on(client.authenticate(prompt.clone())).expect_err("no match");
    assert_eq!(
        BiometricError::try_from(err).expect("domain error"),
        BiometricError::AuthFailed
    );
    log.lock().expect("log").calls.clear();

    *scenario.lock().expect("scenario") = Scenario::Silent;
    let mut call = Box::pin(client.authenticate(prompt));
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(call.as_mut().poll(&mut cx).is_pending());
    std::thread::sleep(Duration::from_millis(300));
    drop(call);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let calls = log.lock().expect("log").calls.clone();
        if calls.ends_with(&["VerifyStop".to_owned(), "Release".to_owned()]) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "cancellation did not stop the reader: {calls:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
