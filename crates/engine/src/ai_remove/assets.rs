//! Permanent repair assets, separate from original pixels and evictable previews.
use lightcraft_develop::{AiPatch, Spot, SpotMode};
use lightcraft_preview::{Hash128, Hasher128, next_tick};
use lightcraft_raster::Rgb32f;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
};

const MAGIC: &[u8; 8] = b"LCAIP001";
const MAX_FILE: usize = 17_000_000;
const MAX_PIXELS: usize = 1_048_576;

pub struct Pixels {
    pub description: AiPatch,
    pub data: Vec<[f32; 4]>,
}
struct Composite {
    key: Hash128,
    base: Weak<Rgb32f>,
    image: Arc<Rgb32f>,
    tick: u64,
}
#[derive(Default)]
struct Memory {
    patches: HashMap<String, (Arc<Pixels>, u64)>,
    composites: Vec<Composite>,
}
pub struct AiAssets {
    directory: Option<PathBuf>,
    memory: Mutex<Memory>,
}
impl Default for AiAssets {
    fn default() -> Self {
        Self::new(None)
    }
}
impl AiAssets {
    pub fn new(directory: Option<PathBuf>) -> Self {
        Self { directory, memory: Mutex::new(Memory::default()) }
    }
    pub fn clear_memory(&self) {
        let mut memory = self.memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        memory.composites.clear();
        if self.directory.is_some() {
            memory.patches.clear();
        }
    }
    pub fn usage(&self) -> crate::memory::Usage {
        let m = self.memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::memory::Usage::new(
            m.patches.len() + m.composites.len(),
            m.patches.values().map(|(p, _)| p.data.len() * 16).sum::<usize>() + m.composites.iter().map(|c| c.image.data.len() * 12).sum::<usize>(),
        )
    }
    pub fn oldest_tick(&self) -> Option<u64> {
        let m = self.memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        m.composites.iter().map(|c| c.tick).chain(m.patches.values().filter(|_| self.directory.is_some()).map(|(_, t)| *t)).min()
    }
    pub fn evict_oldest(&self) -> Option<usize> {
        let mut m = self.memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let comp = m.composites.iter().enumerate().min_by_key(|(_, c)| c.tick).map(|(i, c)| (i, c.tick));
        let patch = m.patches.iter().filter(|_| self.directory.is_some()).min_by_key(|(_, (_, t))| *t).map(|(k, (_, t))| (k.clone(), *t));
        if let Some((key, t)) = patch.as_ref()
            && comp.is_none_or(|(_, ct)| *t < ct)
        {
            return m.patches.remove(key).map(|(p, _)| p.data.len() * 16);
        }
        comp.map(|(i, _)| m.composites.remove(i).image.data.len() * 12)
    }
    /// Atomic durable write precedes the history change. A full disk leaves that change unapplied.
    pub fn store(&self, mut description: AiPatch, data: Vec<[f32; 4]>) -> Result<AiPatch, String> {
        description.id.clear();
        validate(&description, &data)?;
        description.id = identity(&description, &data)?;
        let pixels = Arc::new(Pixels { description: description.clone(), data });
        if let Some(dir) = &self.directory {
            let bytes = encode(&pixels)?;
            std::fs::create_dir_all(dir).map_err(|e| format!("Cannot save AI Remove: {e}"))?;
            let temporary = dir.join(format!(".repair-{}-{}.tmp", std::process::id(), next_tick()));
            let written = (|| {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
                f.write_all(&bytes)?;
                f.sync_all()?;
                std::fs::rename(&temporary, dir.join(format!("{}.lcaip", description.id)))
            })();
            if let Err(e) = written {
                let _ = std::fs::remove_file(&temporary);
                return Err(format!("Cannot save AI Remove: {e}"));
            }
        }
        self.memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner).patches.insert(description.id.clone(), (pixels, next_tick()));
        Ok(description)
    }
    fn read(&self, description: &AiPatch, m: &mut Memory) -> Result<Arc<Pixels>, String> {
        if !valid_id(&description.id) {
            return Err("Invalid AI Remove asset id".into());
        }
        if let Some((p, t)) = m.patches.get_mut(&description.id) {
            if p.description != *description {
                return Err("AI Remove description differs from its saved pixels".into());
            }
            *t = next_tick();
            return Ok(p.clone());
        }
        let directory = self.directory.as_ref().ok_or("AI Remove pixels are unavailable; regenerate the repair")?;
        let path = directory.join(format!("{}.lcaip", description.id));
        let bytes = read_bounded(&path)?;
        let pixels = Arc::new(decode(&bytes)?);
        if pixels.description != *description {
            return Err("AI Remove description differs from its saved pixels".into());
        }
        m.patches.insert(description.id.clone(), (pixels.clone(), next_tick()));
        Ok(pixels)
    }
    /// Exports and every preview use the same immutable patches, before geometric/colour edits.
    pub fn composite(&self, base: &Arc<Rgb32f>, spots: &[Spot], source: Option<Hash128>) -> Result<Arc<Rgb32f>, String> {
        let repairs: Vec<_> = spots.iter().filter(|s| s.mode == SpotMode::Ai && s.opacity > 0.0).collect();
        if repairs.is_empty() {
            return Ok(base.clone());
        }
        let source = source.ok_or("AI Remove requires a source identity")?.to_string();
        let mut h = Hasher128::new();
        for spot in &repairs {
            let d = spot.ai_patch.as_ref().ok_or("AI Remove pixels are missing; regenerate this repair")?;
            if d.source != source {
                return Err("This AI repair belongs to another original; paint a new repair on this photo".into());
            }
            if !spot.opacity.is_finite() {
                return Err("Invalid AI Remove opacity".into());
            }
            h.str(&d.id).u64(spot.opacity.to_bits());
        }
        let key = h.finish();
        let mut m = self.memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(c) = m.composites.iter_mut().find(|c| c.key == key && c.base.upgrade().is_some_and(|b| Arc::ptr_eq(&b, base))) {
            c.tick = next_tick();
            return Ok(c.image.clone());
        }
        if base.width == 0 || base.height == 0 || base.width.checked_mul(base.height) != Some(base.data.len()) {
            return Err("Invalid AI Remove source dimensions".into());
        }
        let mut image = (**base).clone();
        for spot in repairs {
            let d = spot.ai_patch.as_ref().ok_or("Missing AI Remove description")?;
            let patch = self.read(d, &mut m)?;
            blend(&mut image, &patch, spot.opacity.clamp(0.0, 100.0) as f32 / 100.0);
        }
        let image = Arc::new(image);
        // Bound composite count independently, including headless callers without a media-cache poll.
        while m.composites.len() >= 4 {
            if let Some(i) = m.composites.iter().enumerate().min_by_key(|(_, c)| c.tick).map(|(i, _)| i) {
                m.composites.remove(i);
            } else {
                break;
            }
        }
        m.composites.push(Composite { key, base: Arc::downgrade(base), image: image.clone(), tick: next_tick() });
        Ok(image)
    }
    pub fn export_asset(&self, description: &AiPatch) -> Result<Vec<u8>, String> {
        let mut m = self.memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        encode(self.read(description, &mut m)?.as_ref())
    }
    pub fn import_asset(&self, bytes: &[u8]) -> Result<AiPatch, String> {
        let p = decode(bytes)?;
        self.store(p.description, p.data)
    }
}
fn valid_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn validate(d: &AiPatch, data: &[[f32; 4]]) -> Result<(), String> {
    let b = d.bounds;
    if d.width == 0
        || d.height == 0
        || d.width.checked_mul(d.height) != Some(data.len())
        || data.len() > MAX_PIXELS
        || ![b.x0, b.y0, b.x1, b.y1, d.radius].iter().all(|v| v.is_finite())
        || b.x0 < 0.0
        || b.y0 < 0.0
        || b.x1 > 1.0
        || b.y1 > 1.0
        || b.width() <= 0.0
        || b.height() <= 0.0
        || !(0.0001..=0.5).contains(&d.radius)
        || !valid_id(&d.source)
        || d.model.len() > 128
        || d.stroke.is_empty()
        || d.stroke.len() > 4096
        || d.stroke.iter().any(|p| ![p.x, p.y].iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)))
        || data.iter().any(|p| p.iter().any(|v| !v.is_finite()) || !(0.0..=1.0).contains(&p[3]))
    {
        return Err("Invalid AI Remove asset".into());
    }
    Ok(())
}
fn identity(d: &AiPatch, data: &[[f32; 4]]) -> Result<String, String> {
    let mut d = d.clone();
    d.id.clear();
    let mut hash = Hasher128::new();
    hash.update(&serde_json::to_vec(&d).map_err(|e| e.to_string())?);
    for pixel in data {
        for value in pixel {
            hash.update(&value.to_le_bytes());
        }
    }
    Ok(hash.finish().to_string())
}
fn encode(p: &Pixels) -> Result<Vec<u8>, String> {
    let header = serde_json::to_vec(&p.description).map_err(|e| e.to_string())?;
    if header.len() > 196_000 {
        return Err("Oversized AI Remove description".into());
    }
    let mut bytes = Vec::from(MAGIC.as_slice());
    bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&header);
    let raw: Vec<u8> = p.data.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
    bytes.extend_from_slice(&miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6));
    if bytes.len() > MAX_FILE {
        return Err("Oversized AI Remove pixels".into());
    }
    Ok(bytes)
}
fn decode(bytes: &[u8]) -> Result<Pixels, String> {
    if bytes.len() > MAX_FILE || bytes.get(..8) != Some(MAGIC.as_slice()) {
        return Err("Invalid AI Remove file".into());
    }
    let len = u32::from_le_bytes(bytes.get(8..12).and_then(|s| s.try_into().ok()).ok_or("Truncated AI Remove header")?) as usize;
    if len > 196_000 {
        return Err("Oversized AI Remove header".into());
    }
    let end = 12usize.checked_add(len).ok_or("Invalid AI Remove header")?;
    let description: AiPatch = serde_json::from_slice(bytes.get(12..end).ok_or("Truncated AI Remove header")?).map_err(|e| e.to_string())?;
    let count = description.width.checked_mul(description.height).filter(|n| *n <= MAX_PIXELS).ok_or("Oversized AI Remove dimensions")?;
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(bytes.get(end..).ok_or("Missing AI Remove pixels")?, count.saturating_mul(16))
        .map_err(|_| "Damaged AI Remove pixels")?;
    if raw.len() != count * 16 {
        return Err("Truncated AI Remove pixels".into());
    }
    let data = raw
        .as_chunks::<16>()
        .0
        .iter()
        .map(|p| {
            let mut out = [0.0; 4];
            for (v, s) in out.iter_mut().zip(p.as_chunks::<4>().0.iter()) {
                *v = f32::from_le_bytes(*s);
            }
            out
        })
        .collect::<Vec<_>>();
    validate(&description, &data)?;
    if identity(&description, &data)? != description.id {
        return Err("AI Remove checksum mismatch".into());
    }
    Ok(Pixels { description, data })
}
fn read_bounded(path: &std::path::Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let f = std::fs::File::open(path).map_err(|e| format!("AI Remove pixels unavailable ({e}); restore the AI Remove folder or regenerate"))?;
    if f.metadata().map_err(|e| e.to_string())?.len() > MAX_FILE as u64 {
        return Err("Oversized AI Remove file".into());
    }
    let mut bytes = Vec::new();
    f.take(MAX_FILE as u64 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FILE {
        return Err("Oversized AI Remove file".into());
    }
    Ok(bytes)
}
fn blend(image: &mut Rgb32f, patch: &Pixels, opacity: f32) {
    let d = &patch.description;
    let b = d.bounds;
    let x0 = (b.x0 * image.width as f64).floor() as usize;
    let y0 = (b.y0 * image.height as f64).floor() as usize;
    let x1 = (b.x1 * image.width as f64).ceil() as usize;
    let y1 = (b.y1 * image.height as f64).ceil() as usize;
    for y in y0..y1.min(image.height) {
        for x in x0..x1.min(image.width) {
            let px = (((x as f64 + 0.5) / image.width as f64 - b.x0) / b.width() * d.width as f64 - 0.5).clamp(0.0, (d.width - 1) as f64);
            let py = (((y as f64 + 0.5) / image.height as f64 - b.y0) / b.height() * d.height as f64 - 0.5).clamp(0.0, (d.height - 1) as f64);
            let (ix, iy) = (px as usize, py as usize);
            let (fx, fy) = ((px - ix as f64) as f32, (py - iy as f64) as f32);
            // Premultiplied interpolation avoids dark fringes where the repair's alpha is zero.
            let mut sample = [0.0f32; 4];
            for (sx, sy, w) in [
                (ix, iy, (1.0 - fx) * (1.0 - fy)),
                ((ix + 1).min(d.width - 1), iy, fx * (1.0 - fy)),
                (ix, (iy + 1).min(d.height - 1), (1.0 - fx) * fy),
                ((ix + 1).min(d.width - 1), (iy + 1).min(d.height - 1), fx * fy),
            ] {
                if let Some(p) = patch.data.get(sy * d.width + sx) {
                    for c in 0..3 {
                        sample[c] += p[c] * p[3] * w;
                    }
                    sample[3] += p[3] * w;
                }
            }
            if sample[3] > 0.0
                && let Some(p) = image.data.get_mut(y * image.width + x)
            {
                for c in 0..3 {
                    p[c] = p[c] * (1.0 - sample[3] * opacity) + sample[c] * opacity;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_geom::{Point, Rect};
    fn description(source: Hash128) -> AiPatch {
        AiPatch {
            id: String::new(),
            model: "test-model".into(),
            source: source.to_string(),
            bounds: Rect::UNIT,
            width: 2,
            height: 2,
            stroke: vec![Point::new(0.25, 0.25)],
            radius: 0.02,
        }
    }
    #[test]
    fn patches_are_lossless_durable_source_bound_and_cached_separately_from_originals() {
        let dir = std::env::temp_dir().join(format!("lc-ai-assets-{}-{}", std::process::id(), next_tick()));
        let source = Hash128(5);
        let assets = AiAssets::new(Some(dir.clone()));
        let description = assets.store(description(source), vec![[0.8, 0.4, 0.2, 1.0], [0.0; 4], [0.0; 4], [0.0; 4]]).unwrap();
        let original = Arc::new(Rgb32f::filled(2, 2, [0.1; 3]));
        let spot = Spot { mode: SpotMode::Ai, ai_patch: Some(description.clone()), ..Default::default() };
        let result = assets.composite(&original, std::slice::from_ref(&spot), Some(source)).unwrap();
        assert_eq!(original.data, vec![[0.1; 3]; 4]);
        assert_eq!(result.data[0], [0.8, 0.4, 0.2]);
        assert_eq!(result.data[1], [0.1; 3]);
        assert!(Arc::ptr_eq(&result, &assets.composite(&original, std::slice::from_ref(&spot), Some(source)).unwrap()));
        let half = Spot { opacity: 50.0, ..spot.clone() };
        let mixed = assets.composite(&original, &[half], Some(source)).unwrap();
        assert!((mixed.data[0][0] - 0.45).abs() < 1e-6);
        assert!(assets.composite(&original, std::slice::from_ref(&spot), Some(Hash128(6))).is_err());
        let bytes = assets.export_asset(&description).unwrap();
        let imported = AiAssets::default();
        assert_eq!(imported.import_asset(&bytes).unwrap(), description);
        let reopened = AiAssets::new(Some(dir.clone()));
        assert_eq!(reopened.composite(&original, std::slice::from_ref(&spot), Some(source)).unwrap().data, result.data);
        reopened.clear_memory();
        std::fs::write(dir.join(format!("{}.lcaip", description.id)), b"damaged").unwrap();
        assert!(reopened.composite(&original, &[spot], Some(source)).unwrap_err().contains("Invalid AI Remove"));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn hostile_assets_and_missing_data_never_fall_back_to_heal() {
        for bytes in [vec![], b"LCAIP001".to_vec(), [MAGIC.as_slice(), &u32::MAX.to_le_bytes()].concat()] {
            assert!(decode(&bytes).is_err());
        }
        let assets = AiAssets::default();
        let base = Arc::new(Rgb32f::filled(4, 4, [0.2; 3]));
        let s = Spot { mode: SpotMode::Ai, ..Default::default() };
        assert!(assets.composite(&base, &[s], Some(Hash128(5))).is_err());
        let mut d = description(Hash128(5));
        d.width = usize::MAX;
        assert!(assets.store(d, vec![]).is_err());
        let mut d = description(Hash128(5));
        d.bounds.x0 = f64::NAN;
        assert!(assets.store(d, vec![[0.0; 4]; 4]).is_err());
        let mut d = description(Hash128(5));
        d.id = "../bad".into();
        assert!(assets.export_asset(&d).is_err());
    }
}
