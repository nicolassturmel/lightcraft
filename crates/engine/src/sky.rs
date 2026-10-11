//! Original-preview sky selection. Vision supplies foreground protection; this pass separates
//! the remaining sky from water/ground using border colour, connectivity and horizon edges.
//! It is deliberately not advertised as a semantic sky model: ambiguous scenes need Brush.

use lightcraft_raster::{
    Plane, Rgb32f,
    resample::{self, Filter},
};
use std::collections::VecDeque;

const EDGE: usize = 512;

#[derive(Clone, Copy, Default)]
struct Feature {
    lab: [f32; 3],
    plausible: bool,
}

fn difference(a: Feature, b: Feature) -> f32 {
    (a.lab[0] - b.lab[0]).abs() + 2.0 * ((a.lab[1] - b.lab[1]).powi(2) + (a.lab[2] - b.lab[2]).powi(2)).sqrt()
}

fn at(features: &[Feature], width: usize, height: usize, x: usize, y: usize) -> Feature {
    features.get(y.min(height.saturating_sub(1)).saturating_mul(width).saturating_add(x.min(width.saturating_sub(1)))).copied().unwrap_or_default()
}

/// A continuous horizon (including a tilted sea horizon) must have an edge across most of the
/// frame. Isolated cloud edges / roof edges cannot set a global horizontal cut.
fn horizon(features: &[Feature], foreground: &Plane) -> Option<(f32, f32)> {
    let (w, h) = (foreground.width, foreground.height);
    let band = (h / 100).max(1);
    let margin = (h / 30).max(band + 1);
    let columns = w.min(96);
    if h <= margin * 2 || columns < 8 {
        return None;
    }
    let mut best = (0.026_f32, 0.0, 0.0);
    for tilt in -24..=24 {
        let slope = tilt as f32 * h as f32 / 80.0;
        for centre in margin..h - margin {
            let mut total = 0.0;
            let mut coherent = 0;
            let mut usable = 0;
            for column in 0..columns {
                let x = column * (w - 1) / (columns - 1);
                let y = centre as f32 + slope * (x as f32 / w as f32 - 0.5);
                if y < margin as f32 || y >= (h - margin) as f32 {
                    continue;
                }
                let y = y as usize;
                let above = at(features, w, h, x, y - band);
                let below = at(features, w, h, x, y + band);
                if !above.plausible || foreground.get(x, y - band) > 0.5 {
                    continue;
                }
                usable += 1;
                // Sky is usually lighter than water/land. A bright wave below a dark sea
                // therefore cannot become the horizon. Chroma also catches sunset boundaries.
                let edge = (above.lab[0] - below.lab[0]).max(0.0)
                    + 2.0 * ((above.lab[1] - below.lab[1]).powi(2) + (above.lab[2] - below.lab[2]).powi(2)).sqrt();
                total += edge.min(0.15);
                coherent += usize::from(edge > 0.025);
            }
            if usable < columns * 2 / 3 || coherent * 10 < usable * 7 {
                continue;
            }
            let score = total / usable as f32 / (1.0 + 0.5 * centre as f32 / h as f32);
            if score > best.0 {
                best = (score, centre as f32, slope);
            }
        }
    }
    (best.0 > 0.026).then_some((best.1, best.2))
}

/// Coverage in oriented, uncropped source coordinates, stretched to the stored square grid.
pub(crate) fn selection(source: &Rgb32f, foreground: &Plane) -> Result<Plane, String> {
    let count = source.width.checked_mul(source.height).filter(|&n| n > 0 && n <= 10_000_000).ok_or("invalid sky source size")?;
    let mask_count = foreground.width.checked_mul(foreground.height).filter(|&n| n > 0 && n <= 1_048_576).ok_or("invalid sky foreground size")?;
    if source.data.len() != count
        || foreground.data.len() != mask_count
        || source.data.iter().flatten().chain(foreground.data.iter()).any(|v| !v.is_finite())
    {
        return Err("invalid sky pixels or foreground coverage".into());
    }
    let image = resample::fit(source, EDGE, EDGE, Filter::Box);
    let foreground = resample::resize(foreground, image.width, image.height, Filter::Bilinear);
    let (w, h) = (image.width, image.height);
    let mut light: Vec<f32> = image.data.iter().take(w * (h / 10).max(1)).map(|c| lightcraft_color::luminance_2020(*c).max(0.0001)).collect();
    light.sort_unstable_by(f32::total_cmp);
    let gain = (0.35 / light.get(light.len() / 2).copied().unwrap_or(0.35)).clamp(1.0 / 64.0, 64.0);
    let transform = lightcraft_color::WORKING.to_space(&lightcraft_color::SRGB);
    let features: Vec<_> = image
        .data
        .iter()
        .map(|c| {
            let c = c.map(|v| v.clamp(0.0, 1_000_000.0) * gain);
            let rgb = transform.apply_f32(c).map(|v| v.max(0.0));
            let max = rgb.into_iter().fold(0.0001_f32, f32::max);
            let min = rgb.into_iter().fold(f32::MAX, f32::min);
            let neutral = (max - min) / max < 0.30;
            let blue = rgb[2] >= rgb[0] * 0.92 && rgb[2] >= rgb[1] * 0.92;
            let warm = rgb[0] >= rgb[1] && rgb[1] >= rgb[2] && rgb[1] > rgb[0] * 0.45 && rgb[2] > rgb[1] * 0.35;
            Feature { lab: lightcraft_color::perceptual::oklab_from_2020(c.map(|v| v / (1.0 + v))), plausible: neutral || blue || warm }
        })
        .collect();
    let boundary = horizon(&features, &foreground);
    let allowed = |x: usize, y: usize| {
        let f = at(&features, w, h, x, y);
        let limit = boundary.map(|(centre, slope)| centre + slope * (x as f32 / w as f32 - 0.5)).unwrap_or(h as f32);
        f.plausible && f.lab[0] > 0.12 && foreground.get(x, y) < 0.85 && (y as f32 + 0.5) < limit
    };
    let mut selected = Plane::new(w, h);
    let mut queue = VecDeque::new();
    for x in 0..w {
        if allowed(x, 0) {
            if let Some(v) = selected.data.get_mut(x) {
                *v = 1.0;
            }
            queue.push_back((x, 0));
        }
    }
    // Side-border sky remains reachable when a tree or roof covers the very top of the photo.
    for y in 1..(h / 12).max(1) {
        for x in [0, w - 1] {
            if allowed(x, y) {
                if let Some(v) = selected.data.get_mut(y * w + x) {
                    *v = 1.0;
                }
                queue.push_back((x, y));
            }
        }
    }
    while let Some((x, y)) = queue.pop_front() {
        let here = at(&features, w, h, x, y);
        for (nx, ny) in [(x.saturating_sub(1), y), ((x + 1).min(w - 1), y), (x, y.saturating_sub(1)), (x, (y + 1).min(h - 1))] {
            let index = ny * w + nx;
            if selected.data.get(index).is_some_and(|v| *v == 0.0) && allowed(nx, ny) && difference(here, at(&features, w, h, nx, ny)) < 0.18 {
                if let Some(v) = selected.data.get_mut(index) {
                    *v = 1.0;
                }
                queue.push_back((nx, ny));
            }
        }
    }
    // Keep Vision's soft subject contours instead of blurring the sky into hair/trees. One-pixel
    // antialiasing comes from resampling; the old 1%-of-frame feather is deliberately absent.
    for (alpha, subject) in selected.data.iter_mut().zip(&foreground.data) {
        *alpha *= 1.0 - subject.clamp(0.0, 1.0);
    }
    Ok(resample::resize(&selected, EDGE, EDGE, Filter::Bilinear))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beach(w: usize, h: usize, gain: f32, tilted: bool) -> (Rgb32f, usize) {
        let horizon = h * 3 / 5;
        let data = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let edge = horizon as isize + if tilted { (x as isize - w as isize / 2) / 8 } else { 0 };
                let c = if (y as isize) < edge {
                    // Blue sky plus grey clouds, down beyond the old heuristic's 25%-height cutoff.
                    if x > w / 3 && x < w * 2 / 3 && y > h / 8 && y < h / 3 { [0.6, 0.61, 0.62] } else { [0.34, 0.48, 0.63] }
                } else {
                    [0.12, 0.20, 0.28]
                }; // blue water must not count as sky
                c.map(|v| v * gain)
            })
            .collect();
        (Rgb32f { width: w, height: h, data }, horizon)
    }

    #[test]
    fn sky_follows_the_horizon_including_clouds_and_excludes_blue_water() {
        for gain in [0.08, 1.0, 6.0] {
            for tilted in [false, true] {
                let (image, horizon) = beach(160, 100, gain, tilted);
                let mask = selection(&image, &Plane::new(EDGE, EDGE)).unwrap();
                assert!(mask.get(256, 130) > 0.98, "clouds: {gain}, {tilted}");
                assert!(mask.get(100, 260) > 0.98, "low sky: {gain}, {tilted}");
                assert!(mask.get(256, 400) < 0.02, "water: {gain}, {tilted}, horizon {horizon}");
                let selected = mask.data.iter().filter(|v| **v > 0.5).count() as f32 / mask.data.len() as f32;
                assert!((0.56..0.64).contains(&selected), "coverage {selected}: {gain}, {tilted}");
            }
        }
    }

    #[test]
    fn sky_retains_soft_vision_edges_and_rejects_disconnected_blue_objects() {
        let (mut image, _) = beach(160, 100, 1.0, false);
        // A patch of blue clothing under the horizon is not connected to sky.
        for y in 80..95 {
            for x in 40..90 {
                image.data[y * 160 + x] = [0.34, 0.48, 0.63];
            }
        }
        let mut foreground = Plane::new(EDGE, EDGE);
        for y in 0..290 {
            for x in 250..270 {
                foreground.data[y * EDGE + x] = 1.0;
            }
        }
        for y in 0..290 {
            foreground.data[y * EDGE + 249] = 0.5;
        }
        let mask = selection(&image, &foreground).unwrap();
        assert!(mask.get(260, 100) < 0.02, "foreground remains protected");
        assert!(mask.get(100, 250) > 0.98, "sky alongside the foreground stays selected");
        assert!(mask.get(200, 450) < 0.02, "blue foreground must not become sky");
        assert!(mask.data.iter().any(|v| (0.1..0.9).contains(v)), "antialiased subject boundary");
    }

    #[test]
    fn no_sky_and_invalid_inputs_do_not_select_the_image() {
        let image = Rgb32f { width: 80, height: 60, data: vec![[0.05, 0.25, 0.03]; 4800] };
        assert!(selection(&image, &Plane::new(EDGE, EDGE)).unwrap().data.iter().all(|v| *v == 0.0));
        assert!(selection(&Rgb32f::new(0, 0), &Plane::new(1, 1)).is_err());
        assert!(selection(&image, &Plane { width: 1, height: 1, data: vec![f32::NAN] }).is_err());
    }
}
