use super::{CommandSpec, cmd};
use serde_json::json;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "apple.status", "Apple Services", [], None, "{} → Photos authorization and Core Image RAW/Vision availability (never requests permission)", crate::apple::enabled, |_, _| {
            crate::apple::status().map_err(crate::EngineError::Other)
        }),
        cmd!(query "library.applePhotos", "Browse Apple Photos", [], None, "{offset?:0, limit?:100 (1–200), album?:PhotoKitId} → {authorization, total, offset, nextOffset, assets, albums}; requests Photos permission when first used", crate::apple::enabled, |_, p| {
            crate::apple::list_photos(p).map_err(crate::EngineError::Other)
        }),
        cmd!(
            "library.importApplePhotos",
            "Import Apple Photos",
            [],
            None,
            "{ids:[PhotoKitId] (1–200), network?:false} → normal import report plus Photos errors; copies unmodified originals into Originals/Apple Photos, preferring RAW in RAW+JPEG pairs",
            crate::apple::enabled,
            |s, p| {
                let request = crate::apple::export_request(s, p).map_err(crate::EngineError::Other)?;
                let exported = crate::apple::export_originals(&request).map_err(crate::EngineError::Other)?;
                let mut report = if exported.get("paths").and_then(|v| v.as_array()).is_some_and(|p| !p.is_empty()) {
                    s.execute("library.import", &crate::apple::import_params(&exported))?
                } else {
                    json!({"imported":[], "duplicates":[], "failed":[]})
                };
                report["photosErrors"] = exported.get("errors").cloned().unwrap_or_else(|| json!([]));
                Ok(report)
            }
        ),
    ]
}
