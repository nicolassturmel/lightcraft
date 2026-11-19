//! Local, original-safe backing for explicitly downloaded Photos images. Never calls PhotoKit.
use crate::media::DecodedSource;
use lightcraft_catalog::Source;
use lightcraft_preview::{Hash128, Hasher128, hash_bytes};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

mod packed;
const LEGACY_MAGIC: &[u8; 8] = b"LCPRAW01";
const MAGIC: &[u8; 8] = b"LCPRAW02";
const END: &[u8; 8] = b"LCPCEND2";
const HEADER_LIMIT: usize = 16_384;
const PIXEL_LIMIT: usize = 100_000_000;
const ENCODED_LIMIT: u64 = 512_000_000;
pub const DISK_BYTES: u64 = 32 << 30;

#[derive(Default, Serialize, Deserialize)]
#[serde(tag = "codec", rename_all = "camelCase")]
enum Storage {
    #[default]
    Float32,
    Deflate {
        pixels: String,
    },
    Encoded {
        suffix: String,
        bytes: String,
        pixels: String,
    },
}
#[derive(Serialize, Deserialize)]
struct Header {
    source: Source,
    width: usize,
    height: usize,
    raw: bool,
    temperature: f64,
    tint: f64,
    relative_wb: bool,
    #[serde(default)]
    storage: Storage,
}
#[derive(Clone)]
pub struct CachedOriginal {
    path: PathBuf,
    source: Source,
}
pub struct OriginalCache {
    pub(super) capture: super::metadata::Cache,
    directory: PathBuf,
    entries: Mutex<BTreeMap<String, CachedOriginal>>,
    budget: u64,
}
pub(super) fn key(source: &Source) -> Option<String> {
    let Source::ApplePhotos { asset, revision } = source else { return None };
    Some(Hasher128::new().str("Photos original pixels v1").str(asset).str(revision).finish().to_string())
}
fn pixel_hash(pixels: &[[f32; 3]]) -> String {
    let mut hash = Hasher128::new();
    let mut buffer = Vec::with_capacity(4096 * 12);
    for chunk in pixels.chunks(4096) {
        buffer.clear();
        for pixel in chunk {
            for v in pixel {
                buffer.extend_from_slice(&v.to_le_bytes());
            }
        }
        hash.update(&buffer);
    }
    hash.finish().to_string()
}
/// Check the small header/footer only; initial library projection never decodes pixels.
fn read_header(file: &mut File) -> Result<(Header, Option<u64>), String> {
    let mut magic = [0; 8];
    let mut size = [0; 4];
    file.read_exact(&mut magic).and_then(|_| file.read_exact(&mut size)).map_err(|e| e.to_string())?;
    let size = u32::from_le_bytes(size) as usize;
    let legacy = &magic == LEGACY_MAGIC;
    if (!legacy && &magic != MAGIC) || size > HEADER_LIMIT {
        return Err("Invalid cached Photos original header".into());
    }
    let mut bytes = vec![0; size];
    file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    if !legacy {
        let mut digest = [0; 16];
        file.read_exact(&mut digest).map_err(|e| e.to_string())?;
        if u128::from_le_bytes(digest) != hash_bytes(&bytes).0 {
            return Err("Corrupt cached Photos header".into());
        }
    }
    let h: Header = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let count =
        h.width.checked_mul(h.height).filter(|&n| h.width > 0 && h.height > 0 && n <= PIXEL_LIMIT).ok_or("Invalid cached Photos dimensions")?;
    if !h.temperature.is_finite() || !h.tint.is_finite() {
        return Err("Invalid cached Photos decoder facts".into());
    }
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    if legacy {
        if !matches!(h.storage, Storage::Float32) || length != 12 + size as u64 + count as u64 * 12 {
            return Err("Truncated cached Photos original".into());
        }
        return Ok((h, None));
    }
    let start = file.stream_position().map_err(|e| e.to_string())?;
    if length < start + 16 {
        return Err("Truncated cached Photos original".into());
    }
    file.seek(SeekFrom::End(-16)).map_err(|e| e.to_string())?;
    let mut end = [0; 8];
    let mut payload = [0; 8];
    file.read_exact(&mut end).and_then(|_| file.read_exact(&mut payload)).map_err(|e| e.to_string())?;
    let payload = u64::from_le_bytes(payload);
    let blocks = count.div_ceil(packed::PIXELS) as u64;
    let valid = match &h.storage {
        Storage::Float32 => false,
        Storage::Deflate { pixels } => {
            Hash128::parse(pixels).is_some() && payload >= blocks * 10 && payload <= count as u64 * 12 + blocks * (packed::OVERHEAD as u64 + 4)
        }
        Storage::Encoded { suffix, bytes, pixels } => {
            !suffix.is_empty()
                && suffix.len() <= 16
                && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
                && Hash128::parse(bytes).is_some()
                && Hash128::parse(pixels).is_some()
                && payload > 0
                && payload <= ENCODED_LIMIT
        }
    };
    if &end != END || !valid || length.checked_sub(start + 16) != Some(payload) {
        return Err("Invalid or truncated compressed Photos cache".into());
    }
    file.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    Ok((h, Some(payload)))
}
impl OriginalCache {
    pub fn scan(adjustments: &Path) -> Arc<Self> {
        let directory = adjustments.join("Original Cache");
        let mut entries = BTreeMap::new();
        if let Ok(files) = std::fs::read_dir(&directory) {
            for entry in files.take(100_000).flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "lcraw") {
                    continue;
                }
                if let Ok((h, _)) = File::open(&path).map_err(|e| e.to_string()).and_then(|mut f| read_header(&mut f))
                    && let Some(k) = key(&h.source)
                    && path.file_stem().is_some_and(|name| name == k.as_str())
                {
                    entries.insert(k, CachedOriginal { path, source: h.source });
                }
            }
        }
        Arc::new(Self { capture: super::metadata::Cache::scan(adjustments), directory, entries: Mutex::new(entries), budget: DISK_BYTES })
    }
    pub fn get(&self, source: &Source) -> Option<CachedOriginal> {
        self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&key(source)?).cloned()
    }
    pub fn store(&self, source: &Source, decoded: &DecodedSource) -> Result<(), String> {
        if let Some(capture) = &decoded.capture {
            self.capture.store(source, capture)?;
        }
        let k = key(source).ok_or("Only Photos originals belong in this cache")?;
        let image = &decoded.image;
        let info = decoded.info.clone().unwrap_or_default();
        if image.width.checked_mul(image.height) != Some(image.data.len())
            || image.data.is_empty()
            || image.data.len() > PIXEL_LIMIT
            || !info.as_shot_temp.is_finite()
            || !info.as_shot_tint.is_finite()
            || image.data.iter().flatten().any(|p| !p.is_finite())
        {
            return Err("Invalid Photos original pixels".into());
        }
        if info.lens.is_some()
            || info.camera_color.is_some()
            || info.camera_tone.is_some()
            || info.mattes.is_some()
            || info.local_tone.is_some()
            || decoded.camera_tone.is_some()
            || decoded.denoised.is_some()
        {
            return Err("Unsupported Photos original cache metadata".into());
        }
        let digest = pixel_hash(&image.data);
        let encoded = decoded.original.as_ref().filter(|o| o.bytes.len() < image.data.len() * 12);
        let storage = if let Some(o) = encoded {
            if o.bytes.is_empty()
                || o.bytes.len() as u64 > ENCODED_LIMIT
                || o.suffix.is_empty()
                || o.suffix.len() > 16
                || !o.suffix.bytes().all(|b| b.is_ascii_alphanumeric())
            {
                return Err("Invalid cached Photos resource".into());
            }
            Storage::Encoded { suffix: o.suffix.clone(), bytes: hash_bytes(&o.bytes).to_string(), pixels: digest }
        } else {
            Storage::Deflate { pixels: digest }
        };
        let header = serde_json::to_vec(&Header {
            source: source.clone(),
            width: image.width,
            height: image.height,
            raw: info.raw,
            temperature: info.as_shot_temp,
            tint: info.as_shot_tint,
            relative_wb: info.relative_wb,
            storage,
        })
        .map_err(|e| e.to_string())?;
        if header.len() > HEADER_LIMIT {
            return Err("Oversized Photos original metadata".into());
        }
        std::fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        }
        let path = self.directory.join(format!("{k}.lcraw"));
        lightcraft_catalog::safe_file::write_atomic_with(&path, &mut |file| {
            let mut writer = BufWriter::new(file);
            writer.write_all(MAGIC)?;
            writer.write_all(&(header.len() as u32).to_le_bytes())?;
            writer.write_all(&header)?;
            writer.write_all(&hash_bytes(&header).0.to_le_bytes())?;
            let length = if let Some(o) = encoded {
                writer.write_all(&o.bytes)?;
                o.bytes.len() as u64
            } else {
                packed::write(&mut writer, &image.data)?
            };
            writer.write_all(END)?;
            writer.write_all(&length.to_le_bytes())?;
            writer.flush()
        })
        .map_err(|e| format!("Could not cache this Photos original: {e}"))?;
        let mut entries = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.insert(k.clone(), CachedOriginal { path, source: source.clone() });
        let mut files: Vec<_> = entries
            .iter()
            .filter_map(|(key, e)| std::fs::metadata(&e.path).ok().map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), key.clone(), m.len())))
            .collect();
        files.sort();
        let mut bytes = files.iter().map(|e| e.2).sum::<u64>();
        for (_, old, size) in files {
            if bytes <= self.budget {
                break;
            }
            if old != k
                && let Some(e) = entries.get(&old)
                && std::fs::remove_file(&e.path).is_ok()
            {
                entries.remove(&old);
                bytes = bytes.saturating_sub(size);
            }
        }
        Ok(())
    }
    /// Offline maintenance only: convert old float files locally, without retrieving resources.
    /// Call while the desktop app is stopped; no descriptions, recipes or Photos assets change.
    pub fn compact_legacy(&self) -> Result<(usize, u64, u64), String> {
        let entries: Vec<_> = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).values().cloned().collect();
        let (mut count, mut before, mut after) = (0, 0, 0);
        for entry in entries {
            let mut file = File::open(&entry.path).map_err(|e| e.to_string())?;
            let (_, payload) = read_header(&mut file)?;
            if payload.is_some() {
                continue;
            }
            let size = file.metadata().map_err(|e| e.to_string())?.len();
            drop(file);
            let decoded = entry.load(usize::MAX)?;
            self.store(&entry.source, &decoded)?;
            let stored = entry.load(usize::MAX)?;
            if pixel_hash(&stored.image.data) != pixel_hash(&decoded.image.data) {
                return Err("Photos cache migration changed pixels".into());
            }
            count += 1;
            before += size;
            after += std::fs::metadata(&entry.path).map_err(|e| e.to_string())?.len();
        }
        Ok((count, before, after))
    }
}
impl CachedOriginal {
    pub fn load(&self, max_edge: usize) -> Result<DecodedSource, String> {
        let mut file = File::open(&self.path).map_err(|e| format!("Cached Photos original unavailable; use Download original to restore it: {e}"))?;
        let (h, payload) = read_header(&mut file)?;
        if h.source != self.source {
            return Err("Cached Photos original belongs to another source revision".into());
        }
        let _ = file.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now()));
        let count = h.width * h.height;
        let mut reader = BufReader::new(file);
        let data = match &h.storage {
            Storage::Float32 => {
                let mut data = Vec::new();
                data.try_reserve_exact(count).map_err(|_| "Not enough memory for cached Photos pixels")?;
                for _ in 0..count {
                    let mut bytes = [0; 12];
                    reader.read_exact(&mut bytes).map_err(|e| e.to_string())?;
                    let mut pixel = [0.0; 3];
                    for (value, bytes) in pixel.iter_mut().zip(bytes.as_chunks::<4>().0) {
                        *value = f32::from_le_bytes(*bytes);
                        if !value.is_finite() {
                            return Err("Invalid cached Photos pixel".into());
                        }
                    }
                    data.push(pixel);
                }
                data
            }
            Storage::Deflate { pixels } => {
                let mut payload = reader.take(payload.ok_or("Missing Photos cache payload")?);
                let data = packed::read(&mut payload, count)?;
                if payload.limit() != 0 || pixel_hash(&data) != *pixels {
                    return Err("Corrupt compressed Photos pixels".into());
                }
                data
            }
            Storage::Encoded { suffix, bytes, pixels } => {
                let length = payload.ok_or("Missing Photos resource payload")?;
                let mut encoded = Vec::new();
                reader.take(length).read_to_end(&mut encoded).map_err(|e| e.to_string())?;
                if encoded.len() as u64 != length || hash_bytes(&encoded).to_string() != *bytes {
                    return Err("Corrupt cached Photos resource".into());
                }
                let decoded = crate::apple::decode_cached_original(&encoded, suffix, h.raw)?;
                if decoded.image.width != h.width || decoded.image.height != h.height || pixel_hash(&decoded.image.data) != *pixels {
                    return Err("Apple decoder output changed; use Download original to rebuild this photo's cache".into());
                }
                Arc::unwrap_or_clone(decoded.image).data
            }
        };
        let image = lightcraft_raster::Rgb32f { width: h.width, height: h.height, data };
        let image = if image.width.max(image.height) > max_edge {
            lightcraft_raster::resample::fit(&image, max_edge.max(1), max_edge.max(1), lightcraft_raster::resample::Filter::Box)
        } else {
            image
        };
        Ok(DecodedSource::new(
            Arc::new(image),
            Some(lightcraft_pipeline::SourceInfo {
                raw: h.raw,
                as_shot_temp: h.temperature,
                as_shot_tint: h.tint,
                relative_wb: h.relative_wb,
                ..Default::default()
            }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    fn folder() -> PathBuf {
        let path = std::env::temp_dir().join(format!("lc-photos-original-cache-{}-{}", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    fn source(revision: &str) -> Source {
        Source::ApplePhotos { asset: "fixture".into(), revision: revision.into() }
    }
    fn decoded() -> DecodedSource {
        DecodedSource::new(
            Arc::new(lightcraft_raster::Rgb32f { width: 8, height: 6, data: vec![[1.75, -0.125, 0.234_567_89]; 48] }),
            Some(lightcraft_pipeline::SourceInfo { raw: true, as_shot_temp: 5300.0, as_shot_tint: -3.0, ..Default::default() }),
        )
    }
    fn legacy(dir: &Path, decoded: &DecodedSource) -> PathBuf {
        let info = decoded.info.as_ref().unwrap();
        let header = serde_json::to_vec(&serde_json::json!({
            "source":source("r1"),"width":decoded.image.width,"height":decoded.image.height,
            "raw":info.raw,"temperature":info.as_shot_temp,"tint":info.as_shot_tint,"relative_wb":info.relative_wb
        }))
        .unwrap();
        let dir = dir.join("Original Cache");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.lcraw", key(&source("r1")).unwrap()));
        let mut file = BufWriter::new(File::create(&path).unwrap());
        file.write_all(LEGACY_MAGIC).unwrap();
        file.write_all(&(header.len() as u32).to_le_bytes()).unwrap();
        file.write_all(&header).unwrap();
        for value in decoded.image.data.iter().flatten() {
            file.write_all(&value.to_le_bytes()).unwrap();
        }
        file.flush().unwrap();
        path
    }
    #[test]
    fn legacy_cache_compacts_locally_with_identical_bits_and_without_changing_edits() {
        let dir = folder();
        let description = dir.join("adjustment.json");
        std::fs::write(&description, b"permanent user edits").unwrap();
        let original = decoded();
        let path = legacy(&dir, &original);
        let before = std::fs::metadata(&path).unwrap().len();
        let cache = OriginalCache::scan(&dir);
        assert_eq!(pixel_hash(&cache.get(&source("r1")).unwrap().load(usize::MAX).unwrap().image.data), pixel_hash(&original.image.data));
        let (count, old, new) = cache.compact_legacy().unwrap();
        assert_eq!((count, old), (1, before));
        assert!(new < old);
        assert_eq!(cache.compact_legacy().unwrap(), (0, 0, 0), "already compacted files must not be rewritten");
        assert_eq!(std::fs::read(&description).unwrap(), b"permanent user edits");
        let reopened = OriginalCache::scan(&dir).get(&source("r1")).unwrap().load(usize::MAX).unwrap();
        assert_eq!(pixel_hash(&reopened.image.data), pixel_hash(&original.image.data));
        assert_eq!(reopened.info, original.info);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn compressed_header_payload_footer_and_encoded_bytes_are_verified() {
        let dir = folder();
        let cache = OriginalCache::scan(&dir);
        cache.store(&source("r1"), &decoded()).unwrap();
        let entry = cache.get(&source("r1")).unwrap();
        let good = std::fs::read(&entry.path).unwrap();
        let mut file = File::open(&entry.path).unwrap();
        read_header(&mut file).unwrap();
        let start = file.stream_position().unwrap() as usize;
        for at in [12, start - 1, start + 4, good.len() - 16, good.len() - 1] {
            let mut bad = good.clone();
            bad[at] ^= 0x55;
            std::fs::write(&entry.path, &bad).unwrap();
            assert!(entry.load(usize::MAX).is_err(), "corruption at {at} must be reported");
        }
        let mut encoded = decoded();
        encoded.original = Some(Arc::new(crate::media::EncodedOriginal { bytes: vec![1, 2, 3, 4], suffix: "cr3".into() }));
        cache.store(&source("r1"), &encoded).unwrap();
        let mut file = File::open(&entry.path).unwrap();
        assert!(matches!(read_header(&mut file).unwrap().0.storage, Storage::Encoded { .. }));
        let start = file.stream_position().unwrap() as usize;
        let mut bad = std::fs::read(&entry.path).unwrap();
        bad[start] ^= 1;
        std::fs::write(&entry.path, &bad).unwrap();
        assert!(
            entry.load(usize::MAX).err().unwrap().contains("Corrupt cached Photos resource"),
            "resource corruption is rejected before any decoder call"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn encoded_resource_reopens_through_apple_without_photos_and_repeated_download_keeps_it() {
        let dir = folder();
        let image = lightcraft_raster::Rgba8 { width: 128, height: 96, data: vec![[130, 75, 210, 255]; 128 * 96] };
        let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        let mut original = crate::apple::decode_cached_original(&bytes, "png", false).unwrap();
        original.original = Some(Arc::new(crate::media::EncodedOriginal { bytes: bytes.clone(), suffix: "png".into() }));
        let cache = OriginalCache::scan(&dir);
        cache.store(&source("r1"), &original).unwrap();
        let entry = cache.get(&source("r1")).unwrap();
        let stored = std::fs::read(&entry.path).unwrap();
        assert!(stored.len() < bytes.len() + 1024);
        let loaded = entry.load(usize::MAX).unwrap();
        assert_eq!(pixel_hash(&loaded.image.data), pixel_hash(&original.image.data));
        let job = super::super::DownloadJob {
            id: lightcraft_catalog::PhotoId(1),
            generation: 0,
            origin: source("r1"),
            source: crate::media::SourceRef::ApplePhotos { asset: "must-not-be-read".into(), max_edge: 128, network: false },
            cache: Some(cache),
        };
        // A valid cache must make an explicit retry a local operation, retaining the encoded file.
        let downloaded = job.run().unwrap();
        assert!(downloaded.full.original.is_none());
        assert_eq!(std::fs::read(&entry.path).unwrap(), stored);
        assert_eq!(pixel_hash(&downloaded.full.image.data), pixel_hash(&original.image.data));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn downloaded_pixels_and_decoder_facts_survive_reopening_without_photokit() {
        let dir = folder();
        let cache = OriginalCache::scan(&dir);
        let original = decoded();
        cache.store(&source("r1"), &original).unwrap();
        assert!(cache.get(&source("r2")).is_none(), "source changes must never reuse an older original");
        drop(cache);
        let cache = OriginalCache::scan(&dir);
        let entry = cache.get(&source("r1")).unwrap();
        let loaded = entry.load(usize::MAX).unwrap();
        assert_eq!(loaded.image, original.image, "f32 highlights and negative samples must remain exact");
        assert_eq!(loaded.info, original.info);
        assert_eq!((entry.load(4).unwrap().image.width, entry.load(4).unwrap().image.height), (4, 3));
        let mut file = File::options().write(true).open(&entry.path).unwrap();
        file.set_len(13).unwrap();
        file.flush().unwrap();
        assert!(entry.load(usize::MAX).is_err());
        assert!(OriginalCache::scan(&dir).get(&source("r1")).is_none(), "an incomplete file must not announce availability");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn failed_cache_write_keeps_the_previous_complete_original() {
        let dir = folder();
        let cache = OriginalCache::scan(&dir);
        cache.store(&source("r1"), &decoded()).unwrap();
        {
            let _fault = lightcraft_catalog::safe_file::fail_writes_after(20);
            assert!(cache.store(&source("r1"), &decoded()).is_err());
        }
        assert_eq!(cache.get(&source("r1")).unwrap().load(usize::MAX).unwrap().image, decoded().image);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn disk_budget_evicts_older_cached_originals_without_removing_the_new_one() {
        let dir = folder();
        let cache = OriginalCache {
            capture: super::super::metadata::Cache::scan(&dir),
            directory: dir.join("Original Cache"),
            entries: Mutex::default(),
            budget: 1,
        };
        cache.store(&source("r1"), &decoded()).unwrap();
        cache.store(&source("r2"), &decoded()).unwrap();
        assert!(cache.get(&source("r1")).is_none());
        assert!(cache.get(&source("r2")).is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
