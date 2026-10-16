// Compile the pinned model using the public Core ML API (CommandLineTools are sufficient).
import Foundation
import CoreML

let arguments = CommandLine.arguments
if arguments.count != 3 {
    FileHandle.standardError.write(Data("Expected package and compiled destination\n".utf8))
    exit(2)
}
let source = URL(fileURLWithPath: arguments[1])
let destination = URL(fileURLWithPath: arguments[2])
let files = FileManager.default
let staging = destination.deletingLastPathComponent().appendingPathComponent("inpaint-\(UUID().uuidString).mlmodelc")
defer { try? files.removeItem(at: staging) }
do {
    let compiled = try MLModel.compileModel(at: source)
    defer { try? files.removeItem(at: compiled) }
    try files.copyItem(at: compiled, to: staging)
    // Validate the expected model interface before replacing the cached build input.
    let model = try MLModel(contentsOf: staging)
    guard model.modelDescription.inputDescriptionsByName["image"]?.type == .image,
          model.modelDescription.inputDescriptionsByName["mask"]?.type == .image,
          model.modelDescription.outputDescriptionsByName["output"]?.type == .image else {
        throw NSError(domain: "LightCraftInpaintModel", code: 1, userInfo: [NSLocalizedDescriptionKey: "Unsupported AI Remove model interface"])
    }
    if files.fileExists(atPath: destination.path) { try files.removeItem(at: destination) }
    try files.moveItem(at: staging, to: destination)
    print("Compiled local AI Remove model")
} catch {
    FileHandle.standardError.write(Data((error.localizedDescription.prefix(4000) + "\n").utf8))
    exit(1)
}
