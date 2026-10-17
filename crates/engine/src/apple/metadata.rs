//! Small, revision-bound capture metadata. No image pixels, library scan or iCloud retrieval.
use super::*;
use lightcraft_meta::Metadata;
use std::{fs::File, io::Read, sync::Mutex};

const LIMIT: u64 = 32_768;

#[derive(Clone, Serialize, Deserialize)]
pub struct Capture {
    pub metadata: Metadata,
    pub file_size: u64,
}

#[derive(Serialize, Deserialize)]
struct Record {
    source: Source,
    capture: Capture,
}

pub(super) struct Cache {
    directory: PathBuf,
    values: Mutex<BTreeMap<String, Capture>>,
}

fn read(path: &Path) -> Result<Record, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("Oversized Photos EXIF cache".into());
    }
    let record: Record = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    validate(&record.capture)?;
    Ok(record)
}

impl Cache {
    pub(super) fn scan(root: &Path) -> Self {
        let directory = root.join("Source Metadata");
        let mut values = BTreeMap::new();
        let mut remaining = 32_000_000_u64;
        if let Ok(files) = std::fs::read_dir(&directory) {
            for entry in files.take(100_000).flatten() {
                let Ok(size) = entry.metadata().map(|m| m.len()) else { continue };
                if size > LIMIT || size > remaining {
                    continue;
                }
                remaining -= size;
                if let Ok(record) = read(&entry.path())
                    && let Some(key) = super::originals::key(&record.source)
                    && entry.path().file_stem().is_some_and(|name| name == key.as_str())
                {
                    values.insert(key, record.capture);
                }
            }
        }
        Self { directory, values: Mutex::new(values) }
    }

    pub(super) fn get(&self, source: &Source) -> Option<Capture> {
        self.values.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&super::originals::key(source)?).cloned()
    }

    pub(super) fn store(&self, source: &Source, capture: &Capture) -> Result<(), String> {
        validate(capture)?;
        let key = super::originals::key(source).ok_or("EXIF cache requires a Photos source")?;
        let bytes = serde_json::to_vec(&Record { source: source.clone(), capture: capture.clone() }).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > LIMIT {
            return Err("Oversized Photos EXIF cache".into());
        }
        std::fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
        lightcraft_catalog::safe_file::write_atomic(&self.directory.join(format!("{key}.json")), &bytes).map_err(|e| e.to_string())?;
        self.values.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key, capture.clone());
        Ok(())
    }
}

fn validate(capture: &Capture) -> Result<(), String> {
    let m = &capture.metadata;
    if capture.file_size > 512_000_000
        || [&m.make, &m.model, &m.lens_make, &m.lens_model].iter().any(|s| s.as_ref().is_some_and(|s| s.len() > 1024))
        || [m.f_number, m.exposure_time, m.focal_length, m.focal_length_35mm]
            .into_iter()
            .flatten()
            .any(|n| !n.is_finite() || n <= 0.0 || n > 1_000_000.0)
        || m.gps.is_some_and(|g| {
            !g.latitude.is_finite()
                || !g.longitude.is_finite()
                || g.latitude.abs() > 90.0
                || g.longitude.abs() > 180.0
                || g.altitude.is_some_and(|a| !a.is_finite())
        })
    {
        return Err("Invalid Photos capture metadata".into());
    }
    Ok(())
}

pub struct MetadataJob {
    pub id: PhotoId,
    pub(super) generation: u64,
    pub source: Source,
    pub(super) cache: Option<Arc<OriginalCache>>,
}

impl MetadataJob {
    pub fn run(&self) -> Result<Capture, String> {
        if let Some(capture) = self.cache.as_ref().and_then(|c| c.capture.get(&self.source)) {
            return Ok(capture);
        }
        let Source::ApplePhotos { asset, .. } = &self.source else { return Err("Select a Photos image".into()) };
        #[cfg(target_os = "macos")]
        {
            let value = lightcraft_apple::photos_metadata(asset)?;
            let capture =
                from_fields(value.get("capture").ok_or("Missing Photos EXIF response")?, value.get("fileSize").and_then(Value::as_u64).unwrap_or(0))?;
            if let Some(cache) = &self.cache {
                cache.capture.store(&self.source, &capture)?;
            }
            Ok(capture)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = asset;
            Err("Apple Photos metadata is available only on macOS".into())
        }
    }
}

#[cfg(target_os = "macos")]
pub(super) fn from_fields(value: &Value, file_size: u64) -> Result<Capture, String> {
    let mut defaults = serde_json::to_value(Metadata::default()).map_err(|e| e.to_string())?;
    defaults.as_object_mut().ok_or("Invalid EXIF defaults")?.extend(value.as_object().ok_or("Invalid Photos EXIF response")?.clone());
    let capture = Capture { metadata: serde_json::from_value(defaults).map_err(|e| e.to_string())?, file_size };
    validate(&capture)?;
    Ok(capture)
}

pub fn prepare_metadata(s: &Session, params: &Value) -> Result<MetadataJob, String> {
    primary(s)?;
    let id = match params.get("id") {
        Some(id) => PhotoId(id.as_u64().ok_or("Invalid photo identifier")?),
        None => s.active().ok_or("Select a photo first")?,
    };
    let photo = s.catalog.photo(id).ok_or("Photos image no longer available")?;
    if !matches!(photo.source, Source::ApplePhotos { .. }) {
        return Err("Select a Photos image".into());
    }
    Ok(MetadataJob { id, source: photo.source.clone(), generation: s.library_generation(), cache: s.media.photos_originals.clone() })
}

pub fn cached_metadata(s: &Session, id: PhotoId) -> Option<Capture> {
    s.media.photos_originals.as_ref()?.capture.get(&s.catalog.photo(id)?.source)
}

pub(super) fn apply_cached(photo: &mut Photo, cache: Option<&Arc<OriginalCache>>) {
    if let Some(capture) = cache.and_then(|c| c.capture.get(&photo.source)) {
        apply(photo, &capture);
    }
}

fn apply(photo: &mut Photo, capture: &Capture) {
    let m = &capture.metadata;
    let model = m.model.as_deref().unwrap_or_default();
    let make = m.make.as_deref().unwrap_or_default();
    if !model.is_empty() || !make.is_empty() {
        photo.meta.camera =
            if model.to_lowercase().starts_with(&make.to_lowercase()) { model.to_string() } else { format!("{make} {model}").trim().to_string() };
    }
    if let Some(v) = &m.lens_model {
        photo.meta.lens = v.clone();
    }
    if let Some(v) = m.focal_length {
        photo.meta.focal_mm = Some(v as f32);
    }
    if let Some(v) = m.f_number {
        photo.meta.aperture = Some(v as f32);
    }
    if let Some(v) = m.iso {
        photo.meta.iso = Some(v);
    }
    if let Some(v) = m.exposure_time {
        photo.meta.shutter = if v < 1.0 { format!("1/{:.0}", 1.0 / v) } else { v.to_string() };
    }
    if capture.file_size > 0 {
        photo.file_size = capture.file_size;
    }
}

pub(super) fn clear_capture(photo: &mut Photo) {
    photo.meta.camera.clear();
    photo.meta.lens.clear();
    photo.meta.shutter.clear();
    photo.meta.focal_mm = None;
    photo.meta.aperture = None;
    photo.meta.iso = None;
    photo.file_size = 0;
}

pub fn accept_metadata(s: &mut Session, job: &MetadataJob, capture: Capture) -> Result<Value, String> {
    if s.library_generation() != job.generation || s.catalog.photo(job.id).is_none_or(|p| p.source != job.source) {
        return Err("Photos image changed while reading EXIF".into());
    }
    validate(&capture)?;
    let mut photo = (**s.catalog.photo(job.id).ok_or("Photos image unavailable")?).clone();
    apply(&mut photo, &capture);
    let result = json!({"id":job.id.0,"camera":photo.meta.camera,"lens":photo.meta.lens,"iso":photo.meta.iso,"originalUnchanged":true});
    s.catalog
        .apply(Op::Batch {
            ops: vec![
                Op::SetMeta { id: job.id, meta: Box::new(photo.meta) },
                Op::SetContent {
                    id: job.id,
                    width: photo.width,
                    height: photo.height,
                    file_size: photo.file_size,
                    content_hash: photo.content_hash,
                    preview_only: photo.preview_only,
                },
            ],
        })
        .map_err(|e| e.to_string())?;
    Ok(result)
}

pub fn enrich_export(meta: &mut Metadata, capture: Capture, options: &crate::export::ExportOptions) {
    if options.metadata == crate::export::MetadataPolicy::All {
        let m = capture.metadata;
        macro_rules! known { ($($field:ident),*) => { $(if m.$field.is_some() { meta.$field=m.$field; })* }; }
        known!(make, model, lens_make, lens_model, exposure_time, f_number, iso, focal_length, focal_length_35mm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture() -> Capture {
        Capture {
            metadata: Metadata {
                make: Some("Synthetic".into()),
                model: Some("Synthetic X1".into()),
                lens_model: Some("Lens 35".into()),
                exposure_time: Some(0.004),
                f_number: Some(2.8),
                focal_length: Some(35.0),
                iso: Some(200),
                gps: Some(lightcraft_meta::Gps { latitude: 48.0, longitude: 2.0, altitude: None }),
                ..Default::default()
            },
            file_size: 12345,
        }
    }
    fn root() -> PathBuf {
        std::env::temp_dir().join(format!("lc-capture-{}-{}", std::process::id(), lightcraft_preview::next_tick()))
    }
    fn session(dir: &Path) -> Session {
        let mut s = Session::new().with_apple_photos(Some(dir.to_path_buf()));
        let snapshot:Snapshot=serde_json::from_value(json!({"authorization":"authorized","albums":[],"assets":[
            {"id":"synthetic-original","name":"test.CR3","width":8,"height":6,"raw":true,"favorite":false,"format":"CR3","revision":"r1","captured":"2024-01-01T12:00:00Z"}]})).unwrap();
        let mut loaded = LoadedLibrary::metadata(snapshot);
        loaded.originals = Some(OriginalCache::scan(dir));
        s.apply_photos_snapshot(loaded).unwrap();
        s.selection = Selection::single(s.apple_library.as_ref().unwrap().asset_ids["synthetic-original"]);
        s
    }
    #[test]
    fn capture_cache_survives_restart_and_cannot_cross_source_revisions() {
        let dir = root();
        let mut s = session(&dir);
        let id = s.active().unwrap();
        let source = s.catalog.photo(id).unwrap().source.clone();
        s.media.photos_originals.as_ref().unwrap().capture.store(&source, &capture()).unwrap();
        let reopened = session(&dir);
        let p = reopened.catalog.photo(id).unwrap();
        assert_eq!(p.meta.camera, "Synthetic X1");
        assert_eq!(p.meta.iso, Some(200));
        assert_eq!(p.file_size, 12345);
        assert!(OriginalCache::scan(&dir).capture.get(&Source::ApplePhotos { asset: "synthetic-original".into(), revision: "r2".into() }).is_none());
        let job = MetadataJob { id, generation: s.library_generation(), source: source.clone(), cache: s.media.photos_originals.clone() };
        s.execute("develop.set", &json!({"control":"light.exposure","value":0.5})).unwrap();
        s.execute("photo.setMeta", &json!({"title":"My title","gps":[12.0,15.0]})).unwrap();
        let before = s.catalog.photo(id).unwrap().clone();
        let history = before.history.clone();
        let pending = s.pending_log.len();
        accept_metadata(&mut s, &job, job.run().unwrap()).unwrap();
        let after = s.catalog.photo(id).unwrap();
        assert_eq!(after.develop, before.develop);
        assert_eq!(after.history, history);
        assert_eq!(s.pending_log.len(), pending);
        assert_eq!(after.meta.title, "My title");
        assert_eq!(after.meta.gps, Some((12.0, 15.0)));
        assert_eq!(after.source, source);
        s.execute("photo.setMeta", &json!({"gps":null})).unwrap();
        accept_metadata(&mut s, &job, job.run().unwrap()).unwrap();
        assert!(s.catalog.photo(id).unwrap().meta.gps.is_none(), "reading source EXIF preserves an explicitly cleared location");
        s.library_generation = s.library_generation.wrapping_add(1);
        assert!(accept_metadata(&mut s, &job, capture()).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn an_explicit_download_keeps_capture_metadata_with_its_lossless_pixels() {
        let dir = root();
        let mut s = session(&dir);
        let id = s.active().unwrap();
        let origin = s.catalog.photo(id).unwrap().source.clone();
        let mut full =
            crate::media::DecodedSource::new(Arc::new(lightcraft_raster::Rgb32f { width: 8, height: 6, data: vec![[0.1, 0.2, 0.3]; 48] }), None);
        full.capture = Some(Arc::new(capture()));
        let cache = s.media.photos_originals.clone().unwrap();
        cache.store(&origin, &full).unwrap();
        let job = DownloadJob {
            id,
            generation: s.library_generation(),
            origin: origin.clone(),
            source: SourceRef::Loaded(Box::new(full.clone())),
            cache: Some(cache),
        };
        accept_download(&mut s, &job, DownloadedOriginal::from_source(full.clone())).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().meta.iso, Some(200));
        let reopened = session(&dir);
        assert_eq!(reopened.catalog.photo(id).unwrap().meta.camera, "Synthetic X1");
        assert_eq!(OriginalCache::scan(&dir).get(&origin).unwrap().load(usize::MAX).unwrap().image, full.image);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn cached_exif_is_embedded_in_exports_and_privacy_policies_still_apply() {
        let dir = root();
        let mut s = session(&dir);
        let id = s.active().unwrap();
        let source = s.catalog.photo(id).unwrap().source.clone();
        s.media.photos_originals.as_ref().unwrap().capture.store(&source, &capture()).unwrap();
        s.media.insert(id, SourceLevel::Full, Arc::new(lightcraft_raster::Rgb32f { width: 8, height: 6, data: vec![[0.2, 0.3, 0.4]; 48] }));
        let exported = crate::export::export_photo(&mut s, id, &crate::export::ExportOptions::default(), 1).unwrap();
        let m = lightcraft_meta::extract(&exported.bytes);
        assert_eq!(m.make.as_deref(), Some("Synthetic"));
        assert_eq!(m.model.as_deref(), Some("Synthetic X1"));
        assert_eq!(m.iso, Some(200));
        assert_eq!(m.focal_length, Some(35.0));
        assert!((m.exposure_time.unwrap() - 0.004).abs() < 1e-6);
        let all = s.execute("photo.allMetadata", &json!({})).unwrap();
        assert!(all["exif"].as_array().unwrap().len() > 5);
        for policy in [crate::export::MetadataPolicy::None, crate::export::MetadataPolicy::Copyright, crate::export::MetadataPolicy::AllExceptCamera]
        {
            let options = crate::export::ExportOptions { metadata: policy, remove_location: true, ..Default::default() };
            let image = crate::export::export_photo(&mut s, id, &options, 1).unwrap();
            let m = lightcraft_meta::extract(&image.bytes);
            assert!(m.model.is_none() && m.iso.is_none() && m.gps.is_none());
        }
        let mut meta = Metadata::default();
        enrich_export(&mut meta, capture(), &crate::export::ExportOptions { remove_location: true, ..Default::default() });
        assert!(meta.gps.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn apple_imageio_reads_synthetic_exif_without_photos_access_or_pixel_decode() {
        let dir = root();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("synthetic.jpg");
        let image = lightcraft_raster::Rgba8 { width: 8, height: 6, data: vec![[12, 30, 60, 255]; 48] };
        let bytes = crate::export::encode_with_metadata(&image, &crate::export::ExportOptions::default(), Some(&capture().metadata)).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let value = lightcraft_apple::invoke(&json!({"operation":"image-metadata","path":path}), std::time::Duration::from_secs(15)).unwrap();
        assert_eq!(value["capture"]["iso"], 200);
        assert_eq!(value["capture"]["model"], "Synthetic X1");
        assert_eq!(value["capture"]["lens_model"], "Lens 35");
        assert_eq!(value["capture"]["exposure_time"], 0.004);
        assert_eq!(value["capture"]["gps"]["latitude"], 48.0);
        assert_eq!(value["capture"]["gps"]["longitude"], 2.0);
        let error = lightcraft_apple::invoke(
            &json!({"operation":"photos-metadata","asset":"never-access","network":true}),
            std::time::Duration::from_secs(10),
        )
        .unwrap_err();
        assert!(error.contains("never enable iCloud"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
