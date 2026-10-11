// System framework calls live in a separate process; Rust never crosses an unsafe FFI boundary.
import Foundation
import Photos
import CoreImage
import ImageIO
import Vision
import UniformTypeIdentifiers
import CryptoKit
import Darwin

struct BridgeError: LocalizedError {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}

struct Request: Decodable {
    let operation: String
    var path: String?
    var output: String?
    var maxEdge: Int?
    var width: Int?
    var height: Int?
    var side: Int?
    var offset: Int?
    var limit: Int?
    var album: String?
    var ids: [String]?
    var network: Bool?
}

func required(_ value: String?, _ name: String) throws -> String {
    guard let value, !value.isEmpty else { throw BridgeError("Missing \(name)") }
    return value
}

func authorizationName(_ status: PHAuthorizationStatus) -> String {
    switch status {
    case .authorized: return "authorized"
    case .limited: return "limited"
    case .denied: return "denied"
    case .restricted: return "restricted"
    case .notDetermined: return "notDetermined"
    @unknown default: return "unknown"
    }
}

func authorizePhotos() async throws -> PHAuthorizationStatus {
    var status = PHPhotoLibrary.authorizationStatus(for: .readWrite)
    if status == .notDetermined {
        status = await PHPhotoLibrary.requestAuthorization(for: .readWrite)
    }
    guard status == .authorized || status == .limited else {
        throw BridgeError("Apple Photos access is \(authorizationName(status)). Allow LightCraft in System Settings > Privacy & Security > Photos, then retry.")
    }
    return status
}

func originalResource(_ asset: PHAsset) -> PHAssetResource? {
    // RAW+JPEG pairs can store the RAW as alternatePhoto. Never export an adjusted/fullSizePhoto.
    let originals = PHAssetResource.assetResources(for: asset).filter { $0.type == .photo || $0.type == .alternatePhoto }
    return originals.first { UTType($0.uniformTypeIdentifier)?.conforms(to: .rawImage) == true }
        ?? originals.first { $0.type == .photo }
}

func listPhotos(_ request: Request) async throws -> [String: Any] {
    let status = try await authorizePhotos()
    let options = PHFetchOptions()
    options.sortDescriptors = [NSSortDescriptor(key: "creationDate", ascending: false)]
    options.predicate = NSPredicate(format: "mediaType = %d", PHAssetMediaType.image.rawValue)
    let assets: PHFetchResult<PHAsset>
    if let album = request.album, !album.isEmpty {
        guard let collection = PHAssetCollection.fetchAssetCollections(withLocalIdentifiers: [album], options: nil).firstObject else {
            throw BridgeError("Apple Photos album is no longer available")
        }
        assets = PHAsset.fetchAssets(in: collection, options: options)
    } else {
        assets = PHAsset.fetchAssets(with: .image, options: options)
    }
    let start = min(max(request.offset ?? 0, 0), assets.count)
    let end = min(start + min(max(request.limit ?? 100, 1), 200), assets.count)
    let formatter = ISO8601DateFormatter()
    var rows: [[String: Any]] = []
    for i in start..<end {
        let asset = assets.object(at: i)
        guard let resource = originalResource(asset) else { continue }
        var row: [String: Any] = ["id": asset.localIdentifier, "name": resource.originalFilename,
            "width": asset.pixelWidth, "height": asset.pixelHeight, "favorite": asset.isFavorite,
            "raw": UTType(resource.uniformTypeIdentifier)?.conforms(to: .rawImage) == true]
        if let date = asset.creationDate { row["captured"] = formatter.string(from: date) }
        rows.append(row)
    }
    var albums: [[String: String]] = []
    let collections = PHAssetCollection.fetchAssetCollections(with: .album, subtype: .any, options: nil)
    for i in 0..<min(collections.count, 1000) {
        let album = collections.object(at: i)
        albums.append(["id": album.localIdentifier, "name": album.localizedTitle ?? "Album"])
    }
    return ["authorization": authorizationName(status), "total": assets.count, "offset": start,
        "nextOffset": end, "assets": rows, "albums": albums]
}

func exportPhotos(_ request: Request) async throws -> [String: Any] {
    _ = try await authorizePhotos()
    guard let ids = request.ids, !ids.isEmpty, ids.count <= 200 else {
        throw BridgeError("Select between 1 and 200 Apple Photos assets")
    }
    let destination = URL(fileURLWithPath: try required(request.output, "output"), isDirectory: true)
    try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
    let assets = PHAsset.fetchAssets(withLocalIdentifiers: ids, options: nil)
    var paths: [String] = []
    var errors: [[String: String]] = []
    for id in ids {
        var selected: PHAsset?
        assets.enumerateObjects { asset, _, stop in
            if asset.localIdentifier == id { selected = asset; stop.pointee = true }
        }
        guard let asset = selected, let resource = originalResource(asset) else {
            errors.append(["id": id, "error": "Asset or original is no longer available"])
            continue
        }
        let digest = SHA256.hash(data: Data((id + "|" + resource.originalFilename + "|" + resource.uniformTypeIdentifier).utf8))
            .map { String(format: "%02x", $0) }.joined()
        let dir = destination.appendingPathComponent(digest, isDirectory: true)
        let name = (resource.originalFilename as NSString).lastPathComponent
        guard !name.isEmpty, name != ".", name != ".." else {
            errors.append(["id": id, "error": "Invalid original filename"]); continue
        }
        let file = dir.appendingPathComponent(name)
        let marker = dir.appendingPathComponent(".complete.json")
        let temporary = dir.appendingPathComponent(".download-" + UUID().uuidString)
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            // A completion marker is written only after PhotoKit has finished and the rename succeeds.
            if !FileManager.default.fileExists(atPath: marker.path) || !FileManager.default.fileExists(atPath: file.path) {
                let options = PHAssetResourceRequestOptions()
                options.isNetworkAccessAllowed = request.network ?? false
                try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                    PHAssetResourceManager.default().writeData(for: resource, toFile: temporary, options: options) { error in
                        if let error { continuation.resume(throwing: error) } else { continuation.resume() }
                    }
                }
                if FileManager.default.fileExists(atPath: file.path) { try FileManager.default.removeItem(at: file) }
                try FileManager.default.moveItem(at: temporary, to: file)
                let stamp = try JSONSerialization.data(withJSONObject: ["id": id, "originalFilename": name])
                try stamp.write(to: marker, options: .atomic)
            }
            paths.append(file.path)
        } catch {
            try? FileManager.default.removeItem(at: temporary)
            errors.append(["id": id, "error": error.localizedDescription])
        }
    }
    return ["paths": paths, "errors": errors]
}

@available(macOS 12.0, *)
func rawFilter(_ path: String) throws -> CIRAWFilter {
    guard let filter = CIRAWFilter(imageURL: URL(fileURLWithPath: path)) else {
        throw BridgeError("Apple RAW cannot decode this camera/file on this macOS version")
    }
    filter.exposure = 0
    filter.boostAmount = 0
    filter.boostShadowAmount = 0
    if filter.isLocalToneMapSupported { filter.localToneMapAmount = 0 }
    if filter.isLuminanceNoiseReductionSupported { filter.luminanceNoiseReductionAmount = 0 }
    if filter.isColorNoiseReductionSupported { filter.colorNoiseReductionAmount = 0 }
    if filter.isSharpnessSupported { filter.sharpnessAmount = 0 }
    if filter.isDetailSupported { filter.detailAmount = 0 }
    if filter.isLensCorrectionSupported { filter.isLensCorrectionEnabled = false }
    filter.isGamutMappingEnabled = false
    return filter
}

func checkedDimensions(_ width: Int, _ height: Int, limit: Int) throws -> Int {
    guard width > 0, height > 0, width <= limit, height <= limit,
          width <= limit / height else { throw BridgeError("Invalid or oversized image dimensions") }
    return width * height
}

@available(macOS 12.0, *)
func raw(_ request: Request) throws -> [String: Any] {
    let filter = try rawFilter(required(request.path, "path"))
    if request.operation == "raw-probe" {
        let swaps = [CGImagePropertyOrientation.left, .leftMirrored, .right, .rightMirrored].contains(filter.orientation)
        let size = filter.nativeSize
        return ["width": Int(swaps ? size.height : size.width), "height": Int(swaps ? size.width : size.height),
            "temperature": filter.neutralTemperature, "tint": filter.neutralTint]
    }
    let edge = request.maxEdge ?? 16384
    guard edge > 0 else { throw BridgeError("Invalid RAW preview size") }
    filter.scaleFactor = Float(min(1, Double(edge) / max(filter.nativeSize.width, filter.nativeSize.height)))
    guard let image = filter.outputImage else { throw BridgeError("Apple RAW produced no image") }
    let bounds = image.extent.integral
    let w = Int(bounds.width), h = Int(bounds.height)
    let count = try checkedDimensions(w, h, limit: 100_000_000)
    guard let space = CGColorSpace(name: CGColorSpace.extendedLinearSRGB) else { throw BridgeError("Linear sRGB is unavailable") }
    let context = CIContext(options: [.cacheIntermediates: false])
    var pixels = [Float](repeating: 0, count: count * 4)
    pixels.withUnsafeMutableBytes { buffer in
        if let address = buffer.baseAddress {
            context.render(image, toBitmap: address, rowBytes: w * 16, bounds: bounds, format: .RGBAf, colorSpace: space)
        }
    }
    try pixels.withUnsafeBytes { buffer in
        try Data(buffer).write(to: URL(fileURLWithPath: try required(request.output, "output")), options: .atomic)
    }
    return ["width": w, "height": h, "temperature": filter.neutralTemperature, "tint": filter.neutralTint,
        "colorSpace": "extended-linear-sRGB", "decoder": "Apple Core Image RAW"]
}

@available(macOS 14.0, *)
func foregroundMask(_ request: Request) throws -> [String: Any] {
    let width = request.width ?? 0, height = request.height ?? 0, side = request.side ?? 512
    let count = try checkedDimensions(width, height, limit: 10_000_000)
    _ = try checkedDimensions(side, side, limit: 1_048_576)
    let data = try Data(contentsOf: URL(fileURLWithPath: try required(request.path, "path")))
    guard data.count == count * 16, let space = CGColorSpace(name: CGColorSpace.extendedLinearITUR_2020) else {
        throw BridgeError("Invalid Vision input")
    }
    let image = CIImage(bitmapData: data, bytesPerRow: width * 16, size: CGSize(width: width, height: height), format: .RGBAf, colorSpace: space)
    let handler = VNImageRequestHandler(ciImage: image, orientation: .up)
    let requestMask = VNGenerateForegroundInstanceMaskRequest()
    try handler.perform([requestMask])
    guard let observation = requestMask.results?.first, !observation.allInstances.isEmpty else {
        throw BridgeError("Apple Vision found no foreground subject in this photo")
    }
    let buffer = try observation.generateScaledMaskForImage(forInstances: observation.allInstances, from: handler)
    let mask = CIImage(cvPixelBuffer: buffer)
    let scaled = mask.transformed(by: CGAffineTransform(scaleX: CGFloat(side) / mask.extent.width, y: CGFloat(side) / mask.extent.height))
    let context = CIContext(options: [.cacheIntermediates: false])
    var coverage = [Float](repeating: 0, count: side * side)
    coverage.withUnsafeMutableBytes { bytes in
        if let address = bytes.baseAddress {
            context.render(scaled, toBitmap: address, rowBytes: side * 4, bounds: CGRect(x: 0, y: 0, width: side, height: side), format: .Rf, colorSpace: nil)
        }
    }
    try coverage.withUnsafeBytes { bytes in
        try Data(bytes).write(to: URL(fileURLWithPath: try required(request.output, "output")), options: .atomic)
    }
    return ["side": side, "provider": "Apple Vision"]
}

@main struct AppleBridge {
    static func main() async {
        // A UI being closed must not leave a PhotoKit download or permission waiter orphaned.
        let parent = getppid()
        let watchdog = DispatchSource.makeTimerSource(queue: .global())
        watchdog.schedule(deadline: .now() + 1, repeating: 1)
        watchdog.setEventHandler { if getppid() != parent { exit(2) } }
        watchdog.resume()
        defer { watchdog.cancel() }
        do {
            let input = FileHandle.standardInput.readData(ofLength: 65_537)
            guard input.count <= 65_536 else { throw BridgeError("Oversized bridge request") }
            let request = try JSONDecoder().decode(Request.self, from: input)
            let result: [String: Any]
            switch request.operation {
            case "health":
                result = ["protocol": 1, "photos": authorizationName(PHPhotoLibrary.authorizationStatus(for: .readWrite)),
                    "raw": ProcessInfo.processInfo.isOperatingSystemAtLeast(OperatingSystemVersion(majorVersion: 12, minorVersion: 0, patchVersion: 0)),
                    "vision": ProcessInfo.processInfo.isOperatingSystemAtLeast(OperatingSystemVersion(majorVersion: 14, minorVersion: 0, patchVersion: 0))]
            case "photos-list": result = try await listPhotos(request)
            case "photos-export": result = try await exportPhotos(request)
            case "raw-probe", "raw-decode":
                guard #available(macOS 12.0, *) else { throw BridgeError("Apple RAW requires macOS 12 or later") }
                result = try raw(request)
            case "vision-mask":
                guard #available(macOS 14.0, *) else { throw BridgeError("Apple Vision foreground masks require macOS 14 or later") }
                result = try foregroundMask(request)
            default: throw BridgeError("Unknown Apple bridge operation")
            }
            let response = try JSONSerialization.data(withJSONObject: result)
            guard response.count <= 192_000 else { throw BridgeError("Photos page is too large; request fewer photos") }
            try FileHandle.standardOutput.write(contentsOf: response)
        } catch {
            let result = ["error": error.localizedDescription]
            if let data = try? JSONSerialization.data(withJSONObject: result) { try? FileHandle.standardOutput.write(contentsOf: data) }
            exit(1)
        }
    }
}
