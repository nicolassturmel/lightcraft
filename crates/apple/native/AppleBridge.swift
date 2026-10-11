// System framework calls live in a separate process; Rust never crosses an unsafe FFI boundary.
import Foundation
import AppKit
import Photos
import CoreImage
import ImageIO
import Vision
import UniformTypeIdentifiers
import Darwin

struct BridgeError: LocalizedError {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}

// Remove temporary PhotoKit originals even if the Rust parent closes during a download.
final class TemporaryOriginals {
    static let shared = TemporaryOriginals()
    private let lock = NSLock()
    private var paths = Set<URL>()
    func track(_ path: URL) { lock.lock(); paths.insert(path); lock.unlock() }
    func forget(_ path: URL) { lock.lock(); paths.remove(path); lock.unlock() }
    func cleanup() {
        lock.lock(); let pending = paths; paths.removeAll(); lock.unlock()
        for path in pending { try? FileManager.default.removeItem(at: path) }
    }
}

struct Request: Decodable {
    var operation: String
    var path: String?
    var output: String?
    var maxEdge: Int?
    var width: Int?
    var height: Int?
    var side: Int?
    var paths: [String]?
    var network: Bool?
    var asset: String?
    var recipe: String?
    var points: [[Double]]?
    var exclude: [[Double]]?
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

// This is a transient metadata projection, never a second photo catalogue.
func snapshotPhotos(_ request: Request) async throws -> [String: Any] {
    let status = try await authorizePhotos()
    let destination = URL(fileURLWithPath: try required(request.output, "output"))
    guard FileManager.default.createFile(atPath: destination.path, contents: nil) else {
        throw BridgeError("Cannot create the Photos metadata response")
    }
    let file = try FileHandle(forWritingTo: destination)
    defer { try? file.close() }
    var bytes = 0
    func write(_ data: Data) throws {
        bytes += data.count
        guard bytes <= 128_000_000 else { throw BridgeError("Photos metadata exceeds the 128 MB session limit") }
        try file.write(contentsOf: data)
    }
    func object(_ value: Any) throws { try write(JSONSerialization.data(withJSONObject: value, options: [.fragmentsAllowed])) }
    let options = PHFetchOptions()
    options.sortDescriptors = [NSSortDescriptor(key: "creationDate", ascending: false)]
    let assets = PHAsset.fetchAssets(with: .image, options: options)
    guard assets.count <= 500_000 else { throw BridgeError("This Photos library exceeds the 500,000 image session limit") }
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    try write(Data("{\"authorization\":".utf8)); try object(authorizationName(status))
    try write(Data(",\"assets\":[".utf8))
    var count = 0
    for i in 0..<assets.count {
        let asset = assets.object(at: i)
        guard let resource = originalResource(asset) else { continue }
        if count > 0 { try write(Data(",".utf8)) }; count += 1
        var row: [String: Any] = ["id": asset.localIdentifier, "name": resource.originalFilename,
            "width": asset.pixelWidth, "height": asset.pixelHeight, "favorite": asset.isFavorite,
            "raw": UTType(resource.uniformTypeIdentifier)?.conforms(to: .rawImage) == true,
            "revision": asset.modificationDate.map { formatter.string(from: $0) } ?? "",
            "format": (resource.originalFilename as NSString).pathExtension.uppercased()]
        if let date = asset.creationDate { row["captured"] = formatter.string(from: date) }
        try object(row)
    }
    try write(Data("],\"albums\":[".utf8))
    var seen = Set<String>()
    var albums = 0
    func collection(_ value: PHCollection, parent: String?, depth: Int) throws {
        guard depth < 32 else { throw BridgeError("Photos album folders are too deeply nested") }
        guard seen.insert(value.localIdentifier).inserted else { return }
        guard seen.count <= 20_000 else { throw BridgeError("Photos has too many albums for this session") }
        var row: [String: Any] = ["id": value.localIdentifier, "name": value.localizedTitle ?? "Album"]
        if let parent { row["parent"] = parent }
        if let album = value as? PHAssetCollection {
            var ids: [String] = []
            let members = PHAsset.fetchAssets(in: album, options: options)
            guard members.count <= 500_000 else { throw BridgeError("Photos album is too large") }
            members.enumerateObjects { asset, _, _ in if asset.mediaType == .image { ids.append(asset.localIdentifier) } }
            row["photos"] = ids; row["folder"] = false
        } else if value is PHCollectionList {
            row["photos"] = [String](); row["folder"] = true
        } else { return }
        if albums > 0 { try write(Data(",".utf8)) }; albums += 1
        try object(row)
        if let folder = value as? PHCollectionList {
            let children = PHCollection.fetchCollections(in: folder, options: nil)
            for i in 0..<children.count { try collection(children.object(at: i), parent: folder.localIdentifier, depth: depth + 1) }
        }
    }
    let roots = PHCollectionList.fetchTopLevelUserCollections(with: nil)
    for i in 0..<roots.count { try collection(roots.object(at: i), parent: nil, depth: 0) }
    let others = PHAssetCollection.fetchAssetCollections(with: .album, subtype: .any, options: nil)
    for i in 0..<others.count { try collection(others.object(at: i), parent: nil, depth: 0) }
    try write(Data("]}".utf8))
    return ["authorization": authorizationName(status), "photos": count, "albums": albums]
}

func photosAsset(_ id: String) throws -> PHAsset {
    guard let asset = PHAsset.fetchAssets(withLocalIdentifiers: [id], options: nil).firstObject,
          asset.mediaType == .image else { throw BridgeError("This photo is no longer available in Apple Photos; refresh the library") }
    return asset
}

// Library thumbnails use Photos' rendered preview, never the RAW resource or iCloud network.
func previewPhotos(_ request: Request) async throws -> [String: Any] {
    _ = try await authorizePhotos()
    let asset = try photosAsset(required(request.asset, "asset"))
    let edge = request.maxEdge ?? 512
    guard edge > 0, edge <= 1024 else { throw BridgeError("Invalid Photos thumbnail size") }
    let options = PHImageRequestOptions()
    options.version = .current
    options.deliveryMode = .fastFormat // Exactly one callback; locally cached degraded previews are welcome.
    options.resizeMode = .fast
    options.isNetworkAccessAllowed = false
    let image: NSImage = try await withCheckedThrowingContinuation { continuation in
        PHImageManager.default().requestImage(for: asset, targetSize: CGSize(width: edge, height: edge),
            contentMode: .aspectFit, options: options) { image, info in
            if let image { continuation.resume(returning: image) }
            else if let error = info?[PHImageErrorKey] as? Error { continuation.resume(throwing: error) }
            else { continuation.resume(throwing: BridgeError("No local Photos preview. Click Download original for this photo.")) }
        }
    }
    guard let cgImage = image.cgImage(forProposedRect: nil, context: nil, hints: nil),
          let space = CGColorSpace(name: CGColorSpace.extendedLinearSRGB) else {
        throw BridgeError("Photos supplied an unreadable preview")
    }
    let source = CIImage(cgImage: cgImage)
    let scale = min(1, Double(edge) / max(source.extent.width, source.extent.height))
    let scaled = source.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
    let bounds = scaled.extent.integral
    let w = Int(bounds.width), h = Int(bounds.height)
    let count = try checkedDimensions(w, h, limit: 1_048_576)
    let context = CIContext(options: [.cacheIntermediates: false])
    var pixels = [Float](repeating: 0, count: count * 4)
    pixels.withUnsafeMutableBytes { buffer in
        if let address = buffer.baseAddress { context.render(scaled, toBitmap: address, rowBytes: w * 16, bounds: bounds, format: .RGBAf, colorSpace: space) }
    }
    try pixels.withUnsafeBytes { try Data($0).write(to: URL(fileURLWithPath: try required(request.output, "output")), options: .atomic) }
    return ["width":w, "height":h, "temperature":6500.0, "tint":0.0, "raw":false,
        "decoder":"Apple Photos cached preview", "colorSpace":"extended-linear-sRGB"]
}

@available(macOS 12.0, *)
func decodePhotos(_ request: Request) async throws -> [String: Any] {
    _ = try await authorizePhotos()
    let asset = try photosAsset(required(request.asset, "asset"))
    guard let original = originalResource(asset) else { throw BridgeError("Photos has no readable image resource") }
    let isRaw = UTType(original.uniformTypeIdentifier)?.conforms(to: .rawImage) == true
    let resource = isRaw ? original : (PHAssetResource.assetResources(for: asset).first { $0.type == .fullSizePhoto } ?? original)
    let output = URL(fileURLWithPath: try required(request.output, "output"))
    // The public PhotoKit API streams a resource to a short-lived file. Rust owns the enclosing
    // temporary directory, so cancellation/timeouts remove it too. No Originals folder or import.
    let suffix = (resource.originalFilename as NSString).pathExtension
    let input = output.deletingLastPathComponent().appendingPathComponent("source-" + UUID().uuidString + "." + suffix)
    TemporaryOriginals.shared.track(input)
    defer { try? FileManager.default.removeItem(at: input); TemporaryOriginals.shared.forget(input) }
    let options = PHAssetResourceRequestOptions()
    options.isNetworkAccessAllowed = request.network ?? false
    try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
        PHAssetResourceManager.default().writeData(for: resource, toFile: input, options: options) { error in
            if let error { continuation.resume(throwing: error) } else { continuation.resume() }
        }
    }
    if isRaw {
        var rawRequest = request
        rawRequest.operation = "raw-decode"; rawRequest.path = input.path
        var result = try raw(rawRequest); result["raw"] = true
        return result
    }
    guard let image = CIImage(contentsOf: input, options: [.applyOrientationProperty: true]) else {
        throw BridgeError("Apple Core Image cannot decode this Photos image")
    }
    let edge = request.maxEdge ?? 16384
    guard edge > 0 else { throw BridgeError("Invalid Photos preview size") }
    let scale = min(1, Double(edge) / max(image.extent.width, image.extent.height))
    let scaled = image.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
    let bounds = scaled.extent.integral
    let w = Int(bounds.width), h = Int(bounds.height)
    let count = try checkedDimensions(w, h, limit: 100_000_000)
    guard let space = CGColorSpace(name: CGColorSpace.extendedLinearSRGB) else { throw BridgeError("Linear sRGB is unavailable") }
    let context = CIContext(options: [.cacheIntermediates: false])
    var pixels = [Float](repeating: 0, count: count * 4)
    pixels.withUnsafeMutableBytes { buffer in
        if let address = buffer.baseAddress { context.render(scaled, toBitmap: address, rowBytes: w * 16, bounds: bounds, format: .RGBAf, colorSpace: space) }
    }
    try pixels.withUnsafeBytes { try Data($0).write(to: output, options: .atomic) }
    return ["width": w, "height": h, "temperature": 6500.0, "tint": 0.0, "raw": false,
        "colorSpace": "extended-linear-sRGB", "decoder": "Apple PhotoKit/Core Image"]
}

let adjustmentIdentifier = "com.nicolassturmel.lightcraft.photos"
let adjustmentVersion = "1"

func savePhotosCopy(_ request: Request) async throws -> [String: Any] {
    _ = try await authorizePhotos()
    let sourceID = try required(request.asset, "asset")
    let source = try photosAsset(sourceID)
    let jpeg = URL(fileURLWithPath: try required(request.path, "path"))
    let recipeURL = URL(fileURLWithPath: try required(request.recipe, "recipe"))
    guard (try recipeURL.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? Int.max) <= 16_000_000 else { throw BridgeError("Adjustment metadata is too large") }
    let recipe = try Data(contentsOf: recipeURL)
    guard let info = try JSONSerialization.jsonObject(with: recipe) as? [String: Any],
          info["schema"] as? Int == 1, info["originalAssetId"] as? String == sourceID else {
        throw BridgeError("The adjustment recipe does not belong to the selected Photos original")
    }
    guard (try jpeg.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? Int.max) <= 512_000_000 else { throw BridgeError("Photos copy is too large") }
    let jpegData = try Data(contentsOf: jpeg)
    guard let image = CGImageSourceCreateWithURL(jpeg as CFURL, nil), CGImageSourceGetCount(image) > 0,
          CGImageSourceGetType(image) as String? == UTType.jpeg.identifier else { throw BridgeError("An edited Photos copy must be a valid JPEG") }
    let options = PHFetchOptions(); options.predicate = NSPredicate(format: "title = %@", "LightCraft")
    let album = PHAssetCollection.fetchAssetCollections(with: .album, subtype: .albumRegular, options: options).firstObject
    var createdID: String?
    var outputError: Error?
    // Only creation requests are used. There is deliberately no change/delete request for source.
    try await PHPhotoLibrary.shared().performChanges {
        let creation = PHAssetCreationRequest.forAsset()
        let resourceOptions = PHAssetResourceCreationOptions()
        resourceOptions.originalFilename = "LightCraft-" + jpeg.lastPathComponent
        creation.addResource(with: .photo, fileURL: jpeg, options: resourceOptions)
        creation.creationDate = source.creationDate; creation.location = source.location
        guard let placeholder = creation.placeholderForCreatedAsset else { return }
        createdID = placeholder.localIdentifier
        let output = PHContentEditingOutput(placeholderForCreatedAsset: placeholder)
        output.adjustmentData = PHAdjustmentData(formatIdentifier: adjustmentIdentifier, formatVersion: adjustmentVersion, data: recipe)
        do {
            try jpegData.write(to: output.renderedContentURL, options: .atomic)
            creation.contentEditingOutput = output
        } catch { outputError = error }
        let change = album.flatMap { PHAssetCollectionChangeRequest(for: $0) }
            ?? PHAssetCollectionChangeRequest.creationRequestForAssetCollection(withTitle: "LightCraft")
        change.addAssets([placeholder] as NSArray)
    }
    guard let createdID else { throw BridgeError("Photos did not return the edited copy identifier") }
    var result: [String: Any] = ["id": createdID, "album": "LightCraft", "originalUnchanged": true, "adjustmentsSaved": outputError == nil]
    if let outputError { result["warning"] = "The edited copy was saved, but its adjustment metadata could not be attached: " + outputError.localizedDescription }
    return result
}

func importPhotos(_ request: Request) async throws -> [String: Any] {
    _ = try await authorizePhotos()
    guard let paths = request.paths, !paths.isEmpty, paths.count <= 200 else { throw BridgeError("Select 1–200 image files") }
    var urls: [URL] = []
    for path in paths {
        let url = URL(fileURLWithPath: path)
        guard let image = CGImageSourceCreateWithURL(url as CFURL, nil), CGImageSourceGetCount(image) > 0 else {
            throw BridgeError("Photos cannot import " + url.lastPathComponent + ". Import folders using the Photos app.")
        }
        urls.append(url)
    }
    var ids: [String] = []
    try await PHPhotoLibrary.shared().performChanges {
        for url in urls {
            let creation = PHAssetCreationRequest.forAsset()
            let options = PHAssetResourceCreationOptions(); options.shouldMoveFile = false
            options.originalFilename = url.lastPathComponent
            creation.addResource(with: .photo, fileURL: url, options: options)
            if let placeholder = creation.placeholderForCreatedAsset { ids.append(placeholder.localIdentifier) }
        }
    }
    return ["imported": ids, "backend": "applePhotos"]
}

func photosRecipe(_ request: Request) async throws -> [String: Any] {
    _ = try await authorizePhotos()
    let asset = try photosAsset(required(request.asset, "asset"))
    let options = PHContentEditingInputRequestOptions()
    options.isNetworkAccessAllowed = request.network ?? false
    options.canHandleAdjustmentData = { $0.formatIdentifier == adjustmentIdentifier && $0.formatVersion == adjustmentVersion }
    let input: PHContentEditingInput = try await withCheckedThrowingContinuation { continuation in
        asset.requestContentEditingInput(with: options) { input, info in
            if let input { continuation.resume(returning: input) }
            else { continuation.resume(throwing: info[PHContentEditingInputErrorKey] as? Error ?? BridgeError("Photos adjustment metadata is unavailable")) }
        }
    }
    guard let adjustment = input.adjustmentData, adjustment.formatIdentifier == adjustmentIdentifier,
          adjustment.formatVersion == adjustmentVersion, adjustment.data.count <= 16_000_000 else { throw BridgeError("This photo has no LightCraft adjustment recipe") }
    try adjustment.data.write(to: URL(fileURLWithPath: try required(request.output, "output")), options: .atomic)
    return ["schema": 1]
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
func selectedInstances(_ observation: VNInstanceMaskObservation, _ request: Request) throws -> IndexSet {
    guard let points = request.points else { return observation.allInstances }
    let exclude = request.exclude ?? []
    guard points.count + exclude.count <= 64,
          (points + exclude).allSatisfy({ $0.count == 2 && $0.allSatisfy({ $0.isFinite && (0...1).contains($0) }) }) else {
        throw BridgeError("Invalid Apple Vision object clicks")
    }
    let labels = observation.instanceMask
    let width = CVPixelBufferGetWidth(labels), height = CVPixelBufferGetHeight(labels)
    _ = try checkedDimensions(width, height, limit: 10_000_000)
    let format = CVPixelBufferGetPixelFormatType(labels)
    let componentSize: Int
    switch format {
    case kCVPixelFormatType_OneComponent8: componentSize = 1
    case kCVPixelFormatType_OneComponent32Float: componentSize = 4
    default: throw BridgeError("Unsupported Apple Vision instance label format")
    }
    let stride = CVPixelBufferGetBytesPerRow(labels)
    guard !CVPixelBufferIsPlanar(labels), stride >= width * componentSize,
          CVPixelBufferLockBaseAddress(labels, .readOnly) == kCVReturnSuccess else {
        throw BridgeError("Cannot read Apple Vision instance labels")
    }
    defer { CVPixelBufferUnlockBaseAddress(labels, .readOnly) }
    guard let address = CVPixelBufferGetBaseAddress(labels) else { throw BridgeError("Missing Apple Vision instance labels") }
    func instance(_ point: [Double]) throws -> Int {
        // Pixel-buffer rows and LightCraft's image coordinates both start at the top left.
        let x = min(width - 1, Int(point[0] * Double(width)))
        let y = min(height - 1, Int(point[1] * Double(height)))
        let offset = y * stride + x * componentSize
        let value = componentSize == 1 ? Float(address.load(fromByteOffset: offset, as: UInt8.self))
            : address.load(fromByteOffset: offset, as: Float.self)
        guard value.isFinite, value >= 0, value <= 255, value.rounded() == value else {
            throw BridgeError("Invalid Apple Vision instance label")
        }
        return Int(value)
    }
    var selected = IndexSet()
    for point in points {
        let label = try instance(point)
        guard label != 0, observation.allInstances.contains(label) else {
            throw BridgeError("Apple Vision found no foreground object at this point. Click a foreground subject or use Brush.")
        }
        selected.insert(label)
    }
    for point in exclude { selected.remove(try instance(point)) }
    return selected
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
    let selected = try selectedInstances(observation, request)
    var coverage = [Float](repeating: 0, count: side * side)
    // Vision's empty IndexSet means all instances. Explicitly retain zero coverage when the
    // user removed every object instead of accidentally selecting all of them again.
    if !selected.isEmpty {
        let buffer = try observation.generateScaledMaskForImage(forInstances: selected, from: handler)
        let mask = CIImage(cvPixelBuffer: buffer)
        let scaled = mask.transformed(by: CGAffineTransform(scaleX: CGFloat(side) / mask.extent.width, y: CGFloat(side) / mask.extent.height))
        let context = CIContext(options: [.cacheIntermediates: false])
        coverage.withUnsafeMutableBytes { bytes in
            if let address = bytes.baseAddress {
                context.render(scaled, toBitmap: address, rowBytes: side * 4, bounds: CGRect(x: 0, y: 0, width: side, height: side), format: .Rf, colorSpace: nil)
            }
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
        watchdog.setEventHandler { if getppid() != parent { TemporaryOriginals.shared.cleanup(); exit(2) } }
        watchdog.resume()
        defer { watchdog.cancel() }
        do {
            let input = FileHandle.standardInput.readData(ofLength: 65_537)
            guard input.count <= 65_536 else { throw BridgeError("Oversized bridge request") }
            let request = try JSONDecoder().decode(Request.self, from: input)
            let result: [String: Any]
            switch request.operation {
            case "health":
                result = ["protocol": 2, "photos": authorizationName(PHPhotoLibrary.authorizationStatus(for: .readWrite)),
                    "raw": ProcessInfo.processInfo.isOperatingSystemAtLeast(OperatingSystemVersion(majorVersion: 12, minorVersion: 0, patchVersion: 0)),
                    "vision": ProcessInfo.processInfo.isOperatingSystemAtLeast(OperatingSystemVersion(majorVersion: 14, minorVersion: 0, patchVersion: 0))]
            case "photos-import": result = try await importPhotos(request)
            case "photos-snapshot": result = try await snapshotPhotos(request)
            case "photos-preview": result = try await previewPhotos(request)
            case "photos-decode":
                guard #available(macOS 12.0, *) else { throw BridgeError("Photos image decoding requires macOS 12 or later") }
                result = try await decodePhotos(request)
            case "photos-save-copy": result = try await savePhotosCopy(request)
            case "photos-recipe": result = try await photosRecipe(request)
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
            let native = error as NSError
            let result: [String: Any] = ["error": native.localizedDescription, "errorDomain": native.domain, "errorCode": native.code]
            if let data = try? JSONSerialization.data(withJSONObject: result) { try? FileHandle.standardOutput.write(contentsOf: data) }
            exit(1)
        }
    }
}
