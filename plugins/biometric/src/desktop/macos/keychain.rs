//! Data-protection keychain items guarded by `SecAccessControl`, used as
//! the vault for biometric-bound secrets.

use std::ptr;
use std::sync::Arc;

use core_foundation::base::{CFType, OSStatus, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use objc2::rc::Retained;
use objc2_foundation::NSString;
use objc2_local_authentication::LAContext;
use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework_sys::access_control::{
    kSecAccessControlBiometryCurrentSet, kSecAccessControlUserPresence,
};
use security_framework_sys::base::{errSecAuthFailed, errSecItemNotFound, errSecSuccess};
use security_framework_sys::item::{
    kSecAttrAccessControl, kSecAttrAccount, kSecAttrService, kSecClass, kSecClassGenericPassword,
    kSecReturnAttributes, kSecReturnData, kSecUseAuthenticationContext,
    kSecUseDataProtectionKeychain, kSecValueData,
};
use security_framework_sys::keychain_item::{SecItemAdd, SecItemCopyMatching, SecItemDelete};

use crate::{AuthPolicy, AuthPrompt, BiometricError, SecretAlias};

/// `kSecAttrService` of every vault item.
const KEYCHAIN_SERVICE: &str = "dev.istmo.biometric";
const ERR_SEC_SUCCESS: OSStatus = errSecSuccess;
const ERR_SEC_ITEM_NOT_FOUND: OSStatus = errSecItemNotFound;
const ERR_SEC_AUTH_FAILED: OSStatus = errSecAuthFailed;
/// `OSStatus` values missing from `security-framework-sys`
/// (`<Security/SecBase.h>`).
const ERR_SEC_USER_CANCELED: OSStatus = -128;
const ERR_SEC_INTERACTION_NOT_ALLOWED: OSStatus = -25308;
const ERR_SEC_MISSING_ENTITLEMENT: OSStatus = -34018;

/// `LAContext` reachable from both the thread blocked in the keychain
/// and the one that may cancel it.
pub(super) struct SharedContext(Retained<LAContext>);

// SAFETY: `LAContext` is thread-safe — the macOS 14 SDK declares it
// `NS_SWIFT_SENDABLE`, and `invalidate` exists precisely to abort an
// evaluation running on another thread.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for SharedContext {}
// SAFETY: see `Send` above.
unsafe impl Sync for SharedContext {}

impl SharedContext {
    pub(super) fn new() -> Arc<Self> {
        // SAFETY: `LAContext` has no initialisation preconditions.
        Arc::new(Self(unsafe { LAContext::new() }))
    }

    pub(super) fn configure(&self, prompt: &AuthPrompt) {
        // SAFETY: property setters on a live context.
        unsafe {
            self.0
                .setLocalizedReason(&NSString::from_str(&prompt.reason));
            if let Some(label) = &prompt.cancel_label {
                self.0
                    .setLocalizedCancelTitle(Some(&NSString::from_str(label)));
            }
            if let Some(label) = &prompt.fallback_label {
                self.0
                    .setLocalizedFallbackTitle(Some(&NSString::from_str(label)));
            }
        }
    }

    /// Make every evaluation fail instead of showing UI.
    pub(super) fn non_interactive(&self) {
        // SAFETY: property setter on a live context.
        unsafe { self.0.setInteractionNotAllowed(true) };
    }

    pub(super) fn invalidate(&self) {
        // SAFETY: invalidating a live context is always allowed.
        unsafe { self.0.invalidate() };
    }

    fn as_cf_type(&self) -> CFType {
        // SAFETY: every Objective-C object is a valid `CFTypeRef`
        // (toll-free bridging of `NSObject`); the get rule retains it.
        unsafe { CFType::wrap_under_get_rule(Retained::as_ptr(&self.0).cast()) }
    }
}

/// Keychain query addressing one vault item.
pub(super) struct VaultQuery(Vec<(CFString, CFType)>);

impl VaultQuery {
    /// Every vault item of this app.
    fn service() -> Self {
        // SAFETY: the `kSec*` constants are immutable statics exported by
        // Security.framework; reading them is always sound.
        unsafe {
            Self(Vec::new())
                .with(
                    kSecClass,
                    CFString::wrap_under_get_rule(kSecClassGenericPassword),
                )
                .with(kSecAttrService, CFString::new(KEYCHAIN_SERVICE))
                .with(kSecUseDataProtectionKeychain, CFBoolean::true_value())
        }
    }

    pub(super) fn item(alias: &SecretAlias) -> Self {
        // SAFETY: immutable Security.framework constant.
        Self::service().with(unsafe { kSecAttrAccount }, CFString::new(alias.as_str()))
    }

    /// Whether this process may use the data-protection keychain at all:
    /// unsigned binaries and apps without `keychain-access-groups` get
    /// `errSecMissingEntitlement`. Never shows UI.
    pub(super) fn keychain_usable() -> bool {
        let context = SharedContext::new();
        context.non_interactive();
        !matches!(
            Self::service().authenticated_by(&context).exists(),
            Err(BiometricError::UnsupportedOperation(_))
        )
    }

    /// Evaluate the item's access control with `context`, which carries
    /// the prompt texts and can be invalidated to cancel.
    pub(super) fn authenticated_by(self, context: &SharedContext) -> Self {
        // SAFETY: immutable Security.framework constant.
        self.with(
            unsafe { kSecUseAuthenticationContext },
            context.as_cf_type(),
        )
    }

    fn with(mut self, key: CFStringRef, value: impl TCFType) -> Self {
        // SAFETY: `key` is one of the static `kSec*` constants.
        let key = unsafe { CFString::wrap_under_get_rule(key) };
        self.0.push((key, value.into_CFType()));
        self
    }

    fn dictionary(&self) -> CFDictionary<CFString, CFType> {
        CFDictionary::from_CFType_pairs(&self.0)
    }

    /// Store `secret` behind an access control matching `policy`.
    pub(super) fn add(self, secret: &[u8], policy: AuthPolicy) -> Result<(), BiometricError> {
        let flags = match policy {
            AuthPolicy::BiometricStrong | AuthPolicy::BiometricWeak => {
                kSecAccessControlBiometryCurrentSet
            }
            AuthPolicy::BiometricOrDeviceCredential => kSecAccessControlUserPresence,
        };
        let access = SecAccessControl::create_with_protection(
            Some(ProtectionMode::AccessibleWhenPasscodeSetThisDeviceOnly),
            flags,
        )
        .map_err(|err| BiometricError::Backend(format!("SecAccessControl: {err}")))?;
        // SAFETY: immutable Security.framework constants.
        let query = unsafe {
            self.with(kSecValueData, CFData::from_buffer(secret))
                .with(kSecAttrAccessControl, access)
        };
        // SAFETY: the dictionary is a valid add query; no result is
        // requested.
        let status =
            unsafe { SecItemAdd(query.dictionary().as_concrete_TypeRef(), ptr::null_mut()) };
        KeychainStatus(status).into_result()
    }

    pub(super) fn delete(&self) -> Result<(), BiometricError> {
        // SAFETY: the dictionary is a valid query.
        let status = unsafe { SecItemDelete(self.dictionary().as_concrete_TypeRef()) };
        match status {
            ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            other => KeychainStatus(other).into_result(),
        }
    }

    pub(super) fn copy_data(self) -> Result<Vec<u8>, BiometricError> {
        // SAFETY: immutable Security.framework constant.
        let query = self.with(unsafe { kSecReturnData }, CFBoolean::true_value());
        let mut result = ptr::null();
        // SAFETY: the query asks for `kSecReturnData`, so on success
        // `result` holds a +1 `CFData` we take ownership of below.
        let status = unsafe {
            SecItemCopyMatching(query.dictionary().as_concrete_TypeRef(), &raw mut result)
        };
        KeychainStatus(status).into_result()?;
        if result.is_null() {
            return Err(BiometricError::Backend(
                "keychain returned no data".to_owned(),
            ));
        }
        // SAFETY: a non-null result is an owned `CFData`.
        let data = unsafe { CFData::wrap_under_create_rule(result.cast()) };
        Ok(data.bytes().to_vec())
    }

    /// Probe for the item without reading it. The context must be
    /// [`SharedContext::non_interactive`] so no UI shows up.
    pub(super) fn exists(self) -> Result<bool, BiometricError> {
        // SAFETY: immutable Security.framework constant.
        let query = self.with(unsafe { kSecReturnAttributes }, CFBoolean::true_value());
        let mut result = ptr::null();
        // SAFETY: the query asks for `kSecReturnAttributes`; a returned
        // dictionary is +1 and released right away.
        let status = unsafe {
            SecItemCopyMatching(query.dictionary().as_concrete_TypeRef(), &raw mut result)
        };
        if !result.is_null() {
            // SAFETY: owned result of `SecItemCopyMatching`.
            drop(unsafe { CFType::wrap_under_create_rule(result) });
        }
        match status {
            // The item exists but reading it would need authentication.
            ERR_SEC_SUCCESS | ERR_SEC_INTERACTION_NOT_ALLOWED => Ok(true),
            ERR_SEC_ITEM_NOT_FOUND => Ok(false),
            other => KeychainStatus(other).into_result().map(|()| false),
        }
    }
}

/// `OSStatus` returned by a `SecItem*` call.
#[derive(Debug, Clone, Copy)]
struct KeychainStatus(OSStatus);

impl KeychainStatus {
    fn into_result(self) -> Result<(), BiometricError> {
        match self.0 {
            ERR_SEC_SUCCESS => Ok(()),
            ERR_SEC_ITEM_NOT_FOUND => Err(BiometricError::SecretNotFound),
            ERR_SEC_USER_CANCELED => Err(BiometricError::UserCancelled),
            ERR_SEC_AUTH_FAILED => Err(BiometricError::AuthFailed),
            ERR_SEC_INTERACTION_NOT_ALLOWED => Err(BiometricError::SystemCancelled),
            ERR_SEC_MISSING_ENTITLEMENT => Err(BiometricError::UnsupportedOperation(
                "the data-protection keychain needs a signed app with the \
                 `keychain-access-groups` entitlement"
                    .to_owned(),
            )),
            other => Err(BiometricError::Backend(format!(
                "keychain OSStatus {other}"
            ))),
        }
    }
}
