//! Edge refinement of Apple's semantic sky map, in original source coordinates. Class labels
//! supply both sky and non-sky seeds; this pass can only remove coverage, never invent sky.

use lightcraft_raster::{
    Plane, Rgb32f,
    resample::{self, Filter},
};
use std::{cmp::Reverse, collections::BinaryHeap};

const EDGE: usize = 512;
type Feature = [f32; 3];

fn difference(a: Feature, b: Feature) -> f32 {
    (a[0] - b[0]).abs() + 2.0 * ((a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn at(features: &[Feature], width: usize, height: usize, x: usize, y: usize) -> Feature {
    features.get(y.min(height.saturating_sub(1)).saturating_mul(width).saturating_add(x.min(width.saturating_sub(1)))).copied().unwrap_or_default()
}

/// Minimum image-edge barrier to any semantic seed. A thin pole connected to a classified
/// building can be excluded even when the model's coarse grid misses its upper outline.
fn connectivity(edges: &[[u16; 2]], width: usize, seeds: &[bool]) -> Vec<u16> {
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

fn valid_coverage(mask: &Plane) -> bool {
    mask.width.checked_mul(mask.height).is_some_and(|n| {
        mask.width > 0
            && mask.height > 0
            && n <= 1_048_576
            && mask.data.len() == n
            && mask.data.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
    })
}

/// Coverage is stretched to a stored square grid, with top-left origin. No horizon, sky colour,
/// or top-border heuristic is used: a uniform overcast sky and an interior remain distinguishable
/// only through the semantic model. With no usable contrast, retain its result unchanged.
pub(crate) fn selection(source: &Rgb32f, semantic: &Plane, foreground: &Plane) -> Result<Plane, String> {
    let count = source.width.checked_mul(source.height).filter(|&n| n > 0 && n <= 10_000_000).ok_or("invalid sky source size")?;
    if source.data.len() != count || source.data.iter().flatten().any(|v| !v.is_finite()) || !valid_coverage(semantic) || !valid_coverage(foreground)
    {
        return Err("invalid sky pixels or semantic coverage".into());
    }
    let image = resample::fit(source, EDGE, EDGE, Filter::Box);
    let mut selected = resample::resize(semantic, image.width, image.height, Filter::Bilinear);
    let foreground = resample::resize(foreground, image.width, image.height, Filter::Bilinear);
    let (w, h) = (image.width, image.height);
    let mut light: Vec<f32> = image.data.iter().take(w * (h / 10).max(1)).map(|c| lightcraft_color::luminance_2020(*c).max(0.0001)).collect();
    light.sort_unstable_by(f32::total_cmp);
    let gain = (0.35 / light.get(light.len() / 2).copied().unwrap_or(0.35)).clamp(1.0 / 64.0, 64.0);
    let features: Vec<_> = image
        .data
        .iter()
        .map(|c| {
            let c = c.map(|v| v.clamp(0.0, 1_000_000.0) * gain);
            lightcraft_color::perceptual::oklab_from_2020(c.map(|v| v / (1.0 + v)))
        })
        .collect();
    let edges: Vec<_> = features
        .iter()
        .enumerate()
        .map(|(i, &here)| {
            let (x, y) = (i % w, i / w);
            [(x + 1, y), (x, y + 1)].map(|(nx, ny)| (difference(here, at(&features, w, h, nx, ny)).min(1.0) * 65534.0) as u16)
        })
        .collect();
    let ground_seeds: Vec<_> = selected.data.iter().map(|v| *v < 0.05).collect();
    let ground = connectivity(&edges, w, &ground_seeds);
    let contrast: Vec<_> = features
        .iter()
        .enumerate()
        .map(|(i, &here)| {
            let (x, y) = (i % w, i / w);
            [(x.saturating_sub(3), y), (x + 3, y), (x, y.saturating_sub(3)), (x, y + 3)]
                .map(|(nx, ny)| difference(here, at(&features, w, h, nx, ny)))
                .into_iter()
                .fold(0.0_f32, f32::max)
        })
        .collect();
    let sky_seeds: Vec<_> = selected
        .data
        .iter()
        .zip(&ground)
        .zip(&contrast)
        .map(|((alpha, barrier), local)| *alpha > 0.95 && *barrier > 0 && *barrier >= (local.min(1.0) * 65534.0) as u16)
        .collect();
    if sky_seeds.iter().any(|v| *v) {
        let sky = connectivity(&edges, w, &sky_seeds);
        for (((alpha, sky), ground), contrast) in selected.data.iter_mut().zip(sky).zip(ground).zip(contrast) {
            if sky != u16::MAX && sky > ground.saturating_add(128) && contrast > 0.004 {
                *alpha = 0.0;
            }
        }
    }
    // Preserve Vision's soft subject contours. The stored map supplies antialiasing without
    // feathering sky colour into buildings or altering the original photograph.
    for (alpha, subject) in selected.data.iter_mut().zip(&foreground.data) {
        *alpha *= 1.0 - subject.clamp(0.0, 1.0);
    }
    Ok(resample::resize(&selected, EDGE, EDGE, Filter::Bilinear))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_labels_survive_uniform_overcast_and_blue_ground() {
        let image = Rgb32f { width: 80, height: 60, data: vec![[0.34, 0.48, 0.63]; 4800] };
        let semantic = Plane { width: 80, height: 60, data: (0..4800).map(|i| if i / 80 < 40 { 1.0 } else { 0.0 }).collect() };
        let mask = selection(&image, &semantic, &Plane::new(1, 1)).unwrap();
        assert!(mask.get(256, 100) > 0.98, "semantic sky survives even without a colour boundary");
        assert!(mask.get(256, 450) < 0.02, "identically coloured non-sky is not added");
        let full = Plane { width: 1, height: 1, data: vec![1.0] };
        assert!(selection(&image, &full, &Plane::new(1, 1)).unwrap().data.iter().all(|v| *v > 0.98));
    }

    #[test]
    fn refinement_removes_ground_connected_poles_without_adding_sky() {
        for gain in [0.08, 1.0, 6.0] {
            let (w, h) = (160, 100);
            let image = Rgb32f {
                width: w,
                height: h,
                data: (0..w * h)
                    .map(|i| {
                        let (x, y) = (i % w, i / w);
                        let c = if y >= 60 || (78..82).contains(&x) { [0.24, 0.25, 0.26] } else { [0.45, 0.46, 0.47] };
                        c.map(|v| v * gain)
                    })
                    .collect(),
            };
            // Deliberately miss the narrow upper pole in the coarse semantic map.
            let semantic = Plane { width: w, height: h, data: (0..w * h).map(|i| if i / w < 60 { 1.0 } else { 0.0 }).collect() };
            let mask = selection(&image, &semantic, &Plane::new(1, 1)).unwrap();
            assert!(mask.get(256, 150) < 0.02, "pole: {gain}");
            assert!(mask.get(100, 150) > 0.98, "sky: {gain}");
            assert!(mask.get(100, 450) < 0.02, "ground: {gain}");
        }
    }

    #[test]
    fn soft_subject_edges_and_empty_semantic_masks_remain_valid() {
        let image = Rgb32f { width: 1, height: 1, data: vec![[0.45; 3]] };
        let sky = Plane { width: 1, height: 1, data: vec![1.0] };
        let subject = Plane { width: 1, height: 1, data: vec![0.5] };
        assert!(selection(&image, &sky, &subject).unwrap().data.iter().all(|v| (*v - 0.5).abs() < 0.0001));
        assert!(selection(&image, &Plane::new(1, 1), &Plane::new(1, 1)).unwrap().data.iter().all(|v| *v == 0.0));
        assert!(selection(&Rgb32f::new(0, 0), &sky, &subject).is_err());
        assert!(selection(&image, &Plane { width: 1, height: 1, data: vec![f32::NAN] }, &subject).is_err());
        assert!(selection(&image, &sky, &Plane { width: 0, height: 1, data: vec![] }).is_err());
    }
}
