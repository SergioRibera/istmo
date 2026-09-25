//! `LocalAuthentication` backend (Touch ID, login password fallback) and
//! data-protection keychain vault for biometric-bound secrets.

mod keychain;

use std::sync::Arc;

use block2::RcBlock;
use istmo_core::CancelToken;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LABiometryType, LAContext, LAError, LAPolicy};

use super::{DesktopOptions, until_cancelled};
use crate::{
    AuthMethod, AuthPolicy, AuthPrompt, Availability, BiometricError, BiometricKind,
    BiometricStatus, SecretAlias,
};
use keychain::{SharedContext, VaultQuery};

#[derive(Debug, Default, Clone)]
pub(super) struct Backend;

impl Backend {
    #[allow(clippy::unused_async)] // Async like every platform backend.
    pub(super) async fn availability(
        &self,
        _options: &DesktopOptions,
        policy: AuthPolicy,
    ) -> Result<Availability, BiometricError> {
        // SAFETY: `LAContext` has no initialisation preconditions.
        let context = unsafe { LAContext::new() };
        let biometrics = can_evaluate(&context, LAPolicy::DeviceOwnerAuthenticationWithBiometrics);
        // `biometryType` is only meaningful after a `canEvaluatePolicy`
        // call on the same context.
        // SAFETY: plain property read on a live context.
        let kinds = match unsafe { context.biometryType() } {
            LABiometryType::TouchID => vec![BiometricKind::Fingerprint],
            LABiometryType::FaceID => vec![BiometricKind::Face],
            LABiometryType::OpticID => vec![BiometricKind::Iris],
            _ => Vec::new(),
        };
        let credential = can_evaluate(&context, LAPolicy::DeviceOwnerAuthentication);
        let evaluated = match policy {
            AuthPolicy::BiometricStrong | AuthPolicy::BiometricWeak => biometrics,
            AuthPolicy::BiometricOrDeviceCredential => credential,
        };
        let status = evaluated.map_or_else(LaErrorCode::status, |()| BiometricStatus::Available);
        let device_credential_available = credential.is_ok();
        // Vault items are only accessible while a login password is set.
        let vault_available = device_credential_available && VaultQuery::keychain_usable();
        Ok(Availability {
            status,
            kinds,
            device_credential_available,
            vault_available,
        })
    }

    pub(super) async fn authenticate(
        &self,
        _options: &DesktopOptions,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<AuthMethod, BiometricError> {
        let (reply_tx, reply_rx) = flume::bounded(1);
        let (cancel_tx, cancel_rx) = flume::bounded(1);
        // `LAContext` is not `Send`: it lives on its own thread for the
        // whole evaluation and is only reached through channels.
        std::thread::Builder::new()
            .name("istmo-biometric-la".to_owned())
            .spawn(move || Evaluation::new(&prompt).run(&reply_tx, &cancel_rx))
            .map_err(|err| BiometricError::Backend(format!("spawn LAContext thread: {err}")))?;

        match until_cancelled(reply_rx.recv_async(), &cancel).await {
            Some(Ok(outcome)) => outcome,
            Some(Err(_)) => Err(BiometricError::Backend(
                "LAContext thread exited without a reply".to_owned(),
            )),
            None => {
                // Invalidating the context dismisses the sheet.
                let _ = cancel_tx.send(());
                Err(BiometricError::SystemCancelled)
            }
        }
    }
}

impl Backend {
    #[allow(clippy::unused_async)] // Async like every platform backend.
    pub(super) async fn store_secret(
        &self,
        _options: &DesktopOptions,
        alias: &SecretAlias,
        secret: Vec<u8>,
        prompt: AuthPrompt,
        _cancel: CancelToken,
    ) -> Result<(), BiometricError> {
        VaultQuery::item(alias).delete()?;
        VaultQuery::item(alias).add(&secret, prompt.policy)
    }

    pub(super) async fn read_secret(
        &self,
        _options: &DesktopOptions,
        alias: &SecretAlias,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<Vec<u8>, BiometricError> {
        let context = SharedContext::new();
        context.configure(&prompt);
        let (reply_tx, reply_rx) = flume::bounded(1);
        // `SecItemCopyMatching` blocks while the Touch ID sheet is up.
        let worker_context = Arc::clone(&context);
        let alias = alias.clone();
        std::thread::Builder::new()
            .name("istmo-biometric-keychain".to_owned())
            .spawn(move || {
                let data = VaultQuery::item(&alias)
                    .authenticated_by(&worker_context)
                    .copy_data();
                let _ = reply_tx.send(data);
            })
            .map_err(|err| BiometricError::Backend(format!("spawn keychain thread: {err}")))?;

        match until_cancelled(reply_rx.recv_async(), &cancel).await {
            Some(Ok(outcome)) => outcome,
            Some(Err(_)) => Err(BiometricError::Backend(
                "keychain thread exited without a reply".to_owned(),
            )),
            None => {
                context.invalidate();
                Err(BiometricError::SystemCancelled)
            }
        }
    }

    #[allow(clippy::unused_async)] // Async like every platform backend.
    pub(super) async fn delete_secret(
        &self,
        _options: &DesktopOptions,
        alias: &SecretAlias,
    ) -> Result<(), BiometricError> {
        VaultQuery::item(alias).delete()
    }

    #[allow(clippy::unused_async)] // Async like every platform backend.
    pub(super) async fn has_secret(
        &self,
        _options: &DesktopOptions,
        alias: &SecretAlias,
    ) -> Result<bool, BiometricError> {
        let context = SharedContext::new();
        context.non_interactive();
        VaultQuery::item(alias).authenticated_by(&context).exists()
    }

    /// `LADomainState` biometry hash (macOS 15+), or the equivalent
    /// `evaluatedPolicyDomainState` on older releases.
    #[allow(clippy::unused_async)] // Async like every platform backend.
    pub(super) async fn enrollment_state(
        &self,
        _options: &DesktopOptions,
    ) -> Result<Option<Vec<u8>>, BiometricError> {
        // SAFETY: `LAContext` has no initialisation preconditions.
        let context = unsafe { LAContext::new() };
        // The domain state is only populated after a successful
        // biometric `canEvaluatePolicy`.
        if can_evaluate(&context, LAPolicy::DeviceOwnerAuthenticationWithBiometrics).is_err() {
            return Ok(None);
        }
        let hash = if objc2::available!(macos = 15.0) {
            // SAFETY: property reads on a live context; `domainState`
            // exists from macOS 15, checked above.
            unsafe { context.domainState().biometry().stateHash() }
        } else {
            // SAFETY: property read on a live context.
            #[allow(deprecated)] // Replaced by `domainState` on macOS 15+.
            unsafe {
                context.evaluatedPolicyDomainState()
            }
        };
        Ok(hash.map(|data| data.to_vec()))
    }
}

/// One `evaluatePolicy` round-trip bound to the thread that owns its
/// `LAContext`.
struct Evaluation {
    context: Retained<LAContext>,
    policy: AuthPolicy,
    reason: Retained<NSString>,
}

impl Evaluation {
    fn new(prompt: &AuthPrompt) -> Self {
        // SAFETY: `LAContext` has no initialisation preconditions.
        let context = unsafe { LAContext::new() };
        if let Some(label) = &prompt.cancel_label {
            // SAFETY: property setter on a live context.
            unsafe { context.setLocalizedCancelTitle(Some(&NSString::from_str(label))) };
        }
        if let Some(label) = &prompt.fallback_label {
            // SAFETY: property setter on a live context; an empty string
            // hides the fallback button, as documented by Apple.
            unsafe { context.setLocalizedFallbackTitle(Some(&NSString::from_str(label))) };
        }
        Self {
            context,
            policy: prompt.policy,
            reason: NSString::from_str(&prompt.reason),
        }
    }

    fn run(
        self,
        reply: &flume::Sender<Result<AuthMethod, BiometricError>>,
        cancel: &flume::Receiver<()>,
    ) {
        let (done_tx, done_rx) = flume::bounded(1);
        let block = RcBlock::new(move |success: Bool, error: *mut NSError| {
            let outcome = if success.as_bool() {
                Ok(())
            } else {
                // SAFETY: LocalAuthentication passes either null or a
                // valid `NSError` that outlives the reply block call.
                Err(unsafe { error.as_ref() }
                    .map_or(LaErrorCode(LAError::AuthenticationFailed.0), |e| {
                        LaErrorCode(e.code())
                    }))
            };
            let _ = done_tx.send(outcome);
        });
        // SAFETY: the reply block only captures a `Send` channel sender,
        // as required by `evaluatePolicy:localizedReason:reply:`.
        unsafe {
            self.context.evaluatePolicy_localizedReason_reply(
                la_policy(self.policy),
                &self.reason,
                &block,
            );
        }

        let finished = flume::Selector::new()
            .recv(&done_rx, Result::ok)
            .recv(cancel, |_| None)
            .wait();
        let Some(outcome) = finished else {
            // SAFETY: invalidating a live context is always allowed; the
            // pending evaluation then replies with `LAErrorAppCancel`.
            unsafe { self.context.invalidate() };
            let _ = done_rx.recv();
            return;
        };
        let _ = reply.send(match outcome {
            Ok(()) => Ok(match self.policy {
                AuthPolicy::BiometricStrong | AuthPolicy::BiometricWeak => AuthMethod::Biometric,
                AuthPolicy::BiometricOrDeviceCredential => AuthMethod::Unspecified,
            }),
            Err(code) => Err(code.into()),
        });
    }
}

const fn la_policy(policy: AuthPolicy) -> LAPolicy {
    match policy {
        AuthPolicy::BiometricStrong | AuthPolicy::BiometricWeak => {
            LAPolicy::DeviceOwnerAuthenticationWithBiometrics
        }
        AuthPolicy::BiometricOrDeviceCredential => LAPolicy::DeviceOwnerAuthentication,
    }
}

fn can_evaluate(context: &LAContext, policy: LAPolicy) -> Result<(), LaErrorCode> {
    // SAFETY: preflight check on a live context; never shows UI.
    unsafe { context.canEvaluatePolicy_error(policy) }.map_err(|err| LaErrorCode(err.code()))
}

/// `NSError.code` in the `LAErrorDomain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LaErrorCode(isize);

impl LaErrorCode {
    const fn status(self) -> BiometricStatus {
        match LAError(self.0) {
            LAError::PasscodeNotSet | LAError::BiometryNotEnrolled => BiometricStatus::NoneEnrolled,
            LAError::BiometryNotAvailable => BiometricStatus::NoHardware,
            LAError::BiometryLockout => BiometricStatus::LockedOut,
            _ => BiometricStatus::HardwareUnavailable,
        }
    }
}

impl From<LaErrorCode> for BiometricError {
    fn from(code: LaErrorCode) -> Self {
        match LAError(code.0) {
            LAError::AuthenticationFailed => Self::AuthFailed,
            LAError::UserCancel => Self::UserCancelled,
            LAError::UserFallback => Self::UserFallback,
            LAError::SystemCancel | LAError::AppCancel => Self::SystemCancelled,
            // Touch ID lockout lifts only after the login password.
            LAError::BiometryLockout => Self::LockedOutPermanent,
            LAError::PasscodeNotSet
            | LAError::BiometryNotEnrolled
            | LAError::BiometryNotAvailable
            | LAError::BiometryNotPaired
            | LAError::BiometryDisconnected => Self::NotAvailable(code.status()),
            LAError::NotInteractive => {
                Self::Backend("LocalAuthentication refused a non-interactive session".to_owned())
            }
            other => Self::Backend(format!("LocalAuthentication error {}", other.0)),
        }
    }
}
