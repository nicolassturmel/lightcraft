//! Original-preview sky selection. Vision supplies foreground protection; this pass separates
//! the remaining sky from buildings/water/ground using competing edge connectivity.
//! It is deliberately not advertised as a semantic sky model: ambiguous scenes need Brush.

use lightcraft_raster::{
    Plane, Rgb32f,
    resample::{self, Filter},
};
use std::{cmp::Reverse, collections::BinaryHeap};

const EDGE: usize = 512;

#[derive(Clone, Copy, Default)]
struct Feature {
    lab: [f32; 3],
    plausible: bool,
    blue_saturated: bool,
}

fn difference(a: Feature, b: Feature) -> f32 {
    (a.lab[0] - b.lab[0]).abs() + 2.0 * ((a.lab[1] - b.lab[1]).powi(2) + (a.lab[2] - b.lab[2]).powi(2)).sqrt()
}

fn at(features: &[Feature], width: usize, height: usize, x: usize, y: usize) -> Feature {
    features.get(y.min(height.saturating_sub(1)).saturating_mul(width).saturating_add(x.min(width.saturating_sub(1)))).copied().unwrap_or_default()
}

/// A continuous horizon (including a tilted sea horizon) must have an edge across most of the
/// frame. Isolated cloud edges / roof edges cannot set a global horizontal cut.
fn water_horizon(features: &[Feature], foreground: &Plane) -> Option<(f32, f32)> {
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
                // A dark cloud bank is still sky. Only use this prior to anchor strongly blue
                // water, where a bright wave can otherwise isolate it from the bottom border.
                if !above.plausible || !below.blue_saturated || foreground.get(x, y - band) > 0.5 {
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

/// Minimum edge barrier to any seed. A facade connected to the ground belongs to that side of
/// its outline, even when it is neutral/blue and reaches the top border. Unlike a permissive
/// flood fill, a small colour step cannot leak a sky seed through every subsequent wall pixel.
fn connectivity(edges: &[[u16; 2]], width: usize, seeds: &[bool], traversable: Option<&[bool]>) -> Vec<u16> {
    let mut costs = vec![u16::MAX; edges.len()];
    let mut queue = BinaryHeap::new();
    for (i, seed) in seeds.iter().copied().enumerate() {
        if seed && let Some(value) = costs.get_mut(i) {
            *value = 0;
            queue.push(Reverse((0_u16, i)));
        }
    }
    while let Some(Reverse((cost, i))) = queue.pop() {
        if costs.get(i).copied() != Some(cost) {
            continue;
        }
        let (x, y) = (i % width, i / width);
        let here = edges.get(i).copied().unwrap_or_default();
        let neighbours = [
            (x.checked_sub(1).map(|_| i - 1), i.checked_sub(1).and_then(|j| edges.get(j)).map(|e| e[0]).unwrap_or(0)),
            ((x + 1 < width).then_some(i + 1), here[0]),
            (y.checked_sub(1).map(|_| i - width), i.checked_sub(width).and_then(|j| edges.get(j)).map(|e| e[1]).unwrap_or(0)),
            (i.checked_add(width).filter(|&j| j < edges.len()), here[1]),
        ];
        for (next, edge) in neighbours {
            let Some(next) = next else { continue };
            if traversable.is_some_and(|allowed| !allowed.get(next).copied().unwrap_or(false)) {
                continue;
            }
            let value = cost.max(edge);
            if let Some(previous) = costs.get_mut(next)
                && value < *previous
            {
                *previous = value;
                queue.push(Reverse((value, next)));
            }
        }
    }
    costs
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
            Feature {
                lab: lightcraft_color::perceptual::oklab_from_2020(c.map(|v| v / (1.0 + v))),
                plausible: neutral || blue || warm,
                blue_saturated: blue && (max - min) / max > 0.40,
            }
        })
        .collect();
    let allowed: Vec<_> = features.iter().zip(&foreground.data).map(|(f, subject)| f.plausible && f.lab[0] > 0.12 && *subject < 0.85).collect();
    let edges: Vec<_> = features
        .iter()
        .enumerate()
        .map(|(i, here)| {
            let (x, y) = (i % w, i / w);
            [(x + 1, y), (x, y + 1)].map(|(nx, ny)| {
                let subject_edge = (foreground.get(x, y) - foreground.get(nx.min(w - 1), ny.min(h - 1))).abs() * 0.18;
                (difference(*here, at(&features, w, h, nx, ny)).max(subject_edge).min(1.0) * 65534.0) as u16
            })
        })
        .collect();
    let boundary = water_horizon(&features, &foreground);
    let ground_seeds: Vec<_> = allowed
        .iter()
        .enumerate()
        .map(|(i, ok)| {
            let (x, y) = (i % w, i / w);
            !ok || y == h - 1
                || boundary
                    .is_some_and(|(centre, slope)| y as f32 >= centre + slope * (x as f32 / w as f32 - 0.5) + ((h / 100).max(1) * 2 + 1) as f32)
        })
        .collect();
    let ground = connectivity(&edges, w, &ground_seeds, None);
    let border = |i: usize| i / w == 0 || (i / w < (h / 12).max(1) && (i.is_multiple_of(w) || i % w == w - 1));
    let scores: Vec<_> = features
        .iter()
        .enumerate()
        .map(|(i, &here)| {
            if !border(i) || !allowed.get(i).copied().unwrap_or(false) {
                return 0;
            }
            // Windows can have a large edge barrier of their own. Their repeated vertical
            // changes are not evidence of sky: prefer the broad, smooth border regions.
            let variation =
                (1..=4).map(|step| difference(here, at(&features, w, h, i % w, i / w + (h / 6).max(1) * step / 4))).fold(0.0_f32, f32::max);
            ground.get(i).copied().unwrap_or(0).saturating_sub((variation.min(1.0) * 65534.0) as u16)
        })
        .collect();
    let separation = scores.iter().copied().max().unwrap_or(0);
    // Seed only the border regions separated from the ground, not every plausible top pixel.
    // This also works when a building occupies most of the top or the horizon is strongly tilted.
    let threshold = separation.saturating_sub(separation / 8);
    let flat_sky = ground.iter().copied().max().unwrap_or(0) < 328 && allowed.iter().all(|ok| *ok);
    let sky_seeds: Vec<_> = allowed
        .iter()
        .enumerate()
        .map(|(i, ok)| *ok && border(i) && (flat_sky || (separation > 0 && scores.get(i).is_some_and(|&v| v >= threshold))))
        .collect();
    let sky = connectivity(&edges, w, &sky_seeds, Some(&allowed));
    let mut selected = Plane::new(w, h);
    for (i, alpha) in selected.data.iter_mut().enumerate() {
        if allowed.get(i).copied().unwrap_or(false)
            && sky.get(i).is_some_and(|&v| v != u16::MAX && (flat_sky || ground.get(i).is_some_and(|&g| v < g)))
        {
            *alpha = 1.0;
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
    fn facades_and_street_lights_touching_the_top_are_not_sky_seeds() {
        for gain in [0.08, 1.0, 6.0] {
            for wall_start in [40, 112] {
                for grey_sky in [false, true] {
                    let (mut image, horizon) = beach(160, 100, gain, true);
                    if grey_sky {
                        for y in 0..horizon - 12 {
                            for x in 0..160 {
                                image.data[y * 160 + x] = [0.45, 0.46, 0.47].map(|v| v * gain);
                            }
                        }
                    }
                    // A neutral facade reaching the upper edge, with windows and cornices.
                    // It is not protected by Vision: large buildings are often background.
                    for y in 0..100 {
                        for x in wall_start..160 {
                            let colour = if y % 20 < 10 && x % 16 < 6 { [0.13, 0.14, 0.15] } else { [0.32, 0.33, 0.34] };
                            image.data[y * 160 + x] = colour.map(|v| v * gain);
                        }
                    }
                    let pole = wall_start * 2 / 3;
                    for y in 0..100 {
                        for x in pole..pole + 3 {
                            image.data[y * 160 + x] = [0.24, 0.25, 0.26].map(|v| v * gain);
                        }
                    }
                    for y in 15..18 {
                        for x in pole - 10..pole {
                            image.data[y * 160 + x] = [0.24, 0.25, 0.26].map(|v| v * gain);
                        }
                    }
                    let mask = selection(&image, &Plane::new(EDGE, EDGE)).unwrap();
                    let value = |x: usize, y: usize| mask.get((2 * x + 1) * EDGE / 320, (2 * y + 1) * EDGE / 200);
                    assert!(value(wall_start + 12, 25) < 0.02, "facade: {gain}, {wall_start}, {grey_sky}");
                    assert!(value(pole + 1, 30) < 0.02, "pole: {gain}, {wall_start}, {grey_sky}");
                    assert!(value(pole - 5, 16) < 0.02, "lamp: {gain}, {wall_start}, {grey_sky}");
                    assert!(value(wall_start / 3, 35) > 0.98, "sky: {gain}, {wall_start}, {grey_sky}");
                    assert!(value(wall_start / 3, 80) < 0.02, "water: {gain}, {wall_start}, {grey_sky}");
                }
            }
        }
    }

    #[test]
    fn bright_waves_do_not_disconnect_dark_water_from_ground() {
        let (mut image, _) = beach(160, 100, 1.0, true);
        for y in 80..85 {
            for x in 0..160 {
                image.data[y * 160 + x] = [0.7, 0.8, 0.9];
            }
        }
        let mask = selection(&image, &Plane::new(EDGE, EDGE)).unwrap();
        assert!(mask.get(100, 100) > 0.98, "sky remains selected");
        assert!(mask.get(100, 360) < 0.02, "water above the wave remains excluded");
        assert!(mask.get(100, 420) < 0.02, "wave remains excluded");
    }

    #[test]
    fn a_frame_filled_with_uniform_sky_has_no_ground_to_subtract() {
        let image = Rgb32f { width: 80, height: 60, data: vec![[0.34, 0.48, 0.63]; 4800] };
        assert!(selection(&image, &Plane::new(EDGE, EDGE)).unwrap().data.iter().all(|v| *v > 0.98));
    }

    #[test]
    fn no_sky_and_invalid_inputs_do_not_select_the_image() {
        let image = Rgb32f { width: 80, height: 60, data: vec![[0.05, 0.25, 0.03]; 4800] };
        assert!(selection(&image, &Plane::new(EDGE, EDGE)).unwrap().data.iter().all(|v| *v == 0.0));
        assert!(selection(&Rgb32f::new(0, 0), &Plane::new(1, 1)).is_err());
        assert!(selection(&image, &Plane { width: 1, height: 1, data: vec![f32::NAN] }).is_err());
    }
}
