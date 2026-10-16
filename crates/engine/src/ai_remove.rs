//! Explicit, local learned inpainting. Generated repairs are separate durable edit assets.
mod assets;
use crate::{
    Session,
    media::{DecodedSource, SourceLevel, SourceRef, content_key},
};
pub use assets::AiAssets;
use lightcraft_develop::{AiPatch, DevelopSettings, Spot, SpotMode};
use lightcraft_geom::{Point, Rect};
use lightcraft_preview::{Hash128, Hasher128};
use lightcraft_raster::Rgb32f;
use serde_json::{Value, json};
use std::sync::Arc;

const SIDE: usize = 800;
const MODEL: &str = "LaMa-CoreML-5ed76e3799ab";

pub struct Job {
    pub photo: lightcraft_catalog::PhotoId,
    generation: u64,
    source: SourceRef,
    source_key: Hash128,
    before: Arc<DevelopSettings>,
    info: lightcraft_pipeline::SourceInfo,
    assets: Arc<AiAssets>,
    stroke: Vec<Point>,
    radius: f64,
    opacity: f64,
    replace: Option<usize>,
    source_coordinates: bool,
}
pub struct Generated {
    patch: AiPatch,
    original: DecodedSource,
}
impl Job {
    pub fn prepare(session: &mut Session, params: &Value, regenerate: bool) -> Result<Self, String> {
        if !cfg!(target_os = "macos") {
            return Err("AI Remove requires the macOS desktop engine".into());
        }
        if session.interaction.is_some() {
            return Err("Finish the current adjustment before using AI Remove".into());
        }
        let photo = session.active().ok_or("Select a photo first")?;
        let p = session.catalog.photo(photo).cloned().ok_or("Photo unavailable")?;
        let before = p.develop.clone();
        let replace = if regenerate {
            Some(
                params
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .or(session.active_spot)
                    .ok_or("Select an AI repair to regenerate")?,
            )
        } else {
            None
        };
        let (stroke, radius, opacity) = if let Some(i) = replace {
            let spot = before.spots.get(i).filter(|s| s.mode == SpotMode::Ai).ok_or("Select an AI repair to regenerate")?;
            let patch = spot.ai_patch.as_ref().ok_or("Missing AI repair description")?;
            (patch.stroke.clone(), patch.radius, spot.opacity)
        } else {
            if before.spots.iter().filter(|s| s.mode == SpotMode::Ai).count() >= 64 {
                return Err("Too many repairs on this photo; delete an unused repair first".into());
            }
            let points = params
                .get("points")
                .and_then(Value::as_array)
                .filter(|p| !p.is_empty() && p.len() <= 4096)
                .ok_or("Paint a bounded area to remove")?;
            let stroke = points
                .iter()
                .map(|p| {
                    Ok(Point::new(
                        p.get(0).and_then(Value::as_f64).ok_or("Invalid AI Remove point")?,
                        p.get(1).and_then(Value::as_f64).ok_or("Invalid AI Remove point")?,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            (stroke, params.get("size").and_then(Value::as_f64).unwrap_or(0.02), params.get("opacity").and_then(Value::as_f64).unwrap_or(100.0))
        };
        if stroke.is_empty()
            || stroke.len() > 4096
            || stroke.iter().any(|p| ![p.x, p.y].iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)))
            || !radius.is_finite()
            || !(0.001..=0.25).contains(&radius)
            || !opacity.is_finite()
            || !(0.0..=100.0).contains(&opacity)
        {
            return Err("Invalid AI Remove stroke, radius or opacity".into());
        }
        Ok(Self {
            photo,
            generation: session.library_generation(),
            source: session.media.source_ref(&p, SourceLevel::Full),
            source_key: Hasher128::new().str(&content_key(&p)).finish(),
            before,
            info: crate::media::source_info(&p),
            assets: session.media.ai_assets.clone(),
            stroke,
            radius,
            opacity,
            replace,
            source_coordinates: regenerate,
        })
    }
    pub fn run(&self) -> Result<Generated, String> {
        let original = self.source.load_source()?;
        let previous = self.before.spots.get(..self.replace.unwrap_or(self.before.spots.len())).ok_or("AI repair no longer exists")?;
        let source = self.assets.composite(&original.image, previous, Some(self.source_key))?;
        if source.width == 0 || source.height == 0 || source.width.checked_mul(source.height) != Some(source.data.len()) {
            return Err("Invalid AI Remove source".into());
        }
        let info = original.info_or(self.info.clone());
        let frame = lightcraft_pipeline::frame_for(&source, &info, &self.before, false);
        let stroke = if self.source_coordinates {
            self.stroke.clone()
        } else {
            self.stroke.iter().map(|p| to_source(&frame, *p)).collect::<Result<Vec<_>, _>>()?
        };
        let radius = if self.source_coordinates {
            self.radius
        } else {
            // Match the visible brush after lens/perspective changes using local source scale.
            let long = source.width.max(source.height) as f64;
            self.stroke
                .iter()
                .zip(&stroke)
                .filter_map(|(p, q)| {
                    let delta = self.radius * frame.ow.max(frame.oh) / frame.ow;
                    let offset = Point::new((p.x + delta).min(1.0), p.y);
                    let distance = offset.x - p.x;
                    if distance < 1e-6 {
                        return None;
                    }
                    let other = to_source(&frame, offset).ok()?;
                    Some(((other.x - q.x) * source.width as f64).hypot((other.y - q.y) * source.height as f64) / long * delta / distance)
                })
                .fold(self.radius, f64::max)
                .clamp(0.001, 0.25)
        };
        let Input { bounds, image: input, mask, alpha, gain } = make_input(&source, &stroke, radius)?;
        #[cfg(target_os = "macos")]
        let output = lightcraft_apple::inpaint(&input, &mask)?;
        #[cfg(not(target_os = "macos"))]
        let output = unavailable_inpaint(&input, &mask)?;
        let to_working = lightcraft_color::SRGB.to_space(&lightcraft_color::WORKING);
        let data = output
            .into_iter()
            .zip(alpha)
            .map(|(p, a)| {
                if a == 0.0 {
                    [0.0; 4]
                } else {
                    let rgb = to_working.apply_f32([p[0], p[1], p[2]].map(|v| lightcraft_color::transfer::decode_srgb8(v) / gain));
                    [rgb[0], rgb[1], rgb[2], a]
                }
            })
            .collect();
        let patch = self.assets.store(
            AiPatch {
                id: String::new(),
                model: MODEL.into(),
                source: self.source_key.to_string(),
                bounds,
                width: SIDE,
                height: SIDE,
                stroke,
                radius,
            },
            data,
        )?;
        Ok(Generated { patch, original })
    }
    pub fn accept(self, session: &mut Session, generated: Generated) -> crate::Result<Value> {
        let current = session.catalog.photo(self.photo);
        if session.library_generation() != self.generation
            || session.interaction.is_some()
            || current.is_none_or(|p| Hasher128::new().str(&content_key(p)).finish() != self.source_key || !Arc::ptr_eq(&p.develop, &self.before))
        {
            return Err(crate::EngineError::Other("AI Remove result discarded because the photo or its edits changed; try again".into()));
        }
        let mut settings = (*self.before).clone();
        let spot = Spot {
            mode: SpotMode::Ai,
            points: self.stroke,
            size: self.radius,
            opacity: self.opacity,
            ai_patch: Some(generated.patch),
            ..Default::default()
        };
        let index = if let Some(index) = self.replace {
            let old = settings.spots.get_mut(index).ok_or_else(|| crate::EngineError::Other("AI repair no longer exists".into()))?;
            *old = spot;
            index
        } else {
            settings.spots.push(spot);
            settings.spots.len() - 1
        };
        session.set_develop(self.photo, settings, if self.replace.is_some() { "Regenerate AI Remove" } else { "AI Remove" })?;
        session.media.insert_source(self.photo, SourceLevel::Full, generated.original);
        if session.active() == Some(self.photo) {
            session.active_spot = Some(index);
        }
        Ok(json!({"index":index,"provider":"coreML","model":MODEL}))
    }
}
fn to_source(frame: &lightcraft_pipeline::geometry::Frame, p: Point) -> Result<Point, String> {
    let p = Point::new(p.x * frame.ow, p.y * frame.oh);
    let p = frame.warp.as_ref().map_or(p, |w| w.to_source(p, 1));
    let (x, y) = frame.orient.inverse().map(p.x / frame.ow, p.y / frame.oh, 1.0, 1.0);
    if !x.is_finite() || !y.is_finite() || !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return Err("Paint inside the photo to use AI Remove".into());
    }
    Ok(Point::new(x, y))
}
struct Input {
    bounds: Rect,
    image: Vec<[u8; 4]>,
    mask: Vec<u8>,
    alpha: Vec<f32>,
    gain: f32,
}
// Context is bounded at model resolution; the repair is sampled at any preview/export resolution.
fn make_input(source: &Rgb32f, points: &[Point], radius: f64) -> Result<Input, String> {
    let (w, h) = (source.width as f64, source.height as f64);
    let long = w.max(h);
    let r = (radius * long).max(1.0);
    let mut bx = Rect::new(w, h, 0.0, 0.0);
    for p in points {
        bx.x0 = bx.x0.min(p.x * w);
        bx.y0 = bx.y0.min(p.y * h);
        bx.x1 = bx.x1.max(p.x * w);
        bx.y1 = bx.y1.max(p.y * h);
    }
    let side = (bx.width().max(bx.height()) + r * 10.0).max(long * 0.15);
    let center = bx.center();
    let cw = side.min(w);
    let ch = side.min(h);
    let x = (center.x - cw / 2.0).clamp(0.0, w - cw);
    let y = (center.y - ch / 2.0).clamp(0.0, h - ch);
    let bounds = Rect::new(x / w, y / h, (x + cw) / w, (y + ch) / h);
    let to_srgb = lightcraft_color::WORKING.to_space(&lightcraft_color::SRGB);
    let mut levels: Vec<f32> =
        source.data.iter().step_by((source.data.len() / 4096).max(1)).map(|p| p[0].max(p[1]).max(p[2])).filter(|v| v.is_finite()).collect();
    levels.sort_unstable_by(f32::total_cmp);
    let gain = 1.0 / levels.get(levels.len() * 99 / 100).copied().unwrap_or(1.0).max(1.0);
    let image = (0..SIDE * SIDE)
        .map(|i| {
            let (sx, sy) = (x + ((i % SIDE) as f64 + 0.5) * cw / SIDE as f64, y + ((i / SIDE) as f64 + 0.5) * ch / SIDE as f64);
            let pixel = sample(source, sx, sy);
            let rgb = to_srgb.apply_f32(pixel).map(|v| lightcraft_color::transfer::encode_srgb8(v * gain));
            [rgb[0], rgb[1], rgb[2], 255]
        })
        .collect();
    let mut alpha = vec![0.0f32; SIDE * SIDE];
    let mut mask = vec![0u8; SIDE * SIDE];
    let feather = (cw.max(ch) / SIDE as f64 * 1.5).max(r * 0.06);
    let dilation = cw.max(ch) / SIDE as f64 * 3.0;
    for segment in points.windows(2).map(|p| (p[0], p[1])).chain(points.first().map(|p| (*p, *p))) {
        let a = Point::new(segment.0.x * w, segment.0.y * h);
        let b = Point::new(segment.1.x * w, segment.1.y * h);
        let reach = r + dilation;
        let x0 = (((a.x.min(b.x) - reach - x) / cw * SIDE as f64).floor().max(0.0) as usize).min(SIDE);
        let x1 = (((a.x.max(b.x) + reach - x) / cw * SIDE as f64).ceil().max(0.0) as usize).min(SIDE);
        let y0 = (((a.y.min(b.y) - reach - y) / ch * SIDE as f64).floor().max(0.0) as usize).min(SIDE);
        let y1 = (((a.y.max(b.y) + reach - y) / ch * SIDE as f64).ceil().max(0.0) as usize).min(SIDE);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let denom = dx * dx + dy * dy;
        for my in y0..y1 {
            for mx in x0..x1 {
                let (px, py) = (x + (mx as f64 + 0.5) * cw / SIDE as f64, y + (my as f64 + 0.5) * ch / SIDE as f64);
                let t = if denom > 1e-12 { ((px - a.x) * dx + (py - a.y) * dy) / denom } else { 0.0 }.clamp(0.0, 1.0);
                let dist = ((px - a.x - t * dx).powi(2) + (py - a.y - t * dy).powi(2)).sqrt();
                let i = my * SIDE + mx;
                if dist <= reach
                    && let Some(v) = mask.get_mut(i)
                {
                    *v = 255;
                }
                if let Some(v) = alpha.get_mut(i) {
                    *v = v.max(((r - dist) / feather).clamp(0.0, 1.0) as f32);
                }
            }
        }
    }
    let painted = mask.iter().filter(|v| **v > 0).count();
    if painted == 0 || painted > SIDE * SIDE * 3 / 5 {
        return Err("Select a smaller object and leave some surrounding context for AI Remove".into());
    }
    Ok(Input { bounds, image, mask, alpha, gain })
}
fn sample(image: &Rgb32f, x: f64, y: f64) -> [f32; 3] {
    let x = (x - 0.5).clamp(0.0, (image.width - 1) as f64);
    let y = (y - 0.5).clamp(0.0, (image.height - 1) as f64);
    let (ix, iy) = (x as usize, y as usize);
    let (fx, fy) = ((x - ix as f64) as f32, (y - iy as f64) as f32);
    let mut out = [0.0; 3];
    for (sx, sy, w) in [
        (ix, iy, (1.0 - fx) * (1.0 - fy)),
        ((ix + 1).min(image.width - 1), iy, fx * (1.0 - fy)),
        (ix, (iy + 1).min(image.height - 1), (1.0 - fx) * fy),
        ((ix + 1).min(image.width - 1), (iy + 1).min(image.height - 1), fx * fy),
    ] {
        if let Some(p) = image.data.get(sy * image.width + sx) {
            for c in 0..3 {
                out[c] += p[c] * w;
            }
        }
    }
    out
}

#[cfg(not(target_os = "macos"))]
fn unavailable_inpaint(_: &[[u8; 4]], _: &[u8]) -> Result<Vec<[u8; 4]>, String> {
    Err("AI Remove requires the macOS desktop engine".into())
}

/// Pins follow the immutable source repair through later orientation/lens/perspective edits.
pub fn display_stroke(patch: &AiPatch, frame: &lightcraft_pipeline::geometry::Frame) -> Vec<Point> {
    patch
        .stroke
        .iter()
        .map(|p| {
            let (x, y) = frame.orient.map(p.x, p.y, 1.0, 1.0);
            let target = Point::new(x * frame.ow, y * frame.oh);
            let mut t = target;
            if let Some(warp) = &frame.warp {
                // Invert the smooth two-dimensional sampling map with bounded Newton iterations.
                for _ in 0..12 {
                    let p = warp.to_source(t, 1);
                    let error = Point::new(p.x - target.x, p.y - target.y);
                    if error.x.hypot(error.y) < 0.01 {
                        break;
                    }
                    let dx = warp.to_source(Point::new(t.x + 0.5, t.y), 1);
                    let dy = warp.to_source(Point::new(t.x, t.y + 0.5), 1);
                    let (a, b, c, d) = ((dx.x - p.x) * 2.0, (dy.x - p.x) * 2.0, (dx.y - p.y) * 2.0, (dy.y - p.y) * 2.0);
                    let det = a * d - b * c;
                    if !det.is_finite() || det.abs() < 1e-8 {
                        break;
                    }
                    t.x -= (d * error.x - b * error.y) / det;
                    t.y -= (-c * error.x + a * error.y) / det;
                }
            }
            Point::new(t.x / frame.ow, t.y / frame.oh)
        })
        .filter(|p| p.x.is_finite() && p.y.is_finite())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn prepared(session: &mut Session) -> (Job, Generated) {
        let photo = session.active().unwrap();
        let p = session.catalog.photo(photo).unwrap().clone();
        let source_key = Hasher128::new().str(&content_key(&p)).finish();
        let original = DecodedSource::new(Arc::new(Rgb32f::filled(16, 12, [0.2; 3])), None);
        let patch = session
            .media
            .ai_assets
            .store(
                AiPatch {
                    id: String::new(),
                    model: MODEL.into(),
                    source: source_key.to_string(),
                    bounds: Rect::UNIT,
                    width: 2,
                    height: 2,
                    stroke: vec![Point::new(0.25, 0.25)],
                    radius: 0.02,
                },
                vec![[0.6, 0.2, 0.1, 1.0]; 4],
            )
            .unwrap();
        let job = Job {
            photo,
            generation: session.library_generation(),
            source: SourceRef::Loaded(Box::new(original.clone())),
            source_key,
            before: p.develop.clone(),
            info: crate::media::source_info(&p),
            assets: session.media.ai_assets.clone(),
            stroke: patch.stroke.clone(),
            radius: 0.02,
            opacity: 100.0,
            replace: None,
            source_coordinates: false,
        };
        (job, Generated { patch, original })
    }
    #[test]
    fn accepted_repairs_use_the_common_render_path_and_survive_undo_redo_without_mutating_source() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        let (job, result) = prepared(&mut s);
        let original = result.original.clone();
        job.accept(&mut s, result).unwrap();
        for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
            s.media.insert_source(id, level, original.clone());
        }
        let d = s.develop_of(id).unwrap();
        let with = s.render_job(id, 16, 12, false, true).unwrap().run().rendered.unwrap().image;
        let before = s.render_job(id, 16, 12, true, true).unwrap().run().rendered.unwrap().image;
        assert_ne!(with, before);
        assert_eq!(original.image.data, vec![[0.2; 3]; 192]);
        assert!(serde_json::to_vec(&d.spots[0]).unwrap().len() < 1500, "pixels stay outside history JSON");
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(s.develop_of(id).unwrap().spots.is_empty());
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(s.develop_of(id).unwrap().spots, d.spots);
        let again = s.render_job(id, 16, 12, false, true).unwrap().run().rendered.unwrap().image;
        assert_eq!(again, with);
        s.execute("develop.set", &json!({"control":"light.exposure","value":1.0})).unwrap();
        let later = s.render_job(id, 16, 12, false, true).unwrap().run().rendered.unwrap().image;
        assert_ne!(later, with);
        assert_eq!(s.develop_of(id).unwrap().spots, d.spots);
        assert!(s.execute("spot.update", &json!({"index":0,"move":[0.1,0.0]})).is_err());
        assert!(s.execute("spot.add", &json!({"mode":"ai","points":[[0.5,0.5]]})).is_err());
    }
    #[test]
    fn asynchronous_result_cannot_overwrite_newer_edits_or_another_library() {
        let mut s = Session::with_demo();
        let (job, result) = prepared(&mut s);
        s.execute("develop.set", &json!({"control":"light.exposure","value":1.0})).unwrap();
        assert!(job.accept(&mut s, result).is_err());
        assert!(s.develop_of(s.active().unwrap()).unwrap().spots.is_empty());
        let (job, result) = prepared(&mut s);
        s = s.with_apple_photos(None);
        assert!(job.accept(&mut s, result).is_err());
    }
    #[test]
    fn source_coordinates_follow_orientation_perspective_and_sampling_at_any_resolution() {
        let settings = DevelopSettings { orientation: lightcraft_geom::Orientation::Rotate90, ..Default::default() };
        let frame = lightcraft_pipeline::geometry::Frame::new(1000, 600, &settings, false);
        let raw = Point::new(0.25, 0.7);
        let d = AiPatch {
            id: String::new(),
            model: MODEL.into(),
            source: Hash128(1).to_string(),
            bounds: Rect::UNIT,
            width: 1,
            height: 1,
            stroke: vec![raw],
            radius: 0.02,
        };
        let display = display_stroke(&d, &frame);
        let roundtrip = to_source(&frame, display[0]).unwrap();
        assert!((roundtrip.x - raw.x).abs() < 1e-8 && (roundtrip.y - raw.y).abs() < 1e-8);
        let input = make_input(&Rgb32f::filled(200, 120, [0.2; 3]), &[Point::new(0.5, 0.5)], 0.02).unwrap();
        assert_eq!(input.mask.len(), SIDE * SIDE);
        assert!(input.mask.contains(&255));
        assert!(input.alpha.contains(&0.0));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn photos_generation_requires_the_explicit_original_action() {
        use lightcraft_catalog::{Op, Photo, PhotoId, Source};
        let mut s = Session::new().with_apple_photos(None);
        let id = PhotoId(1);
        s.catalog
            .apply(Op::AddPhoto {
                photo: Box::new(Photo::new(
                    id,
                    Source::ApplePhotos { asset: "no-resource-read".into(), revision: "1".into() },
                    "test.CR3",
                    "CR3",
                    20,
                    16,
                    "",
                )),
            })
            .unwrap();
        s.selection = crate::Selection::single(id);
        let job = Job::prepare(&mut s, &json!({"points":[[0.5,0.5]],"size":0.02}), false).unwrap();
        assert_eq!(job.run().err().unwrap(), crate::apple::ORIGINAL_REQUIRED);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod model_tests {
    use super::*;
    #[test]
    fn bundled_model_removes_a_synthetic_object_without_touching_the_original_or_unpainted_pixels() {
        if !std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/apple-models/LaMa.mlmodelc").is_dir() {
            return;
        }
        let mut s = Session::with_demo();
        let (mut job, _) = super::tests::prepared(&mut s);
        let to_working = lightcraft_color::SRGB.to_space(&lightcraft_color::WORKING);
        let background = to_working.apply_f32([0.3, 0.4, 0.5]);
        let object = to_working.apply_f32([0.9, 0.02, 0.01]);
        let original =
            Arc::new(Rgb32f::from_fn(600, 400, |x, y| if (280..320).contains(&x) && (180..220).contains(&y) { object } else { background }));
        job.source = SourceRef::Loaded(Box::new(DecodedSource::new(original.clone(), None)));
        job.stroke = vec![Point::new(0.5, 0.5)];
        job.radius = 0.06;
        let result = job.run().unwrap();
        let patch = result.patch.clone();
        let spot = Spot { mode: SpotMode::Ai, ai_patch: Some(patch.clone()), ..Default::default() };
        let rendered = job.assets.composite(&original, &[spot], Some(job.source_key)).unwrap();
        assert_eq!(original.data[200 * 600 + 300], object);
        assert_eq!(rendered.data[0], original.data[0]);
        let filled = lightcraft_color::WORKING.to_space(&lightcraft_color::SRGB).apply_f32(rendered.data[200 * 600 + 300]);
        assert!(filled[0] < 0.6 && filled[1] > 0.2, "AI should fill the red object with background: {filled:?}");
        let saved = job.assets.export_asset(&patch).unwrap();
        let reopened = AiAssets::default();
        reopened.import_asset(&saved).unwrap();
        let spot = Spot { mode: SpotMode::Ai, ai_patch: Some(patch), ..Default::default() };
        assert_eq!(reopened.composite(&original, &[spot], Some(job.source_key)).unwrap().data, rendered.data);
    }
}
