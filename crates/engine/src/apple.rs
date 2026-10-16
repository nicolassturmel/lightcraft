//! Apple Photos is the source of truth. Only adjustment metadata belongs to LightCraft.
use crate::{
    LibrarySource, Selection, Session,
    media::{DecodedSource, SourceLevel, SourceRef},
};
use lightcraft_catalog::{Album, AlbumId, Catalog, ColorLabel, Flag, HistoryStep, MediaKind, Meta, Op, Photo, PhotoId, Source, Version};
use lightcraft_develop::{DevelopSettings, MaskShape, SegMask};
use lightcraft_geom::Point;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

mod changes;
pub use changes::{PhotosChanges, PhotosUpdate, PhotosWatch};
mod originals;
pub use originals::{CachedOriginal, OriginalCache};

pub fn services_status() -> Value {
    #[cfg(target_os = "macos")]
    return lightcraft_apple::services_status();
    #[cfg(not(target_os = "macos"))]
    json!({"state":"unavailable"})
}

pub fn reset_services() {
    #[cfg(target_os = "macos")]
    lightcraft_apple::reset_services();
}

pub fn shutdown_services() {
    #[cfg(target_os = "macos")]
    lightcraft_apple::shutdown_services();
}

#[cfg(target_os = "macos")]
pub use lightcraft_apple::PHOTOS_ORIGINAL_REQUIRED as ORIGINAL_REQUIRED;
#[cfg(not(target_os = "macos"))]
pub const ORIGINAL_REQUIRED: &str = "Click Download original for this photo before editing. Selecting a photo never retrieves its original.";

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
    /// Last accepted PhotoKit metadata only. Edits and decoded-original caches are independent.
    snapshot_key: Option<lightcraft_preview::Hash128>,
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

#[derive(Deserialize, Serialize)]
pub struct Snapshot {
    pub authorization: String,
    pub assets: Vec<Asset>,
    pub albums: Vec<PhotosAlbum>,
}

#[derive(Deserialize, Serialize)]
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

#[derive(Deserialize, Serialize)]
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
    originals: Option<Arc<OriginalCache>>,
}

impl LoadedLibrary {
    /// A transient metadata response. The native loader also attaches on-disk adjustment drafts.
    pub fn metadata(snapshot: Snapshot) -> Self {
        Self { snapshot, drafts: BTreeMap::new(), warnings: vec![], originals: None }
    }
}

fn snapshot_key(snapshot: &Snapshot) -> Result<lightcraft_preview::Hash128, String> {
    // Hash the structured metadata without allocating another copy of a large library response.
    struct Digest(lightcraft_preview::Hasher128);
    impl std::io::Write for Digest {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut digest = Digest(lightcraft_preview::Hasher128::new());
    serde_json::to_writer(&mut digest, snapshot).map_err(|e| e.to_string())?;
    Ok(digest.0.finish())
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
        Ok(LoadedLibrary { snapshot, drafts, warnings, originals: directory.map(OriginalCache::scan) })
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
        self.media.photos_originals = None;
        self.media.rendered.retire();
        self.media.rendered = Arc::new(lightcraft_preview::PreviewCache::memory(self.media.budget() / 3));
        self.media.smart_dir = None;
        self.visible_key = None;
        self.total = None;
        self.library_generation = crate::LIBRARY_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.library_identity = Arc::new(());
        self.media.ai_assets = Arc::new(crate::ai_remove::AiAssets::new(directory.as_ref().map(|d| d.join("AI Remove"))));
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
        let snapshot_key = snapshot_key(&loaded.snapshot)?;
        if current.loaded && current.snapshot_key == Some(snapshot_key) {
            if let Some(state) = self.apple_library.as_mut() {
                state.error = None;
            }
            return Ok(
                json!({"backend":"applePhotos", "photos":self.catalog.len(), "albums":self.catalog.albums().count(), "changed":false, "warnings":loaded.warnings}),
            );
        }
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
        if self.media.photos_originals.is_none() {
            self.media.photos_originals = loaded.originals;
        }
        state.asset_ids = ids;
        state.album_ids = albums;
        state.drafts = drafts;
        state.authorization = loaded.snapshot.authorization;
        state.loaded = true;
        state.snapshot_key = Some(snapshot_key);
        state.error = None;
        self.pending_log.clear();
        self.visible_key = None;
        self.total = None;
        Ok(
            json!({"backend":"applePhotos", "photos":self.catalog.len(), "albums":self.catalog.albums().count(), "changed":true, "warnings":loaded.warnings}),
        )
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
    if !network {
        return Err(ORIGINAL_REQUIRED.into());
    }
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

pub fn preview_photo(asset: &str, max_edge: usize) -> Result<lightcraft_raster::Rgba8, String> {
    #[cfg(target_os = "macos")]
    {
        let decoded = lightcraft_apple::photos_preview(asset, max_edge)?;
        let encode = |v: f32| (lightcraft_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8;
        Ok(lightcraft_raster::Rgba8 {
            width: decoded.metadata.width,
            height: decoded.metadata.height,
            data: decoded
                .pixels
                .into_iter()
                .map(|[r, g, b, a]| [encode(r), encode(g), encode(b), (a.clamp(0.0, 1.0) * 255.0).round() as u8])
                .collect(),
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (asset, max_edge);
        Err("Apple Photos previews are available only on macOS".into())
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
    ai_assets: Arc<crate::ai_remove::AiAssets>,
}

/// An explicit request for one original. Normal source refs never have network access.
pub struct DownloadJob {
    pub id: PhotoId,
    generation: u64,
    origin: Source,
    source: SourceRef,
    cache: Option<Arc<OriginalCache>>,
}

pub struct DownloadedOriginal {
    full: DecodedSource,
    preview: DecodedSource,
    thumb: DecodedSource,
}

impl DownloadedOriginal {
    fn from_source(full: DecodedSource) -> Self {
        let reduced = |edge| {
            let image = lightcraft_raster::resample::fit(&full.image, edge, edge, lightcraft_raster::resample::Filter::Box);
            DecodedSource::new(Arc::new(image), full.info.clone())
        };
        Self { preview: reduced(SourceLevel::Preview.max_edge()), thumb: reduced(SourceLevel::Thumb.max_edge()), full }
    }
}

pub fn prepare_download(s: &Session, params: &Value) -> Result<DownloadJob, String> {
    primary(s)?;
    let id = match params.get("id") {
        Some(value) => PhotoId(value.as_u64().ok_or("Photo id must be an unsigned integer")?),
        None => s.active().ok_or("Select a photo first")?,
    };
    let photo = s.catalog.photo(id).ok_or("This Photos image is no longer available")?;
    let Source::ApplePhotos { asset, .. } = &photo.source else { return Err("Select an Apple Photos image".into()) };
    Ok(DownloadJob {
        id,
        generation: s.library_generation(),
        origin: photo.source.clone(),
        source: SourceRef::ApplePhotos { asset: asset.clone(), max_edge: SourceLevel::Full.max_edge(), network: true },
        cache: s.media.photos_originals.clone(),
    })
}

impl DownloadJob {
    pub fn run(&self) -> Result<DownloadedOriginal, String> {
        let full = match self.cache.as_ref().and_then(|cache| cache.get(&self.origin)) {
            Some(cached) => cached.load(usize::MAX).or_else(|_| self.source.load_source())?,
            None => self.source.load_source()?,
        };
        if let Some(cache) = &self.cache {
            cache.store(&self.origin, &full)?;
        }
        Ok(DownloadedOriginal::from_source(full))
    }
}

pub fn accept_download(s: &mut Session, job: &DownloadJob, downloaded: DownloadedOriginal) -> Result<Value, String> {
    if s.library_generation() != job.generation || s.catalog.photo(job.id).is_none_or(|p| p.source != job.origin) {
        return Err("The Photos original changed during download; refresh and retry".into());
    }
    let (width, height) = (downloaded.full.image.width, downloaded.full.image.height);
    s.media.insert_source(job.id, SourceLevel::Thumb, downloaded.thumb);
    s.media.insert_source(job.id, SourceLevel::Preview, downloaded.preview);
    s.media.insert_source(job.id, SourceLevel::Full, downloaded.full);
    s.catalog.revision = s.catalog.revision.wrapping_add(1);
    Ok(json!({"id":job.id.0, "width":width, "height":height, "originalUnchanged":true}))
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
    let options =
        crate::export::ExportOptions { format: crate::export::ExportFormat::Jpeg, quality: 100, resize: None, limit_kb: None, ..Default::default() };
    let render = crate::export::prepare_export(s, id, &options, 1)?;
    Ok(CopyJob { render, asset, recipe, ai_assets: s.media.ai_assets.clone() })
}

impl CopyJob {
    pub fn run(mut self) -> Result<Value, String> {
        // Keep the edit-copy recipe self-contained, including repairs needed by undo/history.
        let draft: Draft = serde_json::from_value(self.recipe["adjustments"].clone()).map_err(|e| e.to_string())?;
        let mut repairs = std::collections::BTreeMap::new();
        for settings in
            std::iter::once(&draft.develop).chain(draft.history.iter().map(|h| &h.settings)).chain(draft.versions.iter().map(|v| &v.settings))
        {
            for patch in settings.spots.iter().filter_map(|s| s.ai_patch.as_ref()) {
                if repairs.contains_key(&patch.id) {
                    continue;
                }
                let bytes = self.ai_assets.export_asset(patch)?;
                repairs.insert(patch.id.clone(), bytes);
                if repairs.values().map(Vec::len).sum::<usize>() > 4_000_000 {
                    return Err("AI Remove history is too large for a Photos recipe; export an image and keep the local adjustment folder".into());
                }
            }
        }
        if !repairs.is_empty() {
            self.recipe["aiRemoveAssets"] = serde_json::to_value(repairs).map_err(|e| e.to_string())?;
        }
        if serde_json::to_vec(&self.recipe).map_err(|e| e.to_string())?.len() > 16_000_000 {
            return Err("Photos copy recipe exceeds the size limit".into());
        }
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
    if let Some(assets) = recipe.get("aiRemoveAssets") {
        if serde_json::to_vec(assets).map_err(|e| crate::EngineError::Other(e.to_string()))?.len() > 16_000_000 {
            return Err(crate::EngineError::Other("Oversized AI Remove recipe".into()));
        }
        let assets: std::collections::BTreeMap<String, Vec<u8>> =
            serde_json::from_value(assets.clone()).map_err(|e| crate::EngineError::Other(e.to_string()))?;
        if assets.len() > 256 {
            return Err(crate::EngineError::Other("Too many AI Remove assets".into()));
        }
        for (id, bytes) in assets {
            let patch = s.media.ai_assets.import_asset(&bytes).map_err(crate::EngineError::Other)?;
            if patch.id != id {
                return Err(crate::EngineError::Other("AI Remove asset identity mismatch".into()));
            }
        }
    }
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

pub const VISION_DESCRIBE_UNSUPPORTED: &str = "Apple Vision does not support masks from text descriptions. Use Object, Subject, Background or Brush.";

/// Real macOS libraries use the system mask provider, without preparing or downloading SAM.
/// Procedural demo sessions keep the portable mask paths for cross-platform UI tests.
pub fn uses_vision(session: &Session) -> bool {
    cfg!(target_os = "macos")
        && (session.apple_library.is_some()
            || session
                .active()
                .and_then(|id| session.catalog.photo(id))
                .is_some_and(|p| matches!(p.source, Source::File { .. } | Source::ApplePhotos { .. })))
}

/// Whether creation needs asynchronous Vision work. An empty Object selection waits for clicks.
pub fn wants_vision(session: &Session, params: &Value) -> bool {
    cfg!(target_os = "macos")
        && (matches!(params.get("kind").and_then(Value::as_str), Some("subject" | "background" | "sky"))
            || (params.get("kind").and_then(Value::as_str) == Some("object")
                && params.get("points").and_then(Value::as_array).is_some_and(|points| !points.is_empty())))
        && params.get("seg").is_none()
        && params.get("provider").and_then(Value::as_str) != Some("heuristic")
        && (params.get("provider").and_then(Value::as_str) == Some("apple") || uses_vision(session))
}

pub fn vision_source(session: &mut Session) -> Result<SourceRef, String> {
    let id = session.active().ok_or("no active photo")?;
    let photo = session.catalog.photo(id).ok_or("no such photo")?.clone();
    Ok(session.media.source_ref(&photo, SourceLevel::Preview))
}

pub fn vision_selection(source: SourceRef) -> Result<SegMask, String> {
    vision_mask(source, None)
}

pub fn vision_sky_selection(source: SourceRef) -> Result<SegMask, String> {
    #[cfg(target_os = "macos")]
    {
        let decoded = source.load_source()?;
        // Prefer a source's genuine semantic sky matte (e.g. ProRAW) when it carries one.
        if let Some(mattes) = decoded.info.as_ref().and_then(|i| i.mattes.as_deref())
            && mattes.has_sky()
        {
            let image = lightcraft_raster::resample::fit(&decoded.image, 512, 512, lightcraft_raster::resample::Filter::Box);
            let frame = lightcraft_pipeline::geometry::Frame::new(image.width, image.height, &DevelopSettings::default(), true);
            let log_l = image.map(lightcraft_pipeline::local::log_lum);
            let mask = lightcraft_pipeline::masks::shape_alpha(&MaskShape::Sky, &frame, image.width, image.height, &image, &log_l, 0.0, Some(mattes));
            let mask = lightcraft_raster::resample::resize(&mask, 512, 512, lightcraft_raster::resample::Filter::Bilinear);
            return coverage_to_seg(512, &mask.data);
        }
        let image = lightcraft_raster::resample::fit(&decoded.image, 2048, 2048, lightcraft_raster::resample::Filter::Box);
        let semantic = lightcraft_apple::semantic_sky_mask(image.width, image.height, &image.data, 512)?;
        let foreground = lightcraft_apple::foreground_mask_or_empty(image.width, image.height, &image.data, 512)?;
        let mask = crate::sky::selection(
            &image,
            &lightcraft_raster::Plane { width: 512, height: 512, data: semantic },
            &lightcraft_raster::Plane { width: 512, height: 512, data: foreground },
        )?;
        coverage_to_seg(512, &mask.data)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = source;
        Err("Apple Vision is available only on macOS".into())
    }
}

pub fn vision_object_selection(source: SourceRef, include: &[Point], exclude: &[Point]) -> Result<SegMask, String> {
    vision_mask(source, Some((include, exclude)))
}

fn vision_mask(source: SourceRef, points: Option<(&[Point], &[Point])>) -> Result<SegMask, String> {
    #[cfg(target_os = "macos")]
    {
        let image = source.load()?;
        let image = lightcraft_raster::resample::fit(&image, 2048, 2048, lightcraft_raster::resample::Filter::Box);
        let coverage = match points {
            None => lightcraft_apple::foreground_mask(image.width, image.height, &image.data, 512)?,
            Some((include, exclude)) => {
                let include: Vec<_> = include.iter().map(|p| [p.x, p.y]).collect();
                let exclude: Vec<_> = exclude.iter().map(|p| [p.x, p.y]).collect();
                lightcraft_apple::foreground_instances(image.width, image.height, &image.data, 512, &include, &exclude)?
            }
        };
        coverage_to_seg(512, &coverage)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (source, points);
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
        let source = vision_source(session)?;
        if let MaskShape::Object { hint, exclude, seg, .. } = shape {
            *seg = Some(vision_object_selection(source, hint, exclude)?);
        } else if params.get("kind").and_then(Value::as_str) == Some("sky") {
            *shape = MaskShape::SkySelection { seg: vision_sky_selection(source)?, edge: 0.0 };
        } else {
            let seg = vision_selection(source)?;
            *shape = MaskShape::Vision { background: params.get("kind").and_then(Value::as_str) == Some("background"), seg, edge: 0.0 };
        }
    }
    Ok(())
}

/// Refresh only a saved sky component. Its image, settings and library revision are pinned
/// until acceptance; other components and local adjustments remain part of the same recipe.
pub struct VisionSkyUpdate {
    source: SourceRef,
    photo: PhotoId,
    generation: u64,
    content: String,
    mask: u32,
    component: usize,
    before: Arc<DevelopSettings>,
    edge: f64,
}

impl VisionSkyUpdate {
    pub fn prepare(session: &mut Session, params: &Value) -> Result<Self, String> {
        let photo = session.active().ok_or("no active photo")?;
        let mask = match params.get("id") {
            Some(id) => id.as_u64().and_then(|id| u32::try_from(id).ok()).ok_or("invalid mask id")?,
            None => session.active_mask.ok_or("no mask selected")?,
        };
        let before = session.develop_of(photo).ok_or("no active photo settings")?;
        let selected = before.masks.iter().find(|m| m.id == mask).ok_or("the selected mask no longer exists")?;
        let component = match params.get("component") {
            Some(index) => index.as_u64().and_then(|i| usize::try_from(i).ok()).ok_or("invalid sky component index")?,
            None => selected
                .components
                .iter()
                .rposition(|c| matches!(c.shape, MaskShape::Sky | MaskShape::SkySelection { .. }))
                .ok_or("the mask has no Sky selection")?,
        };
        let edge = match selected.components.get(component).map(|c| &c.shape) {
            Some(MaskShape::SkySelection { edge, .. }) => *edge,
            Some(MaskShape::Sky) => 0.0,
            _ => return Err("not a Sky component".into()),
        };
        let source = vision_source(session)?;
        let content = crate::media::content_key(session.catalog.photo(photo).ok_or("no such photo")?);
        Ok(Self { source, photo, generation: session.library_generation(), content, mask, component, before, edge })
    }

    pub fn run(&self) -> Result<SegMask, String> {
        vision_sky_selection(self.source.clone())
    }

    pub fn accept(self, session: &mut Session, seg: SegMask) -> Result<Value, String> {
        let current = session.catalog.photo(self.photo).map(|p| crate::media::content_key(p));
        if session.library_generation() != self.generation
            || session.active() != Some(self.photo)
            || current.as_ref() != Some(&self.content)
            || !session.develop_of(self.photo).is_some_and(|d| Arc::ptr_eq(&d, &self.before))
        {
            return Err("Apple Vision selection discarded because the photo or mask changed".into());
        }
        if seg.logits().is_none() {
            return Err("invalid Apple Vision sky mask".into());
        }
        let shape = MaskShape::SkySelection { seg, edge: self.edge };
        session.execute("mask.update", &json!({"id":self.mask, "component":self.component, "shape":shape})).map_err(|e| e.to_string())?;
        Ok(json!({"updated":true, "provider":"appleVision"}))
    }
}

/// A click's immutable target. Results must not overwrite a changed photo, mask, undo or library.
pub struct VisionObjectClick {
    source: SourceRef,
    photo: PhotoId,
    generation: u64,
    content: String,
    mask: u32,
    component: usize,
    before: Arc<DevelopSettings>,
    hint: Vec<Point>,
    exclude: Vec<Point>,
    edge: f64,
}

impl VisionObjectClick {
    pub fn prepare(session: &mut Session, params: &Value) -> Result<Self, String> {
        let coordinate = |key| params.get(key).and_then(Value::as_f64).filter(|v| v.is_finite() && (0.0..=1.0).contains(v));
        let at = Point::new(
            coordinate("x").ok_or("Object click x must be within the photo")?,
            coordinate("y").ok_or("Object click y must be within the photo")?,
        );
        let photo = session.active().ok_or("no active photo")?;
        let mask = match params.get("id") {
            Some(id) => id.as_u64().and_then(|id| u32::try_from(id).ok()).ok_or("invalid mask id")?,
            None => session.active_mask.ok_or("no mask selected")?,
        };
        let before = session.develop_of(photo).ok_or("no active photo settings")?;
        let selected = before.masks.iter().find(|m| m.id == mask).ok_or("the selected mask no longer exists")?;
        let component = selected
            .components
            .iter()
            .rposition(|c| matches!(c.shape, MaskShape::Object { .. }))
            .ok_or("the mask has no Object selection (add one first)")?;
        let Some(MaskShape::Object { hint, exclude, edge, .. }) = selected.components.get(component).map(|c| &c.shape) else {
            return Err("not an Object component".into());
        };
        if hint.len().saturating_add(exclude.len()) >= crate::segment::MAX_CLICKS {
            return Err(format!("an Object selection takes at most {} clicks; start a new one", crate::segment::MAX_CLICKS));
        }
        let (mut hint, mut exclude, edge) = (hint.clone(), exclude.clone(), *edge);
        if params.get("exclude").and_then(Value::as_bool).unwrap_or(false) {
            exclude.push(at)
        } else {
            hint.push(at)
        }
        let source = vision_source(session)?;
        let content = crate::media::content_key(session.catalog.photo(photo).ok_or("no such photo")?);
        Ok(Self { source, photo, generation: session.library_generation(), content, mask, component, before, hint, exclude, edge })
    }

    pub fn run(&self) -> Result<Option<SegMask>, String> {
        if self.hint.is_empty() { Ok(None) } else { vision_object_selection(self.source.clone(), &self.hint, &self.exclude).map(Some) }
    }

    pub fn accept(self, session: &mut Session, seg: Option<SegMask>) -> Result<Value, String> {
        let current = session.catalog.photo(self.photo).map(|p| crate::media::content_key(p));
        if session.library_generation() != self.generation
            || session.active() != Some(self.photo)
            || current.as_ref() != Some(&self.content)
            || !session.develop_of(self.photo).is_some_and(|d| Arc::ptr_eq(&d, &self.before))
        {
            return Err("Apple Vision selection discarded because the photo or mask changed".into());
        }
        if seg.as_ref().is_some_and(|s| s.logits().is_none()) || (!self.hint.is_empty() && seg.is_none()) {
            return Err("invalid Apple Vision object mask".into());
        }
        let out = json!({"include":self.hint.len(), "exclude":self.exclude.len(), "provider":"appleVision"});
        let shape = MaskShape::Object { hint: self.hint, exclude: self.exclude, seg, detail: vec![], edge: self.edge };
        session.execute("mask.update", &json!({"id":self.mask, "component":self.component, "shape":shape})).map_err(|e| e.to_string())?;
        Ok(out)
    }
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
        LoadedLibrary { snapshot, drafts, warnings, originals: directory.map(OriginalCache::scan) }
    }

    fn photos_session() -> Session {
        let mut s = Session::new().with_apple_photos(None);
        s.apply_photos_snapshot(projected(&["a", "b"], None)).unwrap();
        s.selection = Selection::single(s.apple_library.as_ref().unwrap().asset_ids["a"]);
        s
    }

    #[test]
    fn ai_repairs_reopen_from_local_photos_descriptions_and_embedded_copy_recipes() {
        use lightcraft_develop::{AiPatch, Spot, SpotMode};
        use lightcraft_geom::{Point, Rect};
        use lightcraft_preview::{Hasher128, next_tick};
        let dir = std::env::temp_dir().join(format!("lc-photos-ai-{}-{}", std::process::id(), next_tick()));
        let mut s = Session::new().with_apple_photos(Some(dir.clone()));
        s.apply_photos_snapshot(projected(&["a"], Some(&dir))).unwrap();
        let id = s.apple_library.as_ref().unwrap().asset_ids["a"];
        s.selection = Selection::single(id);
        let key = Hasher128::new().str(&crate::media::content_key(s.catalog.photo(id).unwrap())).finish();
        let patch = s
            .media
            .ai_assets
            .store(
                AiPatch {
                    id: String::new(),
                    model: "test".into(),
                    source: key.to_string(),
                    bounds: Rect::UNIT,
                    width: 1,
                    height: 1,
                    stroke: vec![Point::new(0.5, 0.5)],
                    radius: 0.02,
                },
                vec![[0.5, 0.3, 0.2, 1.0]],
            )
            .unwrap();
        let mut dd = (*s.develop_of(id).unwrap()).clone();
        dd.spots.push(Spot { mode: SpotMode::Ai, ai_patch: Some(patch.clone()), ..Default::default() });
        s.set_develop(id, dd, "AI Remove").unwrap();
        s.persist().unwrap();
        let bytes = s.media.ai_assets.export_asset(&patch).unwrap();
        let recipe =
            json!({"schema":1,"originalAssetId":"a","adjustments":Draft::of(s.catalog.photo(id).unwrap()),"aiRemoveAssets":{patch.id.clone():bytes}});
        let base = Arc::new(lightcraft_raster::Rgb32f::filled(8, 6, [0.1; 3]));
        let mut reopened = Session::new().with_apple_photos(Some(dir.clone()));
        reopened.apply_photos_snapshot(projected(&["a"], Some(&dir))).unwrap();
        let d = reopened.develop_of(id).unwrap();
        assert_eq!(reopened.media.ai_assets.composite(&base, &d.spots, Some(key)).unwrap().data[0], [0.5, 0.3, 0.2]);
        let mut fresh = photos_session();
        apply_recipe(&mut fresh, &recipe).unwrap();
        let d = fresh.develop_of(id).unwrap();
        assert_eq!(fresh.media.ai_assets.composite(&base, &d.spots, Some(key)).unwrap().data[0], [0.5, 0.3, 0.2]);
        assert_eq!(base.data, vec![[0.1; 3]; 48]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn photos_browsing_pixels_are_cached_by_source_across_edits_sizes_and_other_photos() {
        let mut s = photos_session();
        let id = s.active().unwrap();
        let job = s.thumb_job(id, 256).unwrap();
        let quick = s.photos_grid_preview_job(&job).unwrap();
        let source = quick.photos_preview.as_ref().unwrap();
        let source_key = source.cache_key;
        let image = Arc::new(lightcraft_raster::Rgba8 { width: 8, height: 6, data: vec![[80, 100, 120, 255]; 48] });
        source.cache.put(source_key, image.clone());
        assert_eq!(quick.run().rendered.unwrap().image, *image);
        s.execute("photo.rate", &json!({"ids":[PhotoId(2).0],"rating":3})).unwrap();
        let untouched = s.thumb_job(id, 256).unwrap();
        assert_eq!(
            s.photos_grid_preview_job(&untouched).unwrap().key,
            job.key ^ 0x7fb4_85c2_142d_e5a9,
            "unrelated catalogue changes never alter this preview"
        );
        for (value, size) in [(0.25, 128), (0.5, 384), (1.0, 512)] {
            s.execute("develop.set", &json!({"control":"light.exposure","value":value})).unwrap();
            let job = s.thumb_job(id, size).unwrap();
            let quick = s.photos_grid_preview_job(&job).unwrap();
            assert_eq!(quick.photos_preview.as_ref().unwrap().cache_key, source_key);
            let result = quick.run();
            assert_eq!(result.quick, Some(crate::media::QuickSource::PhotosPreview));
            assert_eq!(result.rendered.unwrap().image, *image, "a seeded cache must work without PhotoKit access");
            assert!(result.loaded.is_none());
            for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
                assert!(!s.media.has_source(id, level));
            }
        }
        let old_job = s.thumb_job(id, 256).unwrap();
        let old = s.photos_grid_preview_job(&old_job).unwrap();
        let mut changed = projected(&["a"], None).snapshot.assets;
        changed[0].revision = "r2".into();
        s.apply_photos_changes(PhotosChanges { assets: changed, ..Default::default() }).unwrap();
        let changed_job = s.thumb_job(id, 256).unwrap();
        let new = s.photos_grid_preview_job(&changed_job).unwrap();
        assert_ne!(new.photos_preview.as_ref().unwrap().cache_key, source_key);
        old.photos_preview.as_ref().unwrap().cache.clear();
        assert!(old.run().rendered.err().unwrap().contains("cache was cleared"));
    }

    #[test]
    fn repeated_develop_changes_render_from_the_cached_original_without_photokit() {
        let mut s = photos_session();
        let id = s.active().unwrap();
        let original = Arc::new(lightcraft_raster::Rgb32f { width: 8, height: 6, data: vec![[0.1, 0.2, 0.3]; 48] });
        s.media.insert(id, SourceLevel::Full, original.clone());
        let mut previous = None;
        for value in [0.25, 0.5, 0.75, 1.0, 1.25] {
            s.execute("develop.set", &json!({"control":"light.exposure","value":value})).unwrap();
            let job = s.thumb_job(id, 128).unwrap();
            assert!(s.photos_grid_preview_job(&job).is_none(), "genuine decoded pixels must take priority over Photos thumbnails");
            assert!(matches!(job.source, SourceRef::Loaded(_)));
            let pixels = job.run().rendered.unwrap().image;
            assert_ne!(Some(pixels.clone()), previous);
            previous = Some(pixels);
            assert!(Arc::ptr_eq(&original, &s.media.get(id, SourceLevel::Full).unwrap()));
        }
    }

    #[test]
    fn exporting_a_downloaded_photo_uses_its_disk_backing_after_ram_eviction_and_restart() {
        let dir = std::env::temp_dir().join(format!("lc-photos-export-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = Session::new().with_apple_photos(Some(dir.clone()));
        let mut snapshot = projected(&["a", "b"], Some(&dir));
        snapshot.snapshot.assets[0].width = 3000;
        snapshot.snapshot.assets[0].height = 2;
        s.apply_photos_snapshot(snapshot).unwrap();
        let (a, b) = (PhotoId(1), PhotoId(2));
        let original_source = s.catalog.photo(a).unwrap().source.clone();
        let original = DecodedSource::new(
            Arc::new(lightcraft_raster::Rgb32f { width: 3000, height: 2, data: vec![[0.1, 0.2, 0.3]; 6000] }),
            Some(lightcraft_pipeline::SourceInfo { raw: true, ..Default::default() }),
        );
        s.media.photos_originals.as_ref().unwrap().store(&original_source, &original).unwrap();
        s.media.insert_source(a, SourceLevel::Full, original.clone());
        s.media.insert(a, SourceLevel::Preview, Arc::new(lightcraft_raster::Rgb32f { width: 20, height: 1, data: vec![[0.1, 0.2, 0.3]; 20] }));
        s.media.insert_source(b, SourceLevel::Full, original);
        assert!(!s.media.has_source(a, SourceLevel::Full), "another photo took the sole full-resolution RAM slot");
        assert!(s.media.has_photos_original(s.catalog.photo(a).unwrap()));
        let job = s.export_job(a, 3000, 2, lightcraft_pipeline::OutputSpace::Srgb, lightcraft_pipeline::OutputDepth::U8).unwrap();
        assert!(matches!(job.source, SourceRef::PhotosCached { .. }), "export must use lossless backing, never the small preview or PhotoKit");
        let options = crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() };
        let exported = crate::export::prepare_export(&mut s, a, &options, 1).unwrap().run().unwrap();
        assert_eq!((exported.width, exported.height), (3000, 2));
        assert_eq!(s.catalog.photo(a).unwrap().source, original_source);
        drop(s);
        let mut reopened = Session::new().with_apple_photos(Some(dir.clone()));
        let mut snapshot = projected(&["a", "b"], Some(&dir));
        snapshot.snapshot.assets[0].width = 3000;
        snapshot.snapshot.assets[0].height = 2;
        reopened.apply_photos_snapshot(snapshot).unwrap();
        let again = crate::export::prepare_export(&mut reopened, a, &options, 1).unwrap().run().unwrap();
        assert_eq!(again.bytes, exported.bytes, "reopening must reconstruct the same full-size PNG without native access");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn incremental_changes_keep_unaffected_pixels_edits_identity_and_undo() {
        let mut s = photos_session();
        let a = s.active().unwrap();
        s.execute("develop.set", &json!({"control":"light.exposure","value":1.25})).unwrap();
        let before = s.catalog.photo(a).unwrap().clone();
        let pixels = Arc::new(lightcraft_raster::Rgb32f { width: 8, height: 6, data: vec![[0.1, 0.2, 0.3]; 48] });
        s.media.insert(a, SourceLevel::Full, pixels.clone());
        let mut incoming = projected(&["b", "c"], None).snapshot;
        incoming.assets[0].name = "renamed.jpg".into();
        incoming.albums[1].name = "Changed album".into();
        incoming.albums[1].photos.push("a".into());
        let changes = PhotosChanges { assets: incoming.assets, removed: vec![], albums: Some(incoming.albums) };
        assert_eq!(s.apply_photos_changes(changes).unwrap()["changed"], true);
        assert_eq!(s.catalog.len(), 3);
        assert_eq!(s.catalog.photo(s.apple_library.as_ref().unwrap().asset_ids["b"]).unwrap().file_name, "renamed.jpg");
        assert!(Arc::ptr_eq(&before, s.catalog.photo(a).unwrap()), "unrelated photo records must keep their identity");
        assert!(Arc::ptr_eq(&pixels, &s.media.get(a, SourceLevel::Full).unwrap()));
        assert_eq!(s.active(), Some(a));
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.develop_of(a).unwrap().light.exposure, 0.0);
    }

    #[test]
    fn empty_and_identical_change_events_do_not_invalidate_the_catalogue() {
        let mut s = photos_session();
        let before = s.catalog.clone();
        let revision = before.revision;
        assert_eq!(s.apply_photos_changes(PhotosChanges::default()).unwrap()["changed"], false);
        let incoming = projected(&["a", "b"], None).snapshot;
        let changes = PhotosChanges { assets: incoming.assets, removed: vec![], albums: Some(incoming.albums) };
        assert_eq!(s.apply_photos_changes(changes).unwrap()["changed"], false);
        assert_eq!(s.catalog.revision, revision);
        assert!(Arc::ptr_eq(s.catalog.photo(PhotoId(1)).unwrap(), before.photo(PhotoId(1)).unwrap()));
    }

    #[test]
    fn malformed_incremental_batches_are_atomic() {
        let mut s = photos_session();
        let before = s.catalog.clone();
        let mut assets = projected(&["c", "d"], None).snapshot.assets;
        assets[1].width = 0;
        assert!(s.apply_photos_changes(PhotosChanges { assets, removed: vec!["b".into()], albums: None }).is_err());
        assert_eq!(s.catalog, before);
        assert!(Arc::ptr_eq(s.catalog.photo(PhotoId(1)).unwrap(), before.photo(PhotoId(1)).unwrap()));
        let mut albums = projected(&["a"], None).snapshot.albums;
        albums[0].parent = Some("album".into());
        assert!(s.apply_photos_changes(PhotosChanges { albums: Some(albums), ..Default::default() }).is_err());
        assert_eq!(s.catalog, before);
    }

    #[test]
    fn incremental_removal_prunes_albums_and_selection_but_keeps_adjustment_drafts() {
        let mut s = photos_session();
        let a = s.active().unwrap();
        s.execute("develop.set", &json!({"control":"light.exposure","value":1.25})).unwrap();
        s.apply_photos_changes(PhotosChanges { removed: vec!["a".into()], ..Default::default() }).unwrap();
        assert_eq!(s.catalog.len(), 1);
        assert_eq!(s.active(), None);
        assert!(s.catalog.albums().all(|album| !album.photos.contains(&a)));
        s.apply_photos_changes(PhotosChanges { assets: projected(&["a"], None).snapshot.assets, ..Default::default() }).unwrap();
        assert_eq!(s.apple_library.as_ref().unwrap().asset_ids["a"], a);
        assert_eq!(s.develop_of(a).unwrap().light.exposure, 1.25);
    }

    #[test]
    fn unchanged_refresh_keeps_revision_edits_selection_and_decoded_sources() {
        let mut s = photos_session();
        let id = s.active().unwrap();
        s.execute("develop.set", &json!({"control":"light.exposure", "value":1.25})).unwrap();
        let pixels = Arc::new(lightcraft_raster::Rgb32f { width: 8, height: 6, data: vec![[0.1, 0.2, 0.3]; 48] });
        s.media.insert(id, SourceLevel::Full, pixels.clone());
        let revision = s.catalog.revision;
        let photo = s.catalog.photo(id).unwrap().clone();
        let generation = s.library_generation();
        for _ in 0..3 {
            assert_eq!(s.apply_photos_snapshot(projected(&["a", "b"], None)).unwrap()["changed"], false);
            assert_eq!(s.catalog.revision, revision);
            assert!(Arc::ptr_eq(s.catalog.photo(id).unwrap(), &photo));
            assert_eq!(s.active(), Some(id));
            assert_eq!(s.library_generation(), generation);
            assert!(Arc::ptr_eq(&s.media.get(id, SourceLevel::Full).unwrap(), &pixels));
        }
        let mut changed = projected(&["a", "b"], None);
        changed.snapshot.albums[1].name = "A renamed Photos album".into();
        assert_eq!(s.apply_photos_snapshot(changed).unwrap()["changed"], true);
        assert!(s.catalog.revision > revision);
        assert_eq!(s.catalog.photo(id).unwrap().develop.light.exposure, 1.25);
        assert!(s.media.has_source(id, SourceLevel::Full));
    }

    #[test]
    fn selection_editing_prefetch_masks_and_export_cannot_read_an_original_implicitly() {
        let mut s = photos_session();
        let id = s.active().unwrap();
        for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
            let photo = s.catalog.photo(id).unwrap().clone();
            assert_eq!(s.media.source_ref(&photo, level).load_source().err().unwrap(), ORIGINAL_REQUIRED);
        }
        s.execute("develop.set", &json!({"control":"light.exposure", "value":0.5})).unwrap();
        assert_eq!(s.render_job(id, 1200, 800, false, true).unwrap().source.load_source().err().unwrap(), ORIGINAL_REQUIRED);
        assert_eq!(s.source_now(id, SourceLevel::Preview).err().unwrap(), ORIGINAL_REQUIRED);
        assert_eq!(s.media.origin_ref(&s.catalog.photo(id).unwrap().source, 6000).load_source().err().unwrap(), ORIGINAL_REQUIRED);
    }

    #[test]
    fn library_previews_never_supply_decoded_originals_to_development_or_exports() {
        let mut s = photos_session();
        let id = s.active().unwrap();
        let job = s.thumb_job(id, 384).unwrap();
        assert!(matches!(&job.source, SourceRef::ApplePhotos { network: false, .. }));
        let preview = s.photos_grid_preview_job(&job).unwrap();
        assert_eq!(preview.photos_preview.as_ref().unwrap().asset, "a");
        assert_eq!(preview.photos_preview.as_ref().unwrap().edge, 384);
        assert!(preview.small.is_none(), "browsing must not follow the preview with a RAW decode");
        assert!(preview.embedded.is_none());
        let (cache, key) = job.cache.unwrap();
        cache.put(key, Arc::new(lightcraft_raster::Rgba8 { width: 4, height: 3, data: vec![[80, 100, 120, 255]; 12] }));
        let result = preview.run();
        assert_eq!(result.quick, Some(crate::media::QuickSource::Cached));
        assert!(result.loaded.is_none());
        s.accept(&result);
        for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
            assert!(!s.media.has_source(id, level));
            let photo = s.catalog.photo(id).unwrap().clone();
            assert!(matches!(s.media.source_ref(&photo, level), SourceRef::ApplePhotos { network: false, .. }));
        }
        assert!(matches!(s.render_job(id, 6000, 4000, false, true).unwrap().source, SourceRef::ApplePhotos { network: false, .. }));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn downloads_are_explicit_per_photo_and_never_enable_network_for_later_reads() {
        let mut s = photos_session();
        let active = s.active().unwrap();
        let other = s.apple_library.as_ref().unwrap().asset_ids["b"];
        let before = s.catalog.to_snapshot();
        let job = prepare_download(&s, &json!({"id":other.0})).unwrap();
        assert_eq!(job.id, other);
        assert!(matches!(&job.source, SourceRef::ApplePhotos { asset, network:true, .. } if asset == "b"));
        assert!(!s.media.has_source(other, SourceLevel::Thumb));
        assert!(prepare_download(&s, &json!({"id":"bad"})).is_err());
        assert!(prepare_download(&s, &json!({"id":999})).is_err());
        let image = Arc::new(lightcraft_raster::Rgb32f { width: 8, height: 6, data: vec![[1.5, 0.2, 0.3]; 48] });
        let source = DecodedSource::new(image.clone(), Some(lightcraft_pipeline::SourceInfo { raw: true, ..Default::default() }));
        let downloaded = DownloadedOriginal::from_source(source);
        let revision = s.catalog.revision;
        let result = accept_download(&mut s, &job, downloaded).unwrap();
        assert_eq!(result["width"], 8);
        assert_eq!(s.catalog.to_snapshot(), before, "downloads must not change Photos or adjustment metadata");
        assert_eq!(s.active(), Some(active));
        assert!(s.catalog.revision > revision, "failed renders must be retried after the explicit download");
        for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
            assert!(s.media.has_source(other, level));
            assert!(!s.media.has_source(active, level));
            let photo = s.catalog.photo(other).unwrap().clone();
            let SourceRef::Loaded(decoded) = s.media.source_ref(&photo, level) else { panic!("download was not cached") };
            assert!(decoded.info.as_ref().unwrap().raw);
            assert_eq!(decoded.image.data[0][0], 1.5, "linear highlights must survive the download");
        }
        for photo in s.catalog.photos().cloned().collect::<Vec<_>>() {
            assert!(matches!(s.media.origin_ref(&photo.source, 6000), SourceRef::ApplePhotos { network: false, .. }));
        }
        let old = prepare_download(&s, &json!({"id":other.0})).unwrap();
        let mut changed = projected(&["a", "b"], None);
        changed.snapshot.assets[1].revision = "new-original".into();
        s.apply_photos_snapshot(changed).unwrap();
        let decoded = DownloadedOriginal::from_source(DecodedSource::new(image.clone(), None));
        assert!(accept_download(&mut s, &old, decoded).is_err());
        assert!(!s.media.has_source(other, SourceLevel::Full));
        let old = prepare_download(&s, &json!({"id":other.0})).unwrap();
        s = s.with_apple_photos(None);
        s.apply_photos_snapshot(projected(&["a", "b"], None)).unwrap();
        assert!(accept_download(&mut s, &old, DownloadedOriginal::from_source(DecodedSource::new(image, None))).is_err());
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
        for (level, edge) in [(SourceLevel::Thumb, 1), (SourceLevel::Preview, 2), (SourceLevel::Full, 8)] {
            let pixels = lightcraft_raster::resample::fit(&image, edge, edge, lightcraft_raster::resample::Filter::Box);
            s.media.insert(id, level, Arc::new(pixels));
        }
        // This direct export must not inherit a reduced-size/low-quality disk-export preset.
        s.last_export = Some(json!({"format":"png", "width":1, "quality":1}));
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
        assert!(rendered.file_name.ends_with(".jpg"));
        let table = rendered.bytes.windows(5).position(|v| v == [0xff, 0xdb, 0x00, 0x43, 0x00]).expect("8-bit JPEG luminance table");
        assert!(rendered.bytes[table + 5..table + 69].iter().all(|q| *q == 1), "quality 100 uses unit quantization, rather than the old quality 95");
        assert_eq!(s.catalog.photo(id).unwrap().source, original);
        let other = s.apple_library.as_ref().unwrap().asset_ids["b"];
        s.media.insert(other, SourceLevel::Full, Arc::new(image));
        let targeted = prepare_copy(&mut s, &json!({"id":other.0})).unwrap();
        assert_eq!(targeted.asset, "b", "a context-menu export targets the clicked photo, not the active one");
        assert_eq!(s.active(), Some(id));
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
    fn photos_object_masks_never_prepare_or_download_sam() {
        let mut s = photos_session();
        s.segmenter.background = true;
        s.segmenter.dir = Some(PathBuf::from("/nonexistent/lightcraft-apple-no-sam"));
        assert!(uses_vision(&s));
        for kind in ["subject", "background", "sky"] {
            assert!(wants_vision(&s, &json!({"kind":kind})));
        }
        assert!(!wants_vision(&s, &json!({"kind":"object"})), "empty Object waits for clicks without loading a model");
        s.execute("mask.add", &json!({"kind":"object"})).unwrap();
        for op in ["add", "subtract", "intersect"] {
            s.execute("mask.addComponent", &json!({"kind":"object", "op":op})).unwrap();
        }
        assert!(!s.segmenter.busy() && !s.segmenter.loaded());
        assert!(s.segmenter.pending_clicks().is_none());
        assert!(!s.segmenter.download_status().running);
        assert_eq!(s.execute("segment.prepare", &json!({})).unwrap()["provider"], "appleVision");
        assert_eq!(s.execute("mask.refineDetail", &json!({})).unwrap()["started"], false);
        let status = s.execute("segment.model.status", &json!({})).unwrap();
        assert_eq!(status["provider"], "appleVision");
        assert_eq!(status["requiresDownload"], false);
        assert!(s.execute("segment.model.download", &json!({"acknowledged":true})).unwrap_err().to_string().contains("Apple Vision"));
        let before = s.develop_of(s.active().unwrap()).unwrap();
        let e = s.execute("mask.add", &json!({"kind":"prompt", "text":"a car"})).unwrap_err().to_string();
        assert_eq!(e, VISION_DESCRIBE_UNSUPPORTED);
        assert_eq!(s.develop_of(s.active().unwrap()).unwrap(), before);
        // Existing stored semantic masks remain renderable without recomputation or a model.
        let seg = coverage_to_seg(2, &[1.0, 0.0, 1.0, 0.0]).unwrap();
        s.execute("mask.add", &json!({"kind":"prompt", "text":"saved car", "seg":seg.clone()})).unwrap();
        s.execute("mask.add", &json!({"kind":"sky", "seg":seg})).unwrap();
        assert!(matches!(s.develop_of(s.active().unwrap()).unwrap().masks.last().unwrap().components[0].shape, MaskShape::SkySelection { .. }));
        assert!(!s.segmenter.busy() && !s.segmenter.loaded());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sky_updates_preserve_manual_edits_and_reject_stale_results() {
        let seg = || coverage_to_seg(2, &[1.0, 0.0, 1.0, 0.0]).unwrap();
        let setup = || {
            let mut s = photos_session();
            s.execute("mask.add", &json!({"kind":"sky", "seg":seg(), "edge":23.0, "name":"Coastal sky"})).unwrap();
            s.execute("mask.adjust", &json!({"values":{"temp":25.0,"exposure":0.7}})).unwrap();
            s.execute("mask.component", &json!({"component":0, "action":"invert"})).unwrap();
            s.execute("mask.addComponent", &json!({"kind":"brush", "op":"subtract"})).unwrap();
            s.execute("mask.brushStroke", &json!({"points":[[0.2,0.4],[0.3,0.5]]})).unwrap();
            s
        };
        let mut s = setup();
        let id = s.active().unwrap();
        let original = s.catalog.photo(id).unwrap().source.clone();
        let before = s.develop_of(id).unwrap();
        let job = VisionSkyUpdate::prepare(&mut s, &json!({"id":1,"component":0})).unwrap();
        assert!(matches!(job.source, SourceRef::ApplePhotos { network: false, .. }));
        let new_seg = coverage_to_seg(2, &[1.0, 1.0, 0.0, 0.0]).unwrap();
        assert_eq!(job.accept(&mut s, new_seg.clone()).unwrap(), json!({"updated":true,"provider":"appleVision"}));
        let mut expected = (*before).clone();
        expected.masks[0].components[0].shape = MaskShape::SkySelection { seg: new_seg, edge: 23.0 };
        let after = s.develop_of(id).unwrap();
        assert_eq!(*after, expected, "only the saved sky coverage changes");
        assert_eq!(s.catalog.photo(id).unwrap().source, original);
        assert_eq!(serde_json::from_value::<DevelopSettings>(serde_json::to_value(&expected).unwrap()).unwrap(), expected);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.develop_of(id).unwrap(), before);
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(s.develop_of(id).unwrap(), after);
        for params in [json!({"id":u64::MAX}), json!({"component":1}), json!({"component":u64::MAX}), json!({"component":-1})] {
            assert!(VisionSkyUpdate::prepare(&mut s, &params).is_err());
        }
        let job = VisionSkyUpdate::prepare(&mut s, &json!({})).unwrap();
        let mut invalid = seg();
        invalid.data.clear();
        let revision = s.catalog.revision;
        assert!(job.accept(&mut s, invalid).unwrap_err().contains("invalid"));
        assert_eq!(s.catalog.revision, revision);
        for change in 0..5 {
            let mut s = setup();
            let job = VisionSkyUpdate::prepare(&mut s, &json!({})).unwrap();
            match change {
                0 => {
                    s.execute("mask.adjust", &json!({"values":{"temp":50.0}})).unwrap();
                }
                1 => {
                    s.execute("edit.undo", &json!({})).unwrap();
                }
                2 => {
                    s.selection = Selection::default();
                }
                3 => {
                    let mut changed = projected(&["a", "b"], None);
                    changed.snapshot.assets[0].revision = "replaced-original".into();
                    s.apply_photos_snapshot(changed).unwrap();
                }
                _ => {
                    s = s.with_apple_photos(None);
                    s.apply_photos_snapshot(projected(&["a", "b"], None)).unwrap();
                }
            }
            let revision = s.catalog.revision;
            assert!(job.accept(&mut s, seg()).unwrap_err().contains("discarded"), "change {change}");
            assert_eq!(s.catalog.revision, revision);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn vision_object_clicks_store_masks_and_reject_stale_results() {
        let selection = || Some(coverage_to_seg(2, &[1.0, 0.0, 1.0, 0.0]).unwrap());
        let setup = || {
            let mut s = photos_session();
            s.execute("mask.add", &json!({"kind":"object"})).unwrap();
            s
        };
        let mut s = setup();
        let id = s.active().unwrap();
        let original = s.catalog.photo(id).unwrap().source.clone();
        let job = VisionObjectClick::prepare(&mut s, &json!({"x":0.2, "y":0.5})).unwrap();
        assert!(matches!(job.source, SourceRef::ApplePhotos { network: false, .. }));
        let out = job.accept(&mut s, selection()).unwrap();
        assert_eq!(out, json!({"include":1, "exclude":0, "provider":"appleVision"}));
        let shape = &s.develop_of(id).unwrap().masks[0].components[0].shape;
        let stored: MaskShape = serde_json::from_value(serde_json::to_value(shape).unwrap()).unwrap();
        assert_eq!(&stored, shape);
        assert!(matches!(stored, MaskShape::Object { seg: Some(_), .. }));
        assert_eq!(s.catalog.photo(id).unwrap().source, original);
        assert!(VisionObjectClick::prepare(&mut s, &json!({"x":-0.1, "y":0.5})).is_err());
        assert!(VisionObjectClick::prepare(&mut s, &json!({"x":0.5, "y":0.5, "id":u64::MAX})).is_err());

        for change in 0..5 {
            let mut s = setup();
            let job = VisionObjectClick::prepare(&mut s, &json!({"x":0.2, "y":0.5})).unwrap();
            match change {
                0 => {
                    s.execute("mask.delete", &json!({})).unwrap();
                    s.execute("mask.add", &json!({"kind":"object"})).unwrap();
                }
                1 => {
                    s.execute("edit.undo", &json!({})).unwrap();
                }
                2 => {
                    s.selection = Selection::default();
                }
                3 => {
                    let mut changed = projected(&["a", "b"], None);
                    changed.snapshot.assets[0].revision = "replaced-original".into();
                    s.apply_photos_snapshot(changed).unwrap();
                }
                _ => {
                    s = s.with_apple_photos(None);
                    s.apply_photos_snapshot(projected(&["a", "b"], None)).unwrap();
                }
            }
            let revision = s.catalog.revision;
            assert!(job.accept(&mut s, selection()).unwrap_err().contains("discarded"), "change {change}");
            assert_eq!(s.catalog.revision, revision, "stale result must not write an adjustment");
        }
    }

    #[test]
    fn preset_description_survives_photos_restart_and_reconstructs_from_the_source() {
        let dir = std::env::temp_dir().join(format!(
            "lightcraft-preset-description-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let mut s = Session::new().with_apple_photos(Some(dir.clone()));
        s.apply_photos_snapshot(projected(&["a"], Some(&dir))).unwrap();
        let id = s.apple_library.as_ref().unwrap().asset_ids["a"];
        s.selection = Selection::single(id);
        let source = s.catalog.photo(id).unwrap().source.clone();
        s.execute("develop.set", &json!({"control":"light.exposure", "value":0.4})).unwrap();
        let base = s.develop_of(id).unwrap();
        s.execute("preset.apply", &json!({"id":"lc.bw-high-contrast"})).unwrap();
        s.persist().unwrap();
        let path = std::fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
        let description: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(description["draft"]["develop"]["preset_selection"]["id"], "lc.bw-high-contrast");
        assert!(!description.to_string().contains("pixels"));
        let mut resumed = Session::new().with_apple_photos(Some(dir.clone()));
        resumed.apply_photos_snapshot(projected(&["a"], Some(&dir))).unwrap();
        resumed.selection = Selection::single(id);
        resumed.execute("preset.apply", &json!({"id":"lc.warm-glow"})).unwrap();
        let preset = resumed.presets.iter().find(|p| p.id == "lc.warm-glow").unwrap();
        assert_eq!(*resumed.develop_of(id).unwrap(), preset.select(&base, 1.0));
        assert_eq!(resumed.catalog.photo(id).unwrap().source, source);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "only one JSON description, no rewritten photo");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_sky_selection_uses_the_prepared_semantic_model() {
        // The weights are an optional build input, not a fixture or a Cargo-time download.
        let health = lightcraft_apple::invoke(&json!({"operation":"health"}), std::time::Duration::from_secs(15)).unwrap();
        if health["sky"] != true {
            return;
        }
        let (w, h) = (320, 200);
        let image = lightcraft_raster::Rgb32f {
            width: w,
            height: h,
            data: (0..w * h).map(|i| if i / w < 120 { [0.34, 0.48, 0.63] } else { [0.12, 0.20, 0.28] }).collect(),
        };
        let source = SourceRef::Loaded(Box::new(DecodedSource::new(Arc::new(image), None)));
        let seg = vision_sky_selection(source).unwrap();
        let values = seg.logits().unwrap();
        assert_eq!(values.len(), 512 * 512);
        assert!(values.iter().all(|v| v.is_finite()));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn canon_r8_vision_object_selection_and_exclusion_use_system_inference() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/raw/cr3-canon-r8-craw.cr3");
        let Ok(bytes) = std::fs::read(path) else { return };
        let (image, _) = crate::files::load_bytes(&bytes, 1000).unwrap();
        let side = 128;
        let foreground = lightcraft_apple::foreground_mask(image.width, image.height, &image.data, side).unwrap();
        let (at, value) = foreground.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
        assert!(*value > 0.9, "the CC0 fixture contains a foreground subject");
        let point = [(at % side) as f64 / side as f64, (at / side) as f64 / side as f64];
        let object = lightcraft_apple::foreground_instances(image.width, image.height, &image.data, side, &[point], &[]).unwrap();
        assert!(object[at] > 0.8, "click and coverage must have the same orientation");
        let excluded = lightcraft_apple::foreground_instances(image.width, image.height, &image.data, side, &[point], &[point]).unwrap();
        assert!(excluded.iter().all(|v| *v == 0.0), "removing the only selected instance must not select all instances");
        let background = foreground.iter().position(|v| *v < 0.01).unwrap();
        let background = [(background % side) as f64 / side as f64, (background / side) as f64 / side as f64];
        assert!(lightcraft_apple::foreground_instances(image.width, image.height, &image.data, side, &[background], &[]).is_err());
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
