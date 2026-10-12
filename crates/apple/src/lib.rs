//! Apple system services behind a bounded JSON/file protocol. No Rust FFI or unsafe code.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use serde::Deserialize;
use serde_json::Value;
use std::{path::Path, time::Duration};

#[derive(Clone, Debug, Deserialize)]
pub struct RawMetadata {
    pub width: usize,
    pub height: usize,
    pub temperature: f64,
    pub tint: f64,
    #[serde(default)]
    pub raw: bool,
}

pub struct RawImage {
    pub metadata: RawMetadata,
    /// Oriented, row-major, extended linear sRGB; highlight values above one are preserved.
    pub pixels: Vec<[f32; 4]>,
}

#[cfg(any(target_os = "macos", test))]
fn response_error(response: &Value) -> Option<String> {
    let error = response.get("error")?.as_str()?;
    // PHPhotosError.networkAccessRequired: an iCloud original, not a RAW decoder failure.
    if response.get("errorDomain").and_then(Value::as_str) == Some("PHPhotosErrorDomain")
        && response.get("errorCode").and_then(Value::as_i64) == Some(3164)
    {
        Some("Original stored in iCloud. Click Download original for this photo to retrieve it.".into())
    } else {
        Some(error.into())
    }
}

/// Send one bounded request; errors and timeouts never leave a helper process behind.
pub fn invoke(request: &Value, timeout: Duration) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    return native::invoke(request, timeout);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (request, timeout);
        Err("Apple Photos, RAW and Vision are available only on macOS".into())
    }
}

pub fn decode_raw(path: &Path, max_edge: usize) -> Result<RawImage, String> {
    #[cfg(target_os = "macos")]
    return native::decode_raw(path, max_edge);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, max_edge);
        Err("Apple RAW is available only on macOS".into())
    }
}

/// Read the system library's metadata into a transient session projection.
pub fn photos_snapshot() -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    return native::photos_file("photos-snapshot", None, false, 128_000_000);
    #[cfg(not(target_os = "macos"))]
    Err("Apple Photos is available only on macOS".into())
}

pub fn photos_recipe(asset: &str, network: bool) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    return native::photos_file("photos-recipe", Some(asset), network, 16_000_000);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (asset, network);
        Err("Apple Photos is available only on macOS".into())
    }
}

/// Decode directly through PhotoKit. The resource file exists only for the duration of decoding.
pub fn decode_photos(asset: &str, max_edge: usize, network: bool) -> Result<RawImage, String> {
    #[cfg(target_os = "macos")]
    return native::decode_image(serde_json::json!({"operation":"photos-decode", "asset":asset,
        "maxEdge":max_edge.min(100_000), "network":network}));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (asset, max_edge, network);
        Err("Apple Photos is available only on macOS".into())
    }
}

/// A rendered Photos thumbnail for browsing only. Network access is always disabled.
pub fn photos_preview(asset: &str, max_edge: usize) -> Result<RawImage, String> {
    #[cfg(target_os = "macos")]
    return native::decode_image(serde_json::json!({"operation":"photos-preview", "asset":asset, "maxEdge":max_edge.clamp(1, 1024)}));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (asset, max_edge);
        Err("Apple Photos previews are available only on macOS".into())
    }
}

/// Create a new Photos asset and attach the edit recipe to that copy. Never modify the source.
pub fn save_photos_copy(asset: &str, filename: &str, jpeg: &[u8], recipe: &Value) -> Result<Value, String> {
    if asset.is_empty() || asset.len() > 1024 || recipe.get("originalAssetId").and_then(Value::as_str) != Some(asset) {
        return Err("the Photos copy recipe must identify its original".into());
    }
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let filename = Path::new(filename).file_name().and_then(|s| s.to_str()).filter(|s| *s != "." && *s != "..").ok_or("invalid copy filename")?;
        let image = dir.path().join(filename);
        let metadata = dir.path().join("recipe.json");
        let data = serde_json::to_vec(recipe).map_err(|e| e.to_string())?;
        if data.len() > 16_000_000 || jpeg.len() > 512_000_000 {
            return Err("Photos copy exceeds the size limit".into());
        }
        std::fs::File::create(&image).and_then(|mut f| f.write_all(jpeg)).map_err(|e| e.to_string())?;
        std::fs::write(&metadata, data).map_err(|e| e.to_string())?;
        invoke(&serde_json::json!({"operation":"photos-save-copy", "asset":asset, "path":image, "recipe":metadata}), Duration::from_secs(180))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (filename, jpeg);
        Err("Apple Photos is available only on macOS".into())
    }
}

pub fn probe_raw_bytes(bytes: &[u8]) -> Result<RawMetadata, String> {
    #[cfg(target_os = "macos")]
    {
        let input = native::raw_input(bytes)?;
        let value = invoke(&serde_json::json!({"operation": "raw-probe", "path": input.path()}), Duration::from_secs(30))?;
        let meta: RawMetadata = serde_json::from_value(value).map_err(|e| e.to_string())?;
        checked_pixels(meta.width, meta.height, 100_000_000)?;
        Ok(meta)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = bytes;
        Err("Apple RAW is available only on macOS".into())
    }
}

pub fn decode_raw_bytes(bytes: &[u8], max_edge: usize) -> Result<RawImage, String> {
    #[cfg(target_os = "macos")]
    return decode_raw(native::raw_input(bytes)?.path(), max_edge);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (bytes, max_edge);
        Err("Apple RAW is available only on macOS".into())
    }
}

/// Vision coverage over the uncropped, oriented, linear Rec.2020 image, stretched to a square.
pub fn foreground_mask(width: usize, height: usize, pixels: &[[f32; 3]], side: usize) -> Result<Vec<f32>, String> {
    vision_mask(width, height, pixels, side, None, false)
}

/// Foreground protection for sky selection: a landscape with no detected subject is valid.
/// Unsupported systems and inference/protocol errors still propagate to the caller.
pub fn foreground_mask_or_empty(width: usize, height: usize, pixels: &[[f32; 3]], side: usize) -> Result<Vec<f32>, String> {
    vision_mask(width, height, pixels, side, None, true)
}

/// Classify sky using Apple's bundled semantic model through Vision/Core ML. Inference is local;
/// a missing model is an explicit packaging error, never a download or a colour-based fallback.
pub fn semantic_sky_mask(width: usize, height: usize, pixels: &[[f32; 3]], side: usize) -> Result<Vec<f32>, String> {
    validate_mask_input(width, height, pixels, side)?;
    #[cfg(target_os = "macos")]
    return native::mask(width, height, pixels, side, None, true, Some(&native::sky_model()?));
    #[cfg(not(target_os = "macos"))]
    Err("Apple semantic sky masks are available only on macOS".into())
}

/// Pick foreground instances at normalized, top-left-origin image points. Exclusions remove
/// whole detected instances; an empty selection stays empty (never selects every instance).
pub fn foreground_instances(
    width: usize,
    height: usize,
    pixels: &[[f32; 3]],
    side: usize,
    include: &[[f64; 2]],
    exclude: &[[f64; 2]],
) -> Result<Vec<f32>, String> {
    if include.len().saturating_add(exclude.len()) > 64 || include.iter().chain(exclude).flatten().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        return Err("invalid or oversized Apple Vision object clicks".into());
    }
    vision_mask(width, height, pixels, side, Some((include, exclude)), false)
}

type InstancePoints<'a> = Option<(&'a [[f64; 2]], &'a [[f64; 2]])>;

fn vision_mask(
    width: usize,
    height: usize,
    pixels: &[[f32; 3]],
    side: usize,
    points: InstancePoints<'_>,
    allow_empty: bool,
) -> Result<Vec<f32>, String> {
    validate_mask_input(width, height, pixels, side)?;
    #[cfg(target_os = "macos")]
    return native::mask(width, height, pixels, side, points, allow_empty, None);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (points, allow_empty);
        Err("Apple Vision is available only on macOS".into())
    }
}

fn validate_mask_input(width: usize, height: usize, pixels: &[[f32; 3]], side: usize) -> Result<(), String> {
    checked_pixels(width, height, 10_000_000)?;
    checked_pixels(side, side, 1_048_576)?;
    if pixels.len() != width.saturating_mul(height) || pixels.iter().flatten().any(|v| !v.is_finite()) {
        return Err("invalid Vision input pixels".into());
    }
    Ok(())
}

fn checked_pixels(width: usize, height: usize, max: usize) -> Result<usize, String> {
    width.checked_mul(height).filter(|&n| width > 0 && height > 0 && n <= max).ok_or_else(|| "invalid or oversized Apple image".into())
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::{
        fs,
        io::{Read, Write},
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        process::{Command, Stdio},
        time::Instant,
    };

    const HELPER: &[u8] = include_bytes!(env!("LIGHTCRAFT_APPLE_HELPER"));
    const MAX_JSON: u64 = 192_000;

    pub fn sky_model() -> Result<PathBuf, String> {
        const MODEL: &str = "DETRResnet50SemanticSegmentationF16P8.mlmodelc";
        let path = if let Some(path) = std::env::var_os("LIGHTCRAFT_APPLE_SKY_MODEL") {
            PathBuf::from(path)
        } else if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
            if dir.file_name().is_some_and(|name| name == "MacOS") {
                dir.parent().ok_or("invalid macOS bundle location")?.join("Resources/Models").join(MODEL)
            } else if dir.join("Models").join(MODEL).is_dir() {
                dir.join("Models").join(MODEL)
            } else {
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/apple-models").join(MODEL)
            }
        } else {
            return Err("cannot locate the Apple semantic sky model".into());
        };
        if !path.is_dir() {
            return Err("Apple sky model missing. Run packaging/macos/prepare-sky-model.sh or rebuild the macOS package.".into());
        }
        Ok(path)
    }

    fn helper() -> Result<PathBuf, String> {
        // Release bundles provide a signed sibling. Cargo/CLI builds extract the embedded helper.
        if let Some(sibling) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("lightcraft-apple")))
            && sibling.is_file()
        {
            return Ok(sibling);
        }
        let home = std::env::var_os("HOME").ok_or("HOME is unavailable for the Apple bridge")?;
        let digest = format!("{:x}", Sha256::digest(HELPER));
        let dir = PathBuf::from(home).join("Library/Application Support/LightCraft/Apple").join(&digest);
        let path = dir.join("lightcraft-apple");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        if path.is_file() && bounded_read(&path, HELPER.len() as u64).is_ok_and(|bytes| bytes == HELPER) {
            return Ok(path);
        }
        let mut file = tempfile::NamedTempFile::new_in(&dir).map_err(|e| e.to_string())?;
        file.write_all(HELPER).map_err(|e| e.to_string())?;
        file.as_file().set_permissions(fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        file.persist(&path).map_err(|e| e.to_string())?;
        Ok(path)
    }

    fn bounded_read(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
        let file = fs::File::open(path).map_err(|e| e.to_string())?;
        if file.metadata().map_err(|e| e.to_string())?.len() > cap {
            return Err("oversized Apple bridge response".into());
        }
        let mut bytes = Vec::new();
        file.take(cap + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > cap {
            return Err("oversized Apple bridge response".into());
        }
        Ok(bytes)
    }

    pub fn invoke(request: &Value, timeout: Duration) -> Result<Value, String> {
        let mut request = request.clone();
        if request.get("operation").and_then(Value::as_str) == Some("health")
            && request.get("model").is_none()
            && let Ok(model) = sky_model()
        {
            request["model"] = json!(model);
        }
        let data = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        if data.len() > 65_536 {
            return Err("oversized Apple bridge request".into());
        }
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let input = dir.path().join("request.json");
        let output = dir.path().join("response.json");
        let errors = dir.path().join("stderr.txt");
        fs::write(&input, data).map_err(|e| e.to_string())?;
        let mut child = Command::new(helper()?)
            .stdin(Stdio::from(fs::File::open(input).map_err(|e| e.to_string())?))
            .stdout(Stdio::from(fs::File::create(&output).map_err(|e| e.to_string())?))
            .stderr(Stdio::from(fs::File::create(&errors).map_err(|e| e.to_string())?))
            .spawn()
            .map_err(|e| format!("starting Apple bridge: {e}"))?;
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(20)),
                result => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(match result {
                        Err(e) => format!("waiting for Apple bridge: {e}"),
                        _ => "Apple operation timed out; retry after checking Photos permissions or iCloud availability".into(),
                    });
                }
            }
        };
        let data = bounded_read(&output, MAX_JSON)?;
        let response: Value = serde_json::from_slice(&data).map_err(|e| {
            let message = bounded_read(&errors, 16_000).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            format!("invalid Apple bridge response ({status}): {e} {message}")
        })?;
        if let Some(error) = super::response_error(&response) {
            return Err(error);
        }
        if !status.success() {
            return Err(format!("Apple bridge exited with {status}"));
        }
        Ok(response)
    }

    pub fn raw_input(bytes: &[u8]) -> Result<tempfile::NamedTempFile, String> {
        if bytes.len() > 512_000_000 {
            return Err("oversized Apple RAW input".into());
        }
        let mut file = tempfile::Builder::new().suffix(".cr3").tempfile().map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        Ok(file)
    }

    pub fn photos_file(operation: &str, asset: Option<&str>, network: bool, cap: u64) -> Result<Value, String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let output = dir.path().join("photos.json");
        invoke(&json!({"operation":operation, "asset":asset, "network":network, "output":output}), Duration::from_secs(180))?;
        serde_json::from_slice(&bounded_read(&output, cap)?).map_err(|e| e.to_string())
    }

    pub fn decode_raw(path: &Path, max_edge: usize) -> Result<RawImage, String> {
        decode_image(json!({"operation":"raw-decode", "path":path, "maxEdge":max_edge.min(100_000)}))
    }

    pub fn decode_image(mut request: Value) -> Result<RawImage, String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let output = dir.path().join("rgba.f32");
        request["output"] = json!(output);
        let value = invoke(&request, Duration::from_secs(180))?;
        let metadata: RawMetadata = serde_json::from_value(value).map_err(|e| e.to_string())?;
        let count = checked_pixels(metadata.width, metadata.height, 100_000_000)?;
        let bytes = bounded_read(&output, (count as u64).saturating_mul(16))?;
        if bytes.len() != count.saturating_mul(16) {
            return Err("truncated Apple RAW pixels".into());
        }
        let mut pixels = Vec::with_capacity(count);
        for chunk in bytes.as_chunks::<16>().0 {
            let mut pixel = [0.0; 4];
            for (channel, component) in pixel.iter_mut().zip(chunk.as_chunks::<4>().0) {
                *channel = f32::from_le_bytes(*component);
                if !channel.is_finite() {
                    return Err("non-finite Apple RAW pixel".into());
                }
            }
            pixels.push(pixel);
        }
        Ok(RawImage { metadata, pixels })
    }

    pub fn mask(
        width: usize,
        height: usize,
        pixels: &[[f32; 3]],
        side: usize,
        points: super::InstancePoints<'_>,
        allow_empty: bool,
        model: Option<&Path>,
    ) -> Result<Vec<f32>, String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let path = dir.path().join("rec2020.f32");
        let output = dir.path().join("mask.f32");
        let mut file = std::io::BufWriter::new(fs::File::create(&path).map_err(|e| e.to_string())?);
        for pixel in pixels {
            for channel in pixel.iter().copied().chain(std::iter::once(1.0)) {
                file.write_all(&channel.to_le_bytes()).map_err(|e| e.to_string())?;
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        let mut request =
            json!({"operation":"vision-mask", "path":path, "output":output, "width":width, "height":height, "side":side, "allowEmpty":allow_empty});
        if let Some(model) = model {
            request["operation"] = json!("vision-sky-mask");
            request["model"] = json!(model);
        }
        if let Some((include, exclude)) = points {
            request["points"] = json!(include);
            request["exclude"] = json!(exclude);
        }
        let _ = invoke(&request, Duration::from_secs(120))?;
        let count = checked_pixels(side, side, 1_048_576)?;
        let bytes = bounded_read(&output, count as u64 * 4)?;
        if bytes.len() != count * 4 {
            return Err("truncated Apple Vision mask".into());
        }
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                let value = f32::from_le_bytes(*p);
                if value.is_finite() { Ok(value.clamp(0.0, 1.0)) } else { Err("non-finite Vision coverage".into()) }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_original_errors_explain_the_per_photo_action_without_hiding_other_failures() {
        let response = serde_json::json!({"error":"opaque localized error", "errorDomain":"PHPhotosErrorDomain", "errorCode":3164});
        let message = response_error(&response).unwrap();
        assert!(message.contains("iCloud"));
        assert!(message.contains("Download original for this photo"));
        for response in [
            serde_json::json!({"error":"network unavailable", "errorDomain":"PHPhotosErrorDomain", "errorCode":3169}),
            serde_json::json!({"error":"different failure", "errorDomain":"OtherDomain", "errorCode":3164}),
            serde_json::json!({"error":"invalid RAW"}),
        ] {
            assert_eq!(response_error(&response).as_deref(), response["error"].as_str());
        }
        assert!(response_error(&serde_json::json!({"width":6000})).is_none());
    }

    #[test]
    fn rejects_hostile_sizes_before_allocation() {
        assert!(checked_pixels(usize::MAX, 2, 100).is_err());
        assert!(checked_pixels(0, 1, 100).is_err());
        assert!(foreground_mask(10, 10, &[], 512).is_err());
        assert!(foreground_mask(1, 1, &[[f32::NAN, 0.0, 0.0]], 512).is_err());
        assert!(semantic_sky_mask(1, 1, &[[0.0; 3]], 1025).is_err());
        assert!(semantic_sky_mask(1, 1, &[[f32::INFINITY; 3]], 512).is_err());
        assert!(foreground_instances(1, 1, &[[0.0; 3]], 2, &[[f64::NAN, 0.5]], &[]).is_err());
        assert!(foreground_instances(1, 1, &[[0.0; 3]], 2, &[[1.01, 0.5]], &[]).is_err());
        assert!(foreground_instances(1, 1, &[[0.0; 3]], 2, &[[0.5, 0.5]; 65], &[]).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn compiled_helper_reports_capabilities_without_requesting_photos() {
        let result = invoke(&serde_json::json!({"operation":"health"}), Duration::from_secs(15)).unwrap();
        assert_eq!(result["protocol"], 2);
        assert!(result["photos"].is_string());
        assert!(result["vision"].is_boolean());
        assert!(result["sky"].is_boolean());
        assert_eq!(result["skyProvider"], "Apple Vision / Core ML");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn malformed_raw_returns_an_error() {
        assert!(probe_raw_bytes(b"not a raw").is_err());
        assert!(decode_raw_bytes(b"not a raw", 100).is_err());
    }
}
