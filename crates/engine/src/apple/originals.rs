//! Lossless local backing for explicitly downloaded Photos originals. Never calls PhotoKit.
use crate::media::DecodedSource;
use lightcraft_catalog::Source;
use lightcraft_preview::Hasher128;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

const MAGIC: &[u8; 8] = b"LCPRAW01";
const HEADER_LIMIT: usize = 16_384;
const PIXEL_LIMIT: usize = 100_000_000;
const DISK_BYTES: u64 = 8 << 30;

#[derive(Serialize, Deserialize)]
struct Header {
    source: Source,
    width: usize,
    height: usize,
    raw: bool,
    temperature: f64,
    tint: f64,
    relative_wb: bool,
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

fn read_header(file: &mut File) -> Result<Header, String> {
    let mut magic = [0; 8];
    let mut size = [0; 4];
    file.read_exact(&mut magic).and_then(|_| file.read_exact(&mut size)).map_err(|e| e.to_string())?;
    let size = u32::from_le_bytes(size) as usize;
    if &magic != MAGIC || size > HEADER_LIMIT {
        return Err("Invalid cached Photos original header".into());
    }
    let mut bytes = vec![0; size];
    file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    let h: Header = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let count =
        h.width.checked_mul(h.height).filter(|&n| h.width > 0 && h.height > 0 && n <= PIXEL_LIMIT).ok_or("Invalid cached Photos dimensions")?;
    if !h.temperature.is_finite() || !h.tint.is_finite() || file.metadata().map_err(|e| e.to_string())?.len() != 12 + size as u64 + count as u64 * 12
    {
        return Err("Truncated cached Photos original".into());
    }
    Ok(h)
}

impl OriginalCache {
    /// Called by the library's background loader. The index makes UI availability checks I/O free.
    pub fn scan(adjustments: &Path) -> Arc<Self> {
        let directory = adjustments.join("Original Cache");
        let mut entries = BTreeMap::new();
        if let Ok(files) = std::fs::read_dir(&directory) {
            for entry in files.take(100_000).flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "lcraw") {
                    continue;
                }
                if let Ok(h) = File::open(&path).map_err(|e| e.to_string()).and_then(|mut f| read_header(&mut f))
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

    /// Runs only on the explicit download worker, before its successful completion is announced.
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
        // These are exactly the facts returned by the Apple decoder. Reject future richer inputs
        // rather than silently losing camera colour, semantic mattes or a denoised twin.
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
        let header = serde_json::to_vec(&Header {
            source: source.clone(),
            width: image.width,
            height: image.height,
            raw: info.raw,
            temperature: info.as_shot_temp,
            tint: info.as_shot_tint,
            relative_wb: info.relative_wb,
        })
        .map_err(|e| e.to_string())?;
        if header.len() > HEADER_LIMIT {
            return Err("Oversized Photos original metadata".into());
        }
        std::fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
        let path = self.directory.join(format!("{k}.lcraw"));
        lightcraft_catalog::safe_file::write_atomic_with(&path, &mut |file| {
            let mut writer = BufWriter::new(file);
            writer.write_all(MAGIC)?;
            writer.write_all(&(header.len() as u32).to_le_bytes())?;
            writer.write_all(&header)?;
            for pixel in &image.data {
                for channel in pixel {
                    writer.write_all(&channel.to_le_bytes())?;
                }
            }
            writer.flush()
        })
        .map_err(|e| format!("Could not cache this Photos original: {e}"))?;
        let mut entries = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.insert(k.clone(), CachedOriginal { path, source: source.clone() });
        let mut files: Vec<_> = entries
            .iter()
            .filter_map(|(key, entry)| {
                std::fs::metadata(&entry.path).ok().map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), key.clone(), m.len()))
            })
            .collect();
        files.sort();
        let mut bytes = files.iter().map(|e| e.2).sum::<u64>();
        for (_, old, size) in files {
            if bytes <= self.budget {
                break;
            }
            if old != k
                && let Some(entry) = entries.get(&old)
                && std::fs::remove_file(&entry.path).is_ok()
            {
                entries.remove(&old);
                bytes = bytes.saturating_sub(size);
            }
        }
        Ok(())
    }
}

impl CachedOriginal {
    pub fn load(&self, max_edge: usize) -> Result<DecodedSource, String> {
        let mut file = File::open(&self.path).map_err(|e| format!("Cached Photos original unavailable; use Download original to restore it: {e}"))?;
        let h = read_header(&mut file)?;
        if h.source != self.source {
            return Err("Cached Photos original belongs to another source revision".into());
        }
        let _ = file.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now()));
        let mut reader = BufReader::new(file);
        let mut data = Vec::with_capacity(h.width * h.height);
        for _ in 0..h.width * h.height {
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
            budget: 1000,
        };
        cache.store(&source("r1"), &decoded()).unwrap();
        cache.store(&source("r2"), &decoded()).unwrap();
        assert!(cache.get(&source("r1")).is_none());
        assert!(cache.get(&source("r2")).is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
