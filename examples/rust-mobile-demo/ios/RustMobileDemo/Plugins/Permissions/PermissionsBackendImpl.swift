import Contacts
import CoreLocation
import EventKit
import Foundation
import IstmoRuntime
import Photos
import UserNotifications
import AVFoundation

public final class PermissionsBackendImpl: NSObject, PermissionsBackend, CLLocationManagerDelegate {

    private let locationManager = CLLocationManager()
    private let locationLock = NSLock()
    private var locationContinuation: CheckedContinuation<PermissionStatus, Never>?

    public override init() {
        super.init()
        locationManager.delegate = self
    }

    public func check(permission: String) async throws -> PermissionStatus {
        switch permission {
        case "android.permission.POST_NOTIFICATIONS":
            return await notificationStatus()
        case "android.permission.CAMERA":
            return Self.map(AVCaptureDevice.authorizationStatus(for: .video))
        case "android.permission.RECORD_AUDIO":
            return Self.map(AVCaptureDevice.authorizationStatus(for: .audio))
        case "android.permission.ACCESS_FINE_LOCATION",
             "android.permission.ACCESS_COARSE_LOCATION":
            return Self.mapLocation(CLLocationManager().authorizationStatus)
        case "android.permission.READ_MEDIA_IMAGES",
             "android.permission.READ_MEDIA_VIDEO":
            return Self.mapPhotos(PHPhotoLibrary.authorizationStatus(for: .readWrite))
        case "android.permission.READ_CONTACTS",
             "android.permission.WRITE_CONTACTS":
            return Self.mapContacts(CNContactStore.authorizationStatus(for: .contacts))
        case "android.permission.READ_CALENDAR",
             "android.permission.WRITE_CALENDAR":
            return Self.mapCalendar(EKEventStore.authorizationStatus(for: .event))
        default:
            return .notSupported
        }
    }

    public func request(permissions: [String]) async throws -> [PermissionOutcome] {
        var out: [PermissionOutcome] = []
        out.reserveCapacity(permissions.count)
        for permission in permissions {
            let status = await requestOne(permission)
            out.append(PermissionOutcome(permission: permission, status: status))
        }
        return out
    }

    public func should_show_rationale(permission: String) async throws -> Bool {
        false
    }

    private func requestOne(_ permission: String) async -> PermissionStatus {
        switch permission {
        case "android.permission.POST_NOTIFICATIONS":
            return await requestNotifications()
        case "android.permission.CAMERA":
            return await requestAVMedia(.video)
        case "android.permission.RECORD_AUDIO":
            return await requestAVMedia(.audio)
        case "android.permission.ACCESS_FINE_LOCATION",
             "android.permission.ACCESS_COARSE_LOCATION":
            return await requestLocation()
        case "android.permission.READ_MEDIA_IMAGES",
             "android.permission.READ_MEDIA_VIDEO":
            return await requestPhotos()
        case "android.permission.READ_CONTACTS",
             "android.permission.WRITE_CONTACTS":
            return await requestContacts()
        case "android.permission.READ_CALENDAR",
             "android.permission.WRITE_CALENDAR":
            return await requestCalendar()
        default:
            return .notSupported
        }
    }

    private func notificationStatus() async -> PermissionStatus {
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        return Self.mapNotifications(settings.authorizationStatus)
    }

    private func requestNotifications() async -> PermissionStatus {
        do {
            let granted = try await UNUserNotificationCenter.current()
                .requestAuthorization(options: [.alert, .badge, .sound])
            if granted { return .granted }
            return await notificationStatus()
        } catch {
            NSLog("PermissionsBackendImpl: requestAuthorization threw: \(error)")
            return .denied
        }
    }

    private func requestAVMedia(_ mediaType: AVMediaType) async -> PermissionStatus {

        switch AVCaptureDevice.authorizationStatus(for: mediaType) {
        case .authorized: return .granted
        case .denied, .restricted: return .permanentlyDenied
        case .notDetermined: break
        @unknown default: return .notSupported
        }
        let granted = await AVCaptureDevice.requestAccess(for: mediaType)
        return granted ? .granted : .permanentlyDenied
    }

    private func requestLocation() async -> PermissionStatus {
        switch locationManager.authorizationStatus {
        case .authorizedAlways, .authorizedWhenInUse: return .granted
        case .denied, .restricted: return .permanentlyDenied
        case .notDetermined: break
        @unknown default: return .notSupported
        }
        return await withCheckedContinuation { (cont: CheckedContinuation<PermissionStatus, Never>) in
            locationLock.lock()
            locationContinuation = cont
            locationLock.unlock()
            locationManager.requestWhenInUseAuthorization()
        }
    }

    public func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        let status = Self.mapLocation(manager.authorizationStatus)

        if manager.authorizationStatus == .notDetermined { return }
        locationLock.lock()
        let cont = locationContinuation
        locationContinuation = nil
        locationLock.unlock()
        cont?.resume(returning: status)
    }

    private func requestPhotos() async -> PermissionStatus {
        let current = PHPhotoLibrary.authorizationStatus(for: .readWrite)
        if current != .notDetermined { return Self.mapPhotos(current) }
        let next = await PHPhotoLibrary.requestAuthorization(for: .readWrite)
        return Self.mapPhotos(next)
    }

    private func requestContacts() async -> PermissionStatus {
        let current = CNContactStore.authorizationStatus(for: .contacts)
        if current != .notDetermined { return Self.mapContacts(current) }
        return await withCheckedContinuation { (cont: CheckedContinuation<PermissionStatus, Never>) in
            CNContactStore().requestAccess(for: .contacts) { granted, error in
                if let error = error {
                    NSLog("PermissionsBackendImpl: contacts request error: \(error)")
                }
                cont.resume(returning: granted ? .granted : .permanentlyDenied)
            }
        }
    }

    private func requestCalendar() async -> PermissionStatus {
        let current = EKEventStore.authorizationStatus(for: .event)
        if current != .notDetermined { return Self.mapCalendar(current) }
        return await withCheckedContinuation { (cont: CheckedContinuation<PermissionStatus, Never>) in
            EKEventStore().requestAccess(to: .event) { granted, error in
                if let error = error {
                    NSLog("PermissionsBackendImpl: calendar request error: \(error)")
                }
                cont.resume(returning: granted ? .granted : .permanentlyDenied)
            }
        }
    }

    private static func map(_ status: AVAuthorizationStatus) -> PermissionStatus {
        switch status {
        case .authorized: return .granted
        case .denied: return .permanentlyDenied
        case .restricted: return .permanentlyDenied
        case .notDetermined: return .notDetermined
        @unknown default: return .notSupported
        }
    }

    private static func mapNotifications(_ status: UNAuthorizationStatus) -> PermissionStatus {
        switch status {
        case .authorized, .provisional, .ephemeral: return .granted
        case .denied: return .permanentlyDenied
        case .notDetermined: return .notDetermined
        @unknown default: return .notSupported
        }
    }

    private static func mapLocation(_ status: CLAuthorizationStatus) -> PermissionStatus {
        switch status {
        case .authorizedAlways, .authorizedWhenInUse: return .granted
        case .denied, .restricted: return .permanentlyDenied
        case .notDetermined: return .notDetermined
        @unknown default: return .notSupported
        }
    }

    private static func mapPhotos(_ status: PHAuthorizationStatus) -> PermissionStatus {
        switch status {
        case .authorized, .limited: return .granted
        case .denied, .restricted: return .permanentlyDenied
        case .notDetermined: return .notDetermined
        @unknown default: return .notSupported
        }
    }

    private static func mapContacts(_ status: CNAuthorizationStatus) -> PermissionStatus {
        switch status {
        case .authorized: return .granted
        case .denied, .restricted: return .permanentlyDenied
        case .notDetermined: return .notDetermined
        @unknown default: return .notSupported
        }
    }

    private static func mapCalendar(_ status: EKAuthorizationStatus) -> PermissionStatus {
        switch status {
        case .authorized, .fullAccess, .writeOnly: return .granted
        case .denied, .restricted: return .permanentlyDenied
        case .notDetermined: return .notDetermined
        @unknown default: return .notSupported
        }
    }
}

