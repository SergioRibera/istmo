// Swift mirrors of the Rust `#[message]` types in `istmo-plugins/src/permissions.rs`.
//
// Codegen for `#[message]` → Swift `struct` / `enum` is a follow-up; for now
// these two shapes are hand-written. Ordering of `PermissionStatus` variants
// must match the Rust declaration (bincode encodes enum discriminants as
// u32 varints in declaration order).

import Foundation

public enum PermissionStatus: UInt32 {
    /// The user has granted the permission.
    case granted = 0
    /// The user has denied but the OS still allows re-prompting.
    case denied = 1
    /// The user has denied with "don't ask again", or policy blocks it.
    case permanentlyDenied = 2
    /// The permission has never been requested — no OS decision yet.
    case notDetermined = 3
    /// The permission id does not exist on this platform / OS version.
    case notSupported = 4
}

public struct PermissionOutcome {
    public let permission: String
    public let status: PermissionStatus

    public init(permission: String, status: PermissionStatus) {
        self.permission = permission
        self.status = status
    }
}
