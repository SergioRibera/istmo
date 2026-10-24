import Foundation
import IstmoRuntime
#if canImport(UIKit) && canImport(AVFoundation)
import AVFoundation
import UIKit
#endif

/// Reference `AVFoundation` backend for `istmo.barcode_scanner`.
///
/// Presents a full-screen camera over the top view controller and
/// resolves with the first code read. Needs `NSCameraUsageDescription`.
public final class BarcodeScannerBackendImpl: BarcodeScannerBackend {

    #if canImport(UIKit) && canImport(AVFoundation)
    public init() {}

    public func scan(config: ScanConfig) async throws -> ScannedCode? {
        guard AVCaptureDevice.default(for: .video) != nil else {
            throw ScanError.unavailable
        }
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            break
        case .notDetermined:
            let granted = await AVCaptureDevice.requestAccess(for: .video)
            if !granted { throw ScanError.permissionDenied }
        default:
            throw ScanError.permissionDenied
        }
        return try await withCheckedThrowingContinuation { cont in
            DispatchQueue.main.async {
                guard let root = Self.topViewController() else {
                    cont.resume(throwing: ScanError.backend("no root view controller to present from"))
                    return
                }
                let controller = ScannerViewController(config: config) { result in
                    cont.resume(with: result)
                }
                controller.modalPresentationStyle = .fullScreen
                root.present(controller, animated: true)
            }
        }
    }

    private static func topViewController() -> UIViewController? {
        var top: UIViewController? = UIApplication.shared.connectedScenes
            .compactMap { ($0 as? UIWindowScene)?.keyWindow?.rootViewController }
            .first
        while let presented = top?.presentedViewController {
            top = presented
        }
        return top
    }

    #else
    public init() {}

    public func scan(config: ScanConfig) async throws -> ScannedCode? {
        throw ScanError.unavailable
    }
    #endif
}

#if canImport(UIKit) && canImport(AVFoundation)
private final class ScannerViewController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    private let config: ScanConfig
    private var finish: ((Result<ScannedCode?, Error>) -> Void)?
    private let session = AVCaptureSession()
    private let sessionQueue = DispatchQueue(label: "dev.istmo.plugins.barcode-scanner.session")

    init(config: ScanConfig, finish: @escaping (Result<ScannedCode?, Error>) -> Void) {
        self.config = config
        self.finish = finish
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        guard
            let device = AVCaptureDevice.default(for: .video),
            let input = try? AVCaptureDeviceInput(device: device),
            session.canAddInput(input)
        else {
            complete(.failure(ScanError.unavailable))
            return
        }
        session.addInput(input)
        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else {
            complete(.failure(ScanError.backend("cannot attach metadata output")))
            return
        }
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        let wanted = config.formats.isEmpty
            ? [BarcodeFormat.qr, .aztec, .dataMatrix, .pdf417, .code128, .ean13]
            : config.formats
        output.metadataObjectTypes = wanted.map(Self.metadataType)
            .filter { output.availableMetadataObjectTypes.contains($0) }

        let preview = AVCaptureVideoPreviewLayer(session: session)
        preview.videoGravity = .resizeAspectFill
        preview.frame = view.bounds
        view.layer.addSublayer(preview)

        let frame = UIView()
        frame.layer.borderColor = UIColor.white.cgColor
        frame.layer.borderWidth = 3
        frame.layer.cornerRadius = 18
        frame.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(frame)

        let hint = UILabel()
        hint.text = config.prompt
        hint.textColor = .white
        hint.font = .preferredFont(forTextStyle: .headline)
        hint.textAlignment = .center
        hint.numberOfLines = 0
        hint.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(hint)

        let close = UIButton(type: .system)
        close.setTitle(NSLocalizedString("Cancel", comment: "Close the scanner"), for: .normal)
        close.setTitleColor(.white, for: .normal)
        close.titleLabel?.font = .preferredFont(forTextStyle: .body)
        close.addTarget(self, action: #selector(cancel), for: .touchUpInside)
        close.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(close)

        NSLayoutConstraint.activate([
            frame.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            frame.centerYAnchor.constraint(equalTo: view.centerYAnchor),
            frame.widthAnchor.constraint(equalTo: view.widthAnchor, multiplier: 0.66),
            frame.heightAnchor.constraint(equalTo: frame.widthAnchor),
            hint.bottomAnchor.constraint(equalTo: frame.topAnchor, constant: -24),
            hint.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            hint.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            close.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -24),
            close.centerXAnchor.constraint(equalTo: view.centerXAnchor),
        ])
        sessionQueue.async { [session] in session.startRunning() }
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        view.layer.sublayers?.compactMap { $0 as? AVCaptureVideoPreviewLayer }.forEach { $0.frame = view.bounds }
    }

    func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        guard
            let code = metadataObjects.compactMap({ $0 as? AVMetadataMachineReadableCodeObject }).first,
            let text = code.stringValue
        else { return }
        complete(.success(ScannedCode(text: text, format: Self.format(of: code.type))))
    }

    @objc private func cancel() {
        complete(.success(nil))
    }

    private func complete(_ result: Result<ScannedCode?, Error>) {
        guard let finish else { return }
        self.finish = nil
        sessionQueue.async { [session] in session.stopRunning() }
        dismiss(animated: true) { finish(result) }
    }

    private static func metadataType(_ format: BarcodeFormat) -> AVMetadataObject.ObjectType {
        switch format {
        case .qr: return .qr
        case .aztec: return .aztec
        case .dataMatrix: return .dataMatrix
        case .pdf417: return .pdf417
        case .code128: return .code128
        case .ean13: return .ean13
        }
    }

    private static func format(of type: AVMetadataObject.ObjectType) -> BarcodeFormat {
        switch type {
        case .aztec: return .aztec
        case .dataMatrix: return .dataMatrix
        case .pdf417: return .pdf417
        case .code128: return .code128
        case .ean13: return .ean13
        default: return .qr
        }
    }
}
#endif
