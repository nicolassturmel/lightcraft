//! Apple Photos is the source of truth. Only adjustment metadata belongs to LightCraft.
use crate::{
    LibrarySource, Selection, Session,
    media::{SourceLevel, SourceRef},
};
use lightcraft_catalog::{Album, AlbumId, Catalog, ColorLabel, Flag, HistoryStep, MediaKind, Meta, Op, Photo, PhotoId, Source, Version};
use lightcraft_develop::{DevelopSettings, MaskShape, SegMask};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Default)]
pub struct Library {
    pub loaded: bool,
    pub authorization: String,
    pub error: Option<String>,
    pub edit_error: Option<String>,
    pub(crate) retry_at: Option<web_time::Instant>,
    /// Adjustment metadata only; no photo catalogue, album cache or original image files.
    pub directory: Option<PathBuf>,
    pub asset_ids: BTreeMap<String, PhotoId>,
    pub album_ids: BTreeMap<String, AlbumId>,
    drafts: BTreeMap<String, Draft>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Draft {
    pub develop: Arc<DevelopSettings>,
    pub rating: u8,
    pub flag: Flag,
    pub label: Option<ColorLabel>,
    pub meta: Meta,
    pub edited: Option<String>,
    pub versions: Vec<Version>,
    pub history: Vec<HistoryStep>,
}

impl Draft {
    fn of(p: &Photo) -> Self {
        Self {
            develop: p.develop.clone(),
            rating: p.rating,
            flag: p.flag,
            label: p.label,
            meta: p.meta.clone(),
            edited: p.edited.clone(),
            versions: p.versions.clone(),
            history: p.history.clone(),
        }
    }
    fn apply(&self, p: &mut Photo) {
        p.develop = self.develop.clone();
        p.rating = self.rating;
        p.flag = self.flag;
        p.label = self.label;
        p.meta = self.meta.clone();
        p.edited = self.edited.clone();
        p.versions = self.versions.clone();
        p.history = self.history.clone();
    }
}

#[derive(Serialize, Deserialize)]
struct DraftFile {
    schema: u32,
    asset: String,
    draft: Draft,
}

#[derive(Deserialize)]
pub struct Snapshot {
    pub authorization: String,
    pub assets: Vec<Asset>,
    pub albums: Vec<PhotosAlbum>,
}

#[derive(Deserialize)]
pub struct Asset {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub raw: bool,
    pub favorite: bool,
    pub format: String,
    #[serde(default)]
    pub revision: String,
    pub captured: Option<String>,
}

#[derive(Deserialize)]
pub struct PhotosAlbum {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub folder: bool,
    pub parent: Option<String>,
    pub photos: Vec<String>,
}

pub struct LoadedLibrary {
    pub snapshot: Snapshot,
    drafts: BTreeMap<String, Draft>,
    pub warnings: Vec<String>,
}

pub fn enabled(_: &Session) -> Result<(), String> {
    if cfg!(target_os = "macos") { Ok(()) } else { Err("Apple services are available only on macOS".into()) }
}

pub fn primary(s: &Session) -> Result<(), String> {
    enabled(s)?;
    if s.apple_library.is_some() { Ok(()) } else { Err("Open the Apple Photos library to use this command".into()) }
}

pub fn has_photo(s: &Session) -> Result<(), String> {
    primary(s)?;
    if s.active().and_then(|id| s.catalog.photo(id)).is_some_and(|p| matches!(p.source, Source::ApplePhotos { .. })) {
        Ok(())
    } else {
        Err("Select an Apple Photos image first".into())
    }
}

/// Catalogue-changing commands must never silently fork the system library into a local one.
pub fn command_allowed(s: &Session, id: &str) -> Result<(), String> {
    if s.apple_library.is_none() {
        return Ok(());
    }
    if (id.starts_with("album.") && !matches!(id, "album.ruleFields" | "album.list" | "album.info"))
        || id.starts_with("folder.")
        || id.starts_with("stack.")
        || matches!(
            id,
            "library.import"
                | "library.importPreview"
                | "library.importLightroom"
                | "library.browse"
                | "library.forgetLocal"
                | "library.removeFolder"
                | "library.restoreBackup"
                | "library.compact"
                | "library.backup"
                | "photo.addToLibrary"
                | "photo.delete"
                | "photo.restore"
                | "photo.deletePermanently"
                | "photo.deleteRejected"
                | "photo.remove"
                | "photo.rename"
                | "photo.setCaptureTime"
                | "photo.duplicate"
                | "photo.virtualCopy"
                | "photo.convertToDng"
                | "photo.editExternal"
                | "photo.relink"
                | "photo.reload"
                | "photo.saveMetadataToFile"
                | "photo.readMetadataFromFile"
                | "library.xmpPreferences"
        )
    {
        return Err("Apple Photos owns these photos and albums. Organize them in Photos; use Save Edited Copy to Photos for LightCraft edits.".into());
    }
    Ok(())
}

pub(crate) fn annotation_op(op: &Op, depth: usize) -> Result<(), String> {
    if depth > 64 {
        return Err("Photos adjustment batch is too deeply nested".into());
    }
    match op {
        Op::Batch { ops } => {
            for op in ops {
                annotation_op(op, depth + 1)?;
            }
            Ok(())
        }
        Op::SetRating { .. }
        | Op::SetFlag { .. }
        | Op::SetLabel { .. }
        | Op::SetDevelop { .. }
        | Op::SetMeta { .. }
        | Op::SetVersions { .. }
        | Op::SetHistory { .. }
        | Op::PushHistory { .. } => Ok(()),
        _ => Err("Apple Photos owns image identities and album membership. Only adjustment annotations can be changed in LightCraft.".into()),
    }
}

pub fn status() -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    return lightcraft_apple::invoke(&json!({"operation":"health"}), std::time::Duration::from_secs(15));
    #[cfg(not(target_os = "macos"))]
    Err("Apple services are available only on macOS".into())
}

fn draft_path(directory: &Path, asset: &str) -> PathBuf {
    let hash = lightcraft_preview::Hasher128::new().str("Photos adjustments v1").str(asset).finish();
    directory.join(format!("{hash}.json"))
}

fn read_draft(path: &Path) -> Result<DraftFile, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > 16_000_000 {
        return Err("Photos adjustment file is too large".into());
    }
    let mut data = Vec::new();
    file.take(16_000_001).read_to_end(&mut data).map_err(|e| e.to_string())?;
    if data.len() > 16_000_000 {
        return Err("Photos adjustment file is too large".into());
    }
    let stored: DraftFile = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    if stored.schema != 1 || stored.asset.is_empty() || stored.asset.len() > 1024 {
        return Err("unsupported Photos adjustment metadata".into());
    }
    Ok(stored)
}

#[cfg(any(target_os = "macos", test))]
fn load_drafts(directory: Option<&Path>) -> (BTreeMap<String, Draft>, Vec<String>) {
    let mut drafts = BTreeMap::new();
    let mut warnings = Vec::new();
    let Some(dir) = directory else {
        return (drafts, warnings);
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (drafts, warnings),
        Err(e) => return (drafts, vec![format!("Photos adjustments could not be read: {e}")]),
    };
    for entry in entries.take(100_001) {
        if drafts.len() >= 100_000 {
            warnings.push("Too many Photos adjustment files".into());
            break;
        }
        let result = entry.map_err(|e| e.to_string()).and_then(|entry| {
            if entry.path().extension().is_none_or(|ext| ext != "json") {
                return Ok(None);
            }
            let stored = read_draft(&entry.path())?;
            if entry.path() != draft_path(dir, &stored.asset) {
                return Err("Photos adjustment file identity does not match its name".into());
            }
            Ok(Some(stored))
        });
        match result {
            Ok(Some(stored)) => {
                drafts.insert(stored.asset, stored.draft);
            }
            Ok(None) => {}
            Err(e) => {
                if warnings.len() < 20 {
                    warnings.push(format!("Photos adjustment metadata kept unchanged: {e}"));
                }
            }
        }
    }
    (drafts, warnings)
}

pub fn load_snapshot(directory: Option<&Path>) -> Result<LoadedLibrary, String> {
    #[cfg(target_os = "macos")]
    {
        let snapshot: Snapshot = serde_json::from_value(lightcraft_apple::photos_snapshot()?).map_err(|e| e.to_string())?;
        let (drafts, warnings) = load_drafts(directory);
        Ok(LoadedLibrary { snapshot, drafts, warnings })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = directory;
        Err("Apple Photos is available only on macOS".into())
    }
}

impl Session {
    /// Opt in explicitly at the host boundary. No authorization or own catalogue is created here.
    pub fn with_apple_photos(mut self, directory: Option<PathBuf>) -> Self {
        self.library = None;
        self.catalog = Catalog::new();
        self.pending_log.clear();
        self.selection = Selection::default();
        self.source = LibrarySource::All;
        self.undo.clear();
        self.redo.clear();
        self.interaction = None;
        self.media.clear_sources();
        self.media.rendered.retire();
        self.media.rendered = Arc::new(lightcraft_preview::PreviewCache::memory(self.media.budget() / 3));
        self.media.smart_dir = None;
        self.visible_key = None;
        self.total = None;
        self.library_generation = crate::LIBRARY_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.library_identity = Arc::new(());
        self.apple_library = Some(Library { directory, ..Default::default() });
        self
    }

    /// Atomically replace the session projection after validating the complete PhotoKit result.
    /// Stable asset IDs preserve selection and edits; Photos alone defines membership and folders.
    pub fn apply_photos_snapshot(&mut self, loaded: LoadedLibrary) -> Result<Value, String> {
        if self.interaction.is_some() {
            return Err("Finish the current adjustment before refreshing Photos".into());
        }
        self.persist().map_err(|e| e.to_string())?;
        let current = self.apple_library.as_ref().ok_or("Apple Photos is not the active library")?;
        let mut ids = current.asset_ids.clone();
        let mut albums = current.album_ids.clone();
        let drafts = if current.loaded { current.drafts.clone() } else { loaded.drafts };
        let mut next_photo = ids.values().map(|id| id.0).max().unwrap_or(0).checked_add(1).ok_or("Photos identity overflow")?;
        let mut next_album = albums.values().map(|id| id.0).max().unwrap_or(0).checked_add(1).ok_or("Photos album identity overflow")?;
        let mut seen = BTreeSet::new();
        let mut seen_albums = BTreeSet::new();
        let mut next = Catalog::new();
        next.revision = self.catalog.revision.wrapping_add(1);
        let mut invalidated = Vec::new();
        for asset in loaded.snapshot.assets {
            if asset.id.is_empty()
                || asset.id.len() > 1024
                || !seen.insert(asset.id.clone())
                || asset.width == 0
                || asset.height == 0
                || u64::from(asset.width) * u64::from(asset.height) > 100_000_000
            {
                return Err("Invalid or duplicate Photos asset metadata; the current library has been kept".into());
            }
            let id = match ids.get(&asset.id).copied() {
                Some(id) => id,
                None => {
                    let id = PhotoId(next_photo);
                    next_photo = next_photo.checked_add(1).ok_or("Photos identity overflow")?;
                    id
                }
            };
            ids.insert(asset.id.clone(), id);
            let source = Source::ApplePhotos { asset: asset.id.clone(), revision: asset.revision };
            let mut photo = self
                .catalog
                .photo(id)
                .map(|p| (**p).clone())
                .unwrap_or_else(|| Photo::new(id, source.clone(), &asset.name, &asset.format, asset.width, asset.height, &(self.clock)()));
            if photo.source != source {
                invalidated.push(id);
            }
            let was_present = self.catalog.photo(id).is_some();
            photo.source = source;
            photo.file_name = asset.name;
            photo.format = asset.format;
            photo.width = asset.width;
            photo.height = asset.height;
            photo.captured = asset.captured;
            photo.kind = if asset.raw { MediaKind::Raw } else { MediaKind::Image };
            photo.deleted = false;
            photo.local = false;
            photo.copy_of = None;
            photo.content_hash = None;
            if !was_present {
                if asset.favorite {
                    photo.flag = Flag::Pick;
                }
                if let Some(draft) = drafts.get(&asset.id) {
                    draft.apply(&mut photo);
                }
            }
            next.apply(Op::AddPhoto { photo: Box::new(photo) }).map_err(|e| e.to_string())?;
        }
        for album in &loaded.snapshot.albums {
            if album.id.is_empty() || album.id.len() > 1024 || !seen_albums.insert(album.id.clone()) {
                return Err("Invalid or duplicate Photos album metadata; the current library has been kept".into());
            }
            let id = match albums.get(&album.id).copied() {
                Some(id) => id,
                None => {
                    let id = AlbumId(next_album);
                    next_album = next_album.checked_add(1).ok_or("Photos album identity overflow")?;
                    id
                }
            };
            albums.insert(album.id.clone(), id);
        }
        for album in loaded.snapshot.albums {
            let id = albums.get(&album.id).copied().ok_or("Photos album identity missing")?;
            let mut a = Album::new(id, &album.name);
            a.folder = album.folder;
            a.parent = album.parent.and_then(|p| albums.get(&p).copied());
            a.photos = album.photos.iter().filter(|p| seen.contains(*p)).filter_map(|p| ids.get(p).copied()).collect();
            next.apply(Op::AddAlbum { album: a }).map_err(|e| e.to_string())?;
        }
        let removed: Vec<_> = self.catalog.photos().filter(|p| next.photo(p.id).is_none()).map(|p| p.id).collect();
        for id in invalidated.iter().chain(&removed) {
            self.media.forget(*id);
        }
        if !removed.is_empty() {
            self.undo.clear();
            self.redo.clear();
        }
        self.catalog = next;
        self.selection.ids.retain(|id| self.catalog.photo(*id).is_some());
        self.selection.active = self.selection.active.filter(|id| self.catalog.photo(*id).is_some());
        if matches!(self.source, LibrarySource::Album(id) if self.catalog.album(id).is_none()) {
            self.source = LibrarySource::All;
        }
        let state = self.apple_library.as_mut().ok_or("Apple Photos closed during refresh")?;
        state.asset_ids = ids;
        state.album_ids = albums;
        state.drafts = drafts;
        state.authorization = loaded.snapshot.authorization;
        state.loaded = true;
        state.error = None;
        self.pending_log.clear();
        self.visible_key = None;
        self.total = None;
        Ok(json!({"backend":"applePhotos", "photos":self.catalog.len(), "albums":self.catalog.albums().count(), "warnings":loaded.warnings}))
    }

    /// Persist only the touched assets' editable annotations, not source paths or library contents.
    pub(crate) fn persist_photos(&mut self) -> crate::Result<()> {
        if self.pending_log.is_empty() {
            return Ok(());
        }
        let mut touched = Vec::new();
        for op in &self.pending_log {
            draft_photos(op, &mut touched);
        }
        let state = self.apple_library.as_mut().ok_or_else(|| crate::EngineError::Other("Apple Photos is closed".into()))?;
        for id in touched {
            let Some(photo) = self.catalog.photo(id) else {
                continue;
            };
            let Source::ApplePhotos { asset, .. } = &photo.source else {
                continue;
            };
            let draft = Draft::of(photo);
            if let Some(dir) = &state.directory {
                let result = (|| -> Result<(), String> {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
                    }
                    let path = draft_path(dir, asset);
                    if !state.drafts.contains_key(asset) && path.exists() {
                        read_draft(&path).map_err(|e| {
                            format!("Existing Photos adjustments kept unchanged: {e}. Back up and move the damaged JSON file before retrying.")
                        })?;
                    }
                    let data = serde_json::to_vec(&DraftFile { schema: 1, asset: asset.clone(), draft: draft.clone() }).map_err(|e| e.to_string())?;
                    if data.len() > 16_000_000 {
                        return Err("Photos adjustments exceed the 16 MB per-image limit".into());
                    }
                    lightcraft_catalog::safe_file::write_atomic(&draft_path(dir, asset), &data).map_err(|e| e.to_string())
                })();
                if let Err(e) = result {
                    state.edit_error = Some(e.clone());
                    state.retry_at = Some(web_time::Instant::now() + std::time::Duration::from_secs(2));
                    return Err(crate::EngineError::NotSaved(e));
                }
            }
            state.drafts.insert(asset.clone(), draft);
        }
        state.edit_error = None;
        state.retry_at = None;
        self.pending_log.clear();
        Ok(())
    }
}

fn draft_photos(op: &Op, ids: &mut Vec<PhotoId>) {
    match op {
        Op::SetVersions { id, .. } | Op::SetHistory { id, .. } | Op::PushHistory { id, .. } => {
            if !ids.contains(id) {
                ids.push(*id);
            }
        }
        Op::Batch { ops } => {
            for op in ops {
                draft_photos(op, ids);
            }
        }
        _ => crate::sidecar::op_photos(op, ids),
    }
}

pub fn decode_photo(asset: &str, max_edge: usize, network: bool) -> Result<crate::media::DecodedSource, String> {
    #[cfg(target_os = "macos")]
    {
        let decoded = lightcraft_apple::decode_photos(asset, max_edge, network)?;
        let matrix = lightcraft_color::SRGB.to_space(&lightcraft_color::WORKING);
        let image = lightcraft_raster::Rgb32f {
            width: decoded.metadata.width,
            height: decoded.metadata.height,
            data: decoded.pixels.into_iter().map(|[r, g, b, _]| matrix.apply_f32([r, g, b])).collect(),
        };
        let info = lightcraft_pipeline::SourceInfo {
            raw: decoded.metadata.raw,
            as_shot_temp: decoded.metadata.temperature.round(),
            as_shot_tint: decoded.metadata.tint.round(),
            ..Default::default()
        };
        Ok(crate::media::DecodedSource::new(Arc::new(image), Some(info)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (asset, max_edge, network);
        Err("Apple Photos decoding is available only on macOS".into())
    }
}

pub fn import_paths(params: &Value) -> Result<Vec<String>, String> {
    let paths = params.get("paths").and_then(Value::as_array).ok_or("Select image files to import into Apple Photos")?;
    if paths.is_empty() || paths.len() > 200 {
        return Err("Select 1–200 files to import into Apple Photos".into());
    }
    paths
        .iter()
        .map(|p| {
            p.as_str().filter(|p| !p.is_empty() && p.len() <= 4096).map(str::to_string).ok_or_else(|| "Invalid image path for Apple Photos".into())
        })
        .collect()
}

pub fn import_into_photos(paths: &[String]) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    return lightcraft_apple::invoke(&json!({"operation":"photos-import", "paths":paths}), std::time::Duration::from_secs(180));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = paths;
        Err("Apple Photos is available only on macOS".into())
    }
}

pub struct CopyJob {
    render: crate::export::PreparedExport,
    asset: String,
    recipe: Value,
}

pub fn prepare_copy(s: &mut Session, params: &Value) -> Result<CopyJob, String> {
    primary(s)?;
    if s.interaction.is_some() {
        return Err("Finish the current adjustment before saving a Photos copy".into());
    }
    s.persist().map_err(|e| e.to_string())?;
    let id = params.get("id").and_then(Value::as_u64).map(PhotoId).or_else(|| s.active()).ok_or("Select a photo first")?;
    let photo = s.catalog.photo(id).ok_or("This Photos image is no longer available")?;
    let Source::ApplePhotos { asset, .. } = &photo.source else {
        return Err("Select an Apple Photos image".into());
    };
    let asset = asset.clone();
    let recipe = json!({"schema":1, "originalAssetId":asset, "adjustments":Draft::of(photo)});
    let options = crate::export::ExportOptions { quality: 95, ..Default::default() };
    let render = crate::export::prepare_export(s, id, &options, 1)?;
    Ok(CopyJob { render, asset, recipe })
}

impl CopyJob {
    pub fn run(self) -> Result<Value, String> {
        let image = self.render.run()?;
        #[cfg(target_os = "macos")]
        return lightcraft_apple::save_photos_copy(&self.asset, &image.file_name, &image.bytes, &self.recipe);
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (image, self.asset, self.recipe);
            Err("Apple Photos is available only on macOS".into())
        }
    }
}

pub fn load_recipe(asset: &str, network: bool) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    return lightcraft_apple::photos_recipe(asset, network);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (asset, network);
        Err("Apple Photos is available only on macOS".into())
    }
}

pub fn apply_recipe(s: &mut Session, recipe: &Value) -> crate::Result<Value> {
    if recipe.get("schema").and_then(Value::as_u64) != Some(1) {
        return Err(crate::EngineError::Other("unsupported Photos recipe".into()));
    }
    let asset =
        recipe.get("originalAssetId").and_then(Value::as_str).ok_or_else(|| crate::EngineError::Other("Photos recipe has no original".into()))?;
    let id = s.apple_library.as_ref().and_then(|l| l.asset_ids.get(asset)).copied().filter(|id| s.catalog.photo(*id).is_some()).ok_or_else(|| {
        crate::EngineError::Other("The recipe's original is unavailable in this Photos library; refresh or grant access to it".into())
    })?;
    let draft: Draft =
        serde_json::from_value(recipe.get("adjustments").cloned().unwrap_or(Value::Null)).map_err(|e| crate::EngineError::Other(e.to_string()))?;
    s.commit(
        "Reopen Photos Adjustments",
        Op::Batch {
            ops: vec![
                Op::SetDevelop { id, settings: draft.develop, label: "Photos Recipe".into(), edited: draft.edited },
                Op::SetMeta { id, meta: Box::new(draft.meta) },
                Op::SetRating { id, rating: draft.rating },
                Op::SetFlag { id, flag: draft.flag },
                Op::SetLabel { id, label: draft.label },
                Op::SetVersions { id, versions: draft.versions },
                Op::SetHistory { id, history: draft.history },
            ],
        },
    )?;
    s.source = LibrarySource::All;
    s.selection = Selection::single(id);
    Ok(json!({"id":id.0, "originalUnchanged":true}))
}

/// Demo photos keep their procedural selections; real macOS photos use Vision by default.
pub fn wants_vision(session: &Session, params: &Value) -> bool {
    cfg!(target_os = "macos")
        && matches!(params.get("kind").and_then(Value::as_str), Some("subject" | "background"))
        && params.get("seg").is_none()
        && params.get("provider").and_then(Value::as_str) != Some("heuristic")
        && (params.get("provider").and_then(Value::as_str) == Some("apple")
            || session
                .active()
                .and_then(|id| session.catalog.photo(id))
                .is_some_and(|p| matches!(p.source, lightcraft_catalog::Source::File { .. } | lightcraft_catalog::Source::ApplePhotos { .. })))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn projected(assets: &[&str], directory: Option<&Path>) -> LoadedLibrary {
        let snapshot: Snapshot = serde_json::from_value(json!({
            "authorization":"authorized",
            "assets":assets.iter().map(|id| json!({"id":id, "name":format!("{id}.jpg"), "width":8, "height":6,
                "raw":false, "favorite":false, "format":"JPEG", "captured":"2025-02-03T12:00:00Z", "revision":"r1"})).collect::<Vec<_>>(),
            "albums":[{"id":"folder", "name":"Trips", "folder":true, "photos":[]},
                {"id":"album", "name":"Weekend", "parent":"folder", "photos":assets}]
        }))
        .unwrap();
        let (drafts, warnings) = load_drafts(directory);
        LoadedLibrary { snapshot, drafts, warnings }
    }

    fn photos_session() -> Session {
        let mut s = Session::new().with_apple_photos(None);
        s.apply_photos_snapshot(projected(&["a", "b"], None)).unwrap();
        s.selection = Selection::single(s.apple_library.as_ref().unwrap().asset_ids["a"]);
        s
    }

    #[test]
    fn system_library_defines_sources_albums_and_live_membership_without_original_copies() {
        let mut s = photos_session();
        assert!(s.library.is_none());
        assert_eq!(s.catalog.len(), 2);
        assert!(s.catalog.photos().all(|p| matches!(p.source, Source::ApplePhotos { .. })));
        assert!(!s.commands().iter().find(|c| c.id == "photo.saveMetadataToFile").unwrap().enabled);
        #[cfg(target_os = "macos")]
        assert!(s.commands().iter().find(|c| c.id == "photo.saveApplePhotosCopy").unwrap().enabled);
        let album_id = s.apple_library.as_ref().unwrap().album_ids["album"];
        let folder_id = s.apple_library.as_ref().unwrap().album_ids["folder"];
        assert_eq!(s.catalog.album(album_id).unwrap().parent, Some(folder_id));
        assert_eq!(s.catalog.album_photos(album_id).len(), 2);
        let id = s.active().unwrap();
        s.execute("develop.set", &json!({"control":"light.exposure", "value":1.25})).unwrap();
        s.apply_photos_snapshot(projected(&["c", "a"], None)).unwrap();
        assert_eq!(s.active(), Some(id));
        assert_eq!(s.catalog.photo(id).unwrap().develop.light.exposure, 1.25);
        assert_eq!(s.catalog.len(), 2);
        assert!(s.catalog.photos().all(|p| p.file_name != "b.jpg"));
        assert_eq!(s.apple_library.as_ref().unwrap().album_ids["album"], album_id);
        assert_eq!(s.catalog.album_photos(album_id).len(), 2);
        assert!(matches!(s.media.origin_ref(&s.catalog.photo(id).unwrap().source, 2560), SourceRef::ApplePhotos { .. }));
    }

    #[test]
    fn refresh_rejects_bad_metadata_and_preserves_the_entire_current_projection() {
        let mut s = photos_session();
        let before = s.catalog.to_snapshot();
        let active = s.active();
        assert!(s.apply_photos_snapshot(projected(&["c", "c"], None)).is_err());
        assert_eq!(s.catalog.to_snapshot(), before);
        assert_eq!(s.active(), active);
    }

    #[test]
    fn own_catalogue_and_original_mutations_are_disabled_in_photos_mode() {
        let mut s = photos_session();
        let id = s.active().unwrap();
        for (cmd, params) in [
            ("library.import", json!({"paths":["/tmp/source.jpg"]})),
            ("album.create", json!({"name":"Own album"})),
            ("photo.delete", json!({})),
            ("photo.deletePermanently", json!({})),
            ("photo.rename", json!({"template":"changed"})),
            ("photo.duplicate", json!({})),
            ("photo.virtualCopy", json!({})),
        ] {
            assert!(s.execute(cmd, &params).is_err(), "{cmd}");
            assert!(!s.commands().iter().find(|c| c.id == cmd).unwrap().enabled, "{cmd}");
        }
        assert!(s.commit("cannot detach source", Op::SetDeleted { id, deleted: true }).is_err());
        assert!(!s.catalog.photo(id).unwrap().deleted);
        assert!(s.execute("photo.rate", &json!({"rating":3})).is_ok());
        assert_eq!(s.catalog.photo(id).unwrap().rating, 3);
        assert!(s.library.is_none());
    }

    #[test]
    fn adjustment_metadata_survives_restart_without_any_photo_catalogue_or_originals() {
        let dir = std::env::temp_dir().join(format!(
            "lightcraft-photos-edits-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let mut s = Session::new().with_apple_photos(Some(dir.clone()));
        s.apply_photos_snapshot(projected(&["a"], Some(&dir))).unwrap();
        let id = s.apple_library.as_ref().unwrap().asset_ids["a"];
        s.selection = Selection::single(id);
        let original = s.catalog.photo(id).unwrap().source.clone();
        s.execute("develop.set", &json!({"control":"light.exposure", "value":1.25})).unwrap();
        {
            let _fault = lightcraft_catalog::safe_file::fail_writes_after(0);
            assert!(matches!(s.execute("photo.rate", &json!({"rating":4})), Err(crate::EngineError::NotSaved(_))));
        }
        assert!(s.unsaved().is_some());
        assert_eq!(s.catalog.photo(id).unwrap().source, original);
        s.persist().unwrap();
        assert!(s.unsaved().is_none());
        s.close_library().unwrap();
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].extension().unwrap(), "json");
        let data = std::fs::read_to_string(&files[0]).unwrap();
        assert!(!data.contains("\"source\""));
        assert!(!data.contains("albums"));
        assert!(!dir.join("catalog.log").exists());
        assert!(!dir.join("catalog.snap").exists());
        assert!(!dir.join("Originals").exists());
        let mut resumed = Session::new().with_apple_photos(Some(dir.clone()));
        resumed.apply_photos_snapshot(projected(&["a"], Some(&dir))).unwrap();
        let p = resumed.catalog.photos().next().unwrap();
        assert_eq!(p.develop.light.exposure, 1.25);
        assert_eq!(p.rating, 4);
        assert_eq!(p.source, original);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn copy_recipe_and_full_size_render_keep_the_source_unchanged_without_calling_photos() {
        let mut s = photos_session();
        let id = s.active().unwrap();
        let image = lightcraft_raster::Rgb32f { width: 8, height: 6, data: vec![[0.1, 0.2, 0.3]; 48] };
        for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
            s.media.insert(id, level, Arc::new(image.clone()));
        }
        s.execute("develop.set", &json!({"control":"light.exposure", "value":1.0})).unwrap();
        let original = s.catalog.photo(id).unwrap().source.clone();
        let check = s.export_job(id, 8, 6, lightcraft_pipeline::OutputSpace::Srgb, lightcraft_pipeline::OutputDepth::U8).unwrap();
        assert!(matches!(check.source, SourceRef::Loaded(_)), "never authorize Photos in a fixture test");
        let job = prepare_copy(&mut s, &json!({})).unwrap();
        assert_eq!(job.asset, "a");
        assert_eq!(job.recipe["originalAssetId"], "a");
        assert_eq!(job.recipe["adjustments"]["develop"]["light"]["exposure"], 1.0);
        let recipe = job.recipe.clone();
        let rendered = job.render.run().unwrap();
        assert_eq!((rendered.width, rendered.height), (8, 6));
        assert!(rendered.bytes.starts_with(&[0xff, 0xd8]));
        assert_eq!(s.catalog.photo(id).unwrap().source, original);
        assert!(lightcraft_apple::save_photos_copy("another", "copy.jpg", &rendered.bytes, &recipe).is_err());
        let before = s.catalog.len();
        apply_recipe(&mut s, &recipe).unwrap();
        assert_eq!(s.catalog.len(), before);
        assert_eq!(s.active(), Some(id));
        assert_eq!(s.catalog.photo(id).unwrap().source, original);
    }

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
