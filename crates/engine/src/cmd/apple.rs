use super::{CommandSpec, cmd};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "photo.readApplePhotosMetadata",
            "Read EXIF from Apple Photos",
            ["Photo"],
            None,
            "{id?:active} → read and cache local capture metadata with PhotoKit/ImageIO; never enable iCloud or decode pixels",
            crate::apple::has_photo,
            |s, p| {
                let job = crate::apple::prepare_metadata(s, p).map_err(crate::EngineError::Other)?;
                let capture = job.run().map_err(crate::EngineError::Other)?;
                crate::apple::accept_metadata(s, &job, capture).map_err(crate::EngineError::Other)
            }
        ),
        cmd!(query "apple.status", "Apple Services", [], None, "{} → Photos authorization and Core Image RAW/Vision availability (never requests permission)", crate::apple::enabled, |_, _| {
            crate::apple::status().map_err(crate::EngineError::Other)
        }),
        cmd!(
            "library.applePhotos",
            "Refresh Apple Photos",
            ["File"],
            None,
            "{} → rebuild the transient main grid and album tree from the System Photo Library; no originals are copied",
            crate::apple::primary,
            |s, _| {
                let loaded =
                    crate::apple::load_snapshot(s.apple_library.as_ref().and_then(|l| l.directory.as_deref())).map_err(crate::EngineError::Other)?;
                s.apply_photos_snapshot(loaded).map_err(crate::EngineError::Other)
            }
        ),
        cmd!(
            "photo.downloadApplePhotosOriginal",
            "Download Original from iCloud",
            ["Photo"],
            None,
            "{id?:active} → download and decode this Photos original only; all other reads keep network access disabled",
            crate::apple::primary,
            |s, p| {
                let job = crate::apple::prepare_download(s, p).map_err(crate::EngineError::Other)?;
                let downloaded = job.run().map_err(crate::EngineError::Other)?;
                crate::apple::accept_download(s, &job, downloaded).map_err(crate::EngineError::Other)
            }
        ),
        cmd!(
            "library.importIntoApplePhotos",
            "Import into Apple Photos",
            [],
            None,
            "{paths:[file] (1–200)} → create assets in the system library; source files are kept",
            crate::apple::primary,
            |_, p| { crate::apple::import_paths(p).and_then(|paths| crate::apple::import_into_photos(&paths)).map_err(crate::EngineError::Other) }
        ),
        cmd!(
            "photo.saveApplePhotosCopy",
            "Export to Apple Photos (JPEG)",
            ["File"],
            Some("Cmd+S"),
            "{id?:active} → export a full-size JPEG at quality 100 in the Photos album LightCraft, with adjustment metadata attached to the copy; never edit the original",
            crate::apple::has_photo,
            |s, p| { crate::apple::prepare_copy(s, p).and_then(|job| job.run()).map_err(crate::EngineError::Other) }
        ),
        cmd!(
            "photo.reopenApplePhotosRecipe",
            "Reopen Photos Adjustments",
            ["Photo"],
            None,
            "{} → read a LightCraft copy's adjustment metadata and reopen its original with those settings",
            crate::apple::has_photo,
            |s, _| {
                let photo =
                    s.active().and_then(|id| s.catalog.photo(id)).ok_or_else(|| super::bad("photo.reopenApplePhotosRecipe", "select a copy"))?;
                let lightcraft_catalog::Source::ApplePhotos { asset, .. } = &photo.source else {
                    return Err(super::bad("photo.reopenApplePhotosRecipe", "select a Photos copy"));
                };
                let recipe = crate::apple::load_recipe(asset, false).map_err(crate::EngineError::Other)?;
                crate::apple::apply_recipe(s, &recipe)
            }
        ),
    ]
}
