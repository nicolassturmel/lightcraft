//! PhotoKit imports and persistent Vision selections. Apple Photos originals are never edited.
use crate::{
    Session,
    media::{SourceLevel, SourceRef},
};
use lightcraft_develop::{MaskShape, SegMask};
use serde_json::{Value, json};
use std::path::PathBuf;

pub fn enabled(_: &Session) -> Result<(), String> {
    if cfg!(target_os = "macos") { Ok(()) } else { Err("Apple services are available only on macOS".into()) }
}

pub fn status() -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    return lightcraft_apple::invoke(&json!({"operation":"health"}), std::time::Duration::from_secs(15));
    #[cfg(not(target_os = "macos"))]
    Err("Apple services are available only on macOS".into())
}

pub fn list_photos(params: &Value) -> Result<Value, String> {
    let offset = params.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(100);
    if offset > 10_000_000 || !(1..=200).contains(&limit) {
        return Err("Apple Photos needs a limit of 1–200 and a valid offset".into());
    }
    #[cfg(target_os = "macos")]
    return lightcraft_apple::invoke(
        &json!({"operation":"photos-list", "offset":offset, "limit":limit,
        "album":params.get("album").and_then(Value::as_str)}),
        std::time::Duration::from_secs(120),
    );
    #[cfg(not(target_os = "macos"))]
    Err("Apple Photos is available only on macOS".into())
}

/// Validate before starting a worker. Original copies belong to this persistent library.
pub fn export_request(session: &Session, params: &Value) -> Result<Value, String> {
    let ids = params.get("ids").and_then(Value::as_array).ok_or("select Apple Photos asset ids")?;
    if ids.is_empty() || ids.len() > 200 || ids.iter().any(|id| id.as_str().is_none_or(|s| s.is_empty() || s.len() > 1024)) {
        return Err("select between 1 and 200 valid Apple Photos assets".into());
    }
    let library = session.library.as_ref().filter(|l| l.on_disk).ok_or("open a library on disk before importing Apple Photos")?;
    let directory = library.dir.join("Originals/Apple Photos");
    Ok(json!({"operation":"photos-export", "ids":ids, "output":directory, "network":params.get("network").and_then(Value::as_bool).unwrap_or(false)}))
}

pub fn export_originals(request: &Value) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    return lightcraft_apple::invoke(request, std::time::Duration::from_secs(600));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = request;
        Err("Apple Photos is available only on macOS".into())
    }
}

pub fn import_params(exported: &Value) -> Value {
    json!({"paths":exported.get("paths").cloned().unwrap_or_else(|| json!([])), "mode":"add", "albumName":"Apple Photos"})
}

/// Demo photos keep their procedural selections; real macOS photos use Vision by default.
pub fn wants_vision(session: &Session, params: &Value) -> bool {
    cfg!(target_os = "macos")
        && matches!(params.get("kind").and_then(Value::as_str), Some("subject" | "background"))
        && params.get("seg").is_none()
        && params.get("provider").and_then(Value::as_str) != Some("heuristic")
        && (params.get("provider").and_then(Value::as_str) == Some("apple")
            || session.active().and_then(|id| session.catalog.photo(id)).is_some_and(|p| matches!(p.source, lightcraft_catalog::Source::File { .. })))
}

pub fn vision_source(session: &mut Session) -> Result<SourceRef, String> {
    let id = session.active().ok_or("no active photo")?;
    let photo = session.catalog.photo(id).ok_or("no such photo")?.clone();
    Ok(session.media.source_ref(&photo, SourceLevel::Preview))
}

pub fn vision_selection(source: SourceRef) -> Result<SegMask, String> {
    #[cfg(target_os = "macos")]
    {
        let image = source.load()?;
        let image = lightcraft_raster::resample::fit(&image, 2048, 2048, lightcraft_raster::resample::Filter::Box);
        let coverage = lightcraft_apple::foreground_mask(image.width, image.height, &image.data, 512)?;
        coverage_to_seg(512, &coverage)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = source;
        Err("Apple Vision is available only on macOS".into())
    }
}

pub fn coverage_to_seg(side: usize, coverage: &[f32]) -> Result<SegMask, String> {
    let count = side.checked_mul(side).filter(|_| side > 0 && side <= lightcraft_develop::segmask::MAX_SIDE).ok_or("invalid Vision mask size")?;
    if coverage.len() != count || coverage.iter().any(|v| !v.is_finite()) {
        return Err("invalid Vision coverage".into());
    }
    let logits: Vec<f32> = coverage
        .iter()
        .map(|&p| {
            let p = p.clamp(0.000_001, 0.999_999);
            (p / (1.0 - p)).ln()
        })
        .collect();
    Ok(SegMask::from_logits(side, &logits))
}

pub fn resolve_vision(session: &mut Session, params: &Value, shape: &mut MaskShape) -> Result<(), String> {
    if wants_vision(session, params) {
        let seg = vision_selection(vision_source(session)?)?;
        *shape = MaskShape::Vision { background: params.get("kind").and_then(Value::as_str) == Some("background"), seg, edge: 0.0 };
    }
    Ok(())
}

pub fn original_directory(session: &Session) -> Option<PathBuf> {
    session.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join("Originals/Apple Photos"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vision_coverage_survives_catalog_serialization() {
        let seg = coverage_to_seg(2, &[0.0, 0.25, 0.75, 1.0]).unwrap();
        let shape = MaskShape::Vision { background: false, seg, edge: 0.0 };
        let serialized = serde_json::to_value(&shape).unwrap();
        let decoded: MaskShape = serde_json::from_value(serialized).unwrap();
        assert_eq!(shape, decoded);
        let MaskShape::Vision { seg, .. } = decoded else { panic!("vision mask") };
        let logits = seg.logits().unwrap();
        assert!(logits[0] < -10.0 && logits[1] < 0.0 && logits[2] > 0.0 && logits[3] > 10.0);
        assert!(coverage_to_seg(0, &[]).is_err());
        assert!(coverage_to_seg(2, &[f32::NAN; 4]).is_err());
    }

    #[test]
    fn photo_import_requires_disk_library_and_bounded_asset_selection() {
        let session = Session::default();
        assert!(export_request(&session, &json!({"ids":["asset"]})).is_err());
        assert!(export_request(&session, &json!({"ids":[]})).is_err());
        assert!(export_request(&session, &json!({"ids":[42]})).is_err());
        assert!(list_photos(&json!({"limit":201})).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn canon_r8_cr3_uses_apple_raw_at_preview_and_full_resolution() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/raw/cr3-canon-r8-craw.cr3");
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let probe = crate::files::probe_bytes("sample.CR3", &bytes).unwrap();
        assert_eq!((probe.width, probe.height), (6000, 4000));
        assert!(probe.preview_only.is_none());
        let (preview, info) = crate::files::load_bytes(&bytes, 1000).unwrap();
        assert_eq!((preview.width, preview.height), (1000, 667));
        assert!(info.raw && !info.relative_wb && info.camera_color.is_none());
        assert!(preview.data.iter().flatten().all(|v| v.is_finite()));
        let native = lightcraft_apple::decode_raw_bytes(&bytes, 1000).unwrap();
        let transform = lightcraft_color::SRGB.to_space(&lightcraft_color::WORKING);
        for (apple, working) in native.pixels.iter().zip(&preview.data).step_by(1000) {
            let expected = transform.apply_f32([apple[0], apple[1], apple[2]]);
            assert!(expected.iter().zip(working).all(|(a, b)| (a - b).abs() < 1e-5));
        }
        let (full, info) = crate::files::load_bytes(&bytes, usize::MAX).unwrap();
        assert_eq!((full.width, full.height), (6000, 4000));
        assert!(info.raw && !info.relative_wb);
    }
}
