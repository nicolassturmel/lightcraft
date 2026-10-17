//! PhotoKit events update only affected records. The original and adjustment stores stay separate.
use super::*;

#[derive(Default, Deserialize, Serialize)]
pub struct PhotosChanges {
    pub assets: Vec<Asset>,
    pub removed: Vec<String>,
    /// Present only when the album structure, names or membership actually changed.
    pub albums: Option<Vec<PhotosAlbum>>,
}

pub enum PhotosUpdate {
    Snapshot(LoadedLibrary),
    Changes(PhotosChanges),
}

pub struct PhotosWatch {
    #[cfg(target_os = "macos")]
    receiver: Option<std::sync::mpsc::Receiver<Result<PhotosUpdate, String>>>,
    #[cfg(target_os = "macos")]
    native: Option<lightcraft_apple::PhotosWatch>,
}

impl PhotosWatch {
    /// The initial snapshot and subsequent incremental events share one persistent subscription.
    pub fn start(directory: Option<PathBuf>, wake: impl Fn() + Send + 'static) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        {
            let (tx, receiver) = std::sync::mpsc::sync_channel(1);
            let native = lightcraft_apple::PhotosWatch::start(move |event| {
                let result = event.and_then(|event| match event {
                    lightcraft_apple::PhotosEvent::Snapshot(value) => {
                        let snapshot = serde_json::from_value(value).map_err(|e| e.to_string())?;
                        let (drafts, warnings) = load_drafts(directory.as_deref());
                        let originals = directory.as_deref().map(super::OriginalCache::scan);
                        Ok(PhotosUpdate::Snapshot(LoadedLibrary { snapshot, drafts, warnings, originals }))
                    }
                    lightcraft_apple::PhotosEvent::Changes(value) => {
                        serde_json::from_value(value).map(PhotosUpdate::Changes).map_err(|e| e.to_string())
                    }
                });
                let keep = tx.send(result).is_ok();
                wake();
                keep
            })?;
            Ok(Self { receiver: Some(receiver), native: Some(native) })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (directory, wake);
            Err("Apple Photos is available only on macOS".into())
        }
    }

    pub fn poll(&self) -> Option<Result<PhotosUpdate, String>> {
        #[cfg(target_os = "macos")]
        return self.receiver.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(event) => Some(event),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err("Photos observer disconnected; use Refresh Photos".into())),
        });
        #[cfg(not(target_os = "macos"))]
        None
    }
}

impl Drop for PhotosWatch {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        {
            // Unblock any bounded send before joining the native reader.
            self.receiver.take();
            self.native.take();
        }
    }
}

impl Session {
    /// Validate on a cheap Arc-sharing clone, then atomically commit the affected metadata only.
    pub fn apply_photos_changes(&mut self, changes: PhotosChanges) -> Result<Value, String> {
        if self.interaction.is_some() {
            return Err("Finish the current adjustment before updating Photos".into());
        }
        if changes.assets.len() > 500_000 || changes.removed.len() > 500_000 || changes.albums.as_ref().is_some_and(|a| a.len() > 20_000) {
            return Err("Photos change exceeds the session limit".into());
        }
        self.persist().map_err(|e| e.to_string())?;
        let state = self.apple_library.as_ref().filter(|l| l.loaded).ok_or("Photos changes arrived before the initial library")?;
        let mut ids = state.asset_ids.clone();
        let mut album_ids = state.album_ids.clone();
        let mut next = self.catalog.clone();
        let mut seen = BTreeSet::new();
        let mut removed = BTreeSet::new();
        let mut invalidated = BTreeSet::new();
        for asset in changes.removed {
            if asset.is_empty() || asset.len() > 1024 || !seen.insert(asset.clone()) {
                return Err("Invalid or duplicate Photos removal; current library kept".into());
            }
            if let Some(id) = ids.get(&asset).copied().filter(|id| next.photo(*id).is_some()) {
                next.apply(Op::RemovePhoto { id }).map_err(|e| e.to_string())?;
                removed.insert(id);
            }
        }
        let mut next_id = ids.values().map(|id| id.0).max().unwrap_or(0).checked_add(1).ok_or("Photos identity overflow")?;
        for asset in changes.assets {
            if asset.id.is_empty()
                || asset.id.len() > 1024
                || !seen.insert(asset.id.clone())
                || asset.width == 0
                || asset.height == 0
                || asset.gps.is_some_and(|(lat, lon)| !lat.is_finite() || !lon.is_finite() || lat.abs() > 90.0 || lon.abs() > 180.0)
                || u64::from(asset.width) * u64::from(asset.height) > 100_000_000
            {
                return Err("Invalid or duplicate Photos change; current library kept".into());
            }
            let id = match ids.get(&asset.id).copied() {
                Some(id) => id,
                None => {
                    let id = PhotoId(next_id);
                    next_id = next_id.checked_add(1).ok_or("Photos identity overflow")?;
                    ids.insert(asset.id.clone(), id);
                    id
                }
            };
            let source = Source::ApplePhotos { asset: asset.id.clone(), revision: asset.revision };
            let before = next.photo(id).cloned();
            let mut photo = before.as_ref().map(|p| (**p).clone()).unwrap_or_else(|| {
                let mut p = Photo::new(id, source.clone(), &asset.name, &asset.format, asset.width, asset.height, &(self.clock)());
                if asset.favorite {
                    p.flag = Flag::Pick;
                }
                if let Some(draft) = state.drafts.get(&asset.id) {
                    draft.apply(&mut p);
                }
                p
            });
            if before.as_ref().is_none_or(|p| p.source != source) {
                super::metadata::clear_capture(&mut photo);
            }
            photo.source = source;
            photo.file_name = asset.name;
            photo.format = asset.format;
            photo.width = asset.width;
            photo.height = asset.height;
            photo.captured = asset.captured;
            if !state.drafts.contains_key(&asset.id) {
                photo.meta.gps = asset.gps;
            }
            photo.kind = if asset.raw { MediaKind::Raw } else { MediaKind::Image };
            super::metadata::apply_cached(&mut photo, self.media.photos_originals.as_ref());
            if let Some(before) = before {
                if *before == photo {
                    continue;
                }
                if before.source != photo.source {
                    invalidated.insert(id);
                }
                next.apply(Op::RemovePhoto { id }).map_err(|e| e.to_string())?;
            }
            next.apply(Op::AddPhoto { photo: Box::new(photo) }).map_err(|e| e.to_string())?;
        }
        if next.len() > 500_000 {
            return Err("Photos library exceeds the session limit".into());
        }
        if let Some(albums) = changes.albums {
            replace_albums(&mut next, &ids, &mut album_ids, albums)?;
        } else if !removed.is_empty() {
            let updates: Vec<_> = next
                .albums()
                .filter(|a| a.photos.iter().any(|id| removed.contains(id)))
                .map(|a| Op::SetAlbumPhotos { id: a.id, photos: a.photos.iter().copied().filter(|id| !removed.contains(id)).collect() })
                .collect();
            for op in updates {
                next.apply(op).map_err(|e| e.to_string())?;
            }
        }
        let changed = next.revision != self.catalog.revision;
        if !changed {
            return Ok(json!({"changed":false}));
        }
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
        let state = self.apple_library.as_mut().ok_or("Apple Photos closed during update")?;
        state.asset_ids = ids;
        state.album_ids = album_ids;
        state.snapshot_key = None;
        state.error = None;
        self.visible_key = None;
        self.total = None;
        Ok(json!({"changed":true, "photos":self.catalog.len(), "albums":self.catalog.albums().count()}))
    }
}

fn replace_albums(
    catalog: &mut Catalog,
    ids: &BTreeMap<String, PhotoId>,
    album_ids: &mut BTreeMap<String, AlbumId>,
    albums: Vec<PhotosAlbum>,
) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    let mut projected = Vec::new();
    let mut next_id = album_ids.values().map(|id| id.0).max().unwrap_or(0).checked_add(1).ok_or("Photos album identity overflow")?;
    for album in albums {
        if album.id.is_empty() || album.id.len() > 1024 || !seen.insert(album.id.clone()) {
            return Err("Invalid or duplicate Photos album change; current library kept".into());
        }
        let id = match album_ids.get(&album.id).copied() {
            Some(id) => id,
            None => {
                let id = AlbumId(next_id);
                next_id = next_id.checked_add(1).ok_or("Photos album identity overflow")?;
                album_ids.insert(album.id.clone(), id);
                id
            }
        };
        let mut a = Album::new(id, &album.name);
        a.folder = album.folder;
        a.parent = album.parent.as_ref().map(|p| album_ids.get(p).copied().ok_or("Unknown Photos parent folder")).transpose()?;
        a.photos = album.photos.iter().filter_map(|p| ids.get(p).copied()).filter(|id| catalog.photo(*id).is_some()).collect();
        projected.push(a);
    }
    if projected.len() == catalog.albums().count() && projected.iter().all(|a| catalog.album(a.id) == Some(a)) {
        return Ok(());
    }
    // Remove leaves before folders, then validate/add the parent-first PhotoKit tree on the clone.
    for _ in 0..33 {
        if catalog.albums().count() == 0 {
            break;
        }
        let parents: BTreeSet<_> = catalog.albums().filter_map(|a| a.parent).collect();
        let leaves: Vec<_> = catalog.albums().filter(|a| !parents.contains(&a.id)).map(|a| a.id).collect();
        for id in leaves {
            catalog.apply(Op::RemoveAlbum { id }).map_err(|e| e.to_string())?;
        }
    }
    if catalog.albums().count() > 0 {
        return Err("Photos album folders are too deeply nested".into());
    }
    for album in projected {
        catalog.apply(Op::AddAlbum { album }).map_err(|e| e.to_string())?;
    }
    Ok(())
}
