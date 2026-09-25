//! The Linux desktop backend against a fake fprintd served on a private
//! `dbus-daemon`, pointed to through `DBUS_SYSTEM_BUS_ADDRESS`. Skips
//! (passes) when `dbus-daemon` is not installed.

#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use istmo_biometric::{
    AuthMethod, AuthPolicy, AuthPrompt, BiometricClient, BiometricError, BiometricHost,
    BiometricKind, BiometricStatus, DesktopBiometric,
};
use istmo_core::Runtime;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedObjectPath;

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
    unsafe { std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &address) };

    let scenario = Arc::new(Mutex::new(Scenario::MatchAfterRetry));
    let log = Arc::new(Mutex::new(Log::default()));
    let _service = pollster::block_on(async {
        zbus::connection::Builder::address(address.as_str())?
            .name("net.reactivated.Fprint")?
            .serve_at("/net/reactivated/Fprint/Manager", FakeManager)?
            .serve_at(
                DEVICE_PATH,
                FakeDevice {
                    scenario: Arc::clone(&scenario),
                    log: Arc::clone(&log),
                },
            )?
            .build()
            .await
    })
    .expect("serve fake fprintd");

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
