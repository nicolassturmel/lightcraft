//! Pixel tests of the local (mask) Noise, Moiré and Defringe sliders.

use lightcraft_develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
use lightcraft_geom::Point;
use lightcraft_raster::{Rgb32f, Rgba8};

use crate::{RenderRequest, SourceInfo, render};

const W: usize = 240;
const H: usize = 120;

/// A deterministic hash in −1..1.
fn noise(x: usize, y: usize, k: u32) -> f32 {
    let mut v = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ k.wrapping_mul(0xcb1a_b31f);
    v ^= v >> 13;
    v = v.wrapping_mul(0x5bd1_e995);
    v ^= v >> 15;
    (v & 0xffff) as f32 / 32768.0 - 1.0
}

/// Settings with one mask covering the left half (x < 0.45), with `adjust`.
fn left_mask(adjust: LocalAdjustments) -> DevelopSettings {
    let shape = MaskShape::Linear { start: Point::new(0.45, 0.5), end: Point::new(0.46, 0.5) };
    let mut s = DevelopSettings::default();
    s.masks.push(Mask { id: 1, components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }], adjust, ..Default::default() });
    s
}

fn shot(src: &Rgb32f, s: &DevelopSettings) -> Rgba8 {
    render(src, &SourceInfo::default(), s, &RenderRequest::fit(W, H)).image
}

/// Standard deviation of `f(pixel)` over columns `x0..x1` (rows 4..H-4).
fn spread(img: &Rgba8, x0: usize, x1: usize, f: impl Fn([u8; 4]) -> f32) -> f32 {
    let v: Vec<f32> = (4..H - 4).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| f(img.data[y * W + x])).collect();
    let m = v.iter().sum::<f32>() / v.len() as f32;
    (v.iter().map(|a| (a - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
}

fn luma(p: [u8; 4]) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

fn chroma(p: [u8; 4]) -> f32 {
    p[0] as f32 - p[2] as f32
}

#[test]
fn local_curves_respect_coverage_amount_visibility_and_channel() {
    let src = Rgb32f::filled(W, H, [0.18; 3]);
    let source_before = src.data.clone();
    let base = shot(&src, &DevelopSettings::default());
    let mut settings = left_mask(LocalAdjustments::default());
    let neutral = shot(&src, &settings);
    assert_eq!(neutral, base, "legacy/neutral masks keep their rendering");
    settings.masks[0].curve.red = vec![Point::new(0.0, 0.0), Point::new(0.5, 0.62), Point::new(1.0, 1.0)];
    let on = shot(&src, &settings);
    let at = |image: &Rgba8, x: usize| image.data[H / 2 * W + x];
    let (plain, altered) = (at(&base, 30), at(&on, 30));
    assert!(altered[0] > plain[0] + 15, "red curve changes red within coverage");
    assert_eq!(&altered[1..], &plain[1..], "other channels stay unchanged");
    assert_eq!(at(&on, 200), at(&base, 200), "outside the mask stays bit-exact");
    settings.masks[0].adjust.amount = 50.0;
    let half = at(&shot(&src, &settings), 30)[0] as f64;
    assert!((half - (f64::from(plain[0]) + f64::from(altered[0])) * 0.5).abs() <= 1.0);
    settings.masks[0].adjust.amount = 0.0;
    assert_eq!(shot(&src, &settings), base, "zero Amount switches the curve off");
    settings.masks[0].adjust.amount = 100.0;
    settings.masks[0].visible = false;
    assert_eq!(shot(&src, &settings), base, "hidden curve switches off");
    settings.masks[0].visible = true;
    settings.masks[0].invert = true;
    let inverted = shot(&src, &settings);
    assert_eq!(at(&inverted, 30), at(&base, 30));
    assert!(at(&inverted, 200)[0] > plain[0] + 15);
    assert_eq!(src.data, source_before, "curves never modify the source pixels");
}

#[test]
fn local_curves_rebuild_from_cached_source_and_match_deep_export() {
    use crate::{DeepSamples, OutputDepth, StageCache, render_cached};
    use std::sync::Arc;
    let src = Arc::new(Rgb32f::from_fn(W, H, |x, _| [0.04 + 0.5 * x as f32 / W as f32; 3]));
    let mut settings = left_mask(LocalAdjustments::default());
    let req = RenderRequest::fit(W, H);
    let cache = StageCache::default();
    render_cached(&src, &SourceInfo::default(), &settings, &req, &cache);
    settings.masks[0].curve.master = vec![Point::new(0.0, 0.02), Point::new(0.5, 0.65), Point::new(1.0, 1.0)];
    settings.masks[0].curve.blue = vec![Point::new(0.0, 0.04), Point::new(1.0, 0.95)];
    settings.masks[0].curve.refine_saturation = 40.0;
    let cached = render_cached(&src, &SourceInfo::default(), &settings, &req, &cache).image;
    assert_eq!(cached, shot(&src, &settings), "curve edits use fresh finish tables with the cached source");
    let deep = render(&src, &SourceInfo::default(), &settings, &RenderRequest { depth: OutputDepth::U16, ..req }).deep.unwrap();
    let DeepSamples::U16(samples) = deep.samples else {
        panic!("16-bit export");
    };
    for (pixel, samples) in cached.data.iter().zip(samples.as_chunks::<3>().0) {
        for channel in 0..3 {
            let exported = (f64::from(samples[channel]) / 257.0).round() as u8;
            assert!(pixel[channel].abs_diff(exported) <= 1, "preview and export follow the same masked curve");
        }
    }
}

#[test]
fn local_noise_smooths_luminance_noise_inside_the_mask() {
    let src = Rgb32f::from_fn(W, H, |x, y| [0.18 * (1.0 + 0.12 * noise(x, y, 1)); 3]);
    let plain = shot(&src, &DevelopSettings::default());
    let base = spread(&plain, 10, 90, luma);
    let on = shot(&src, &left_mask(LocalAdjustments { noise: 100.0, ..Default::default() }));
    let off = shot(&src, &left_mask(LocalAdjustments { noise: -100.0, ..Default::default() }));
    let (smoothed, boosted) = (spread(&on, 10, 90, luma), spread(&off, 10, 90, luma));
    assert!(smoothed < 0.6 * base, "noise +100: {smoothed} vs {base}");
    assert!(boosted > 1.2 * base, "noise −100: {boosted} vs {base}");
    // outside the mask nothing changes
    assert_eq!(spread(&on, 150, 230, luma), spread(&plain, 150, 230, luma));
    // the mean brightness is kept
    let mean = |img: &Rgba8| (4..H - 4).flat_map(|y| (10..90).map(move |x| luma(img.data[y * W + x]))).sum::<f32>() / (80 * (H - 8)) as f32;
    assert!((mean(&on) - mean(&plain)).abs() < 2.0);
}

#[test]
fn local_moire_removes_colour_speckle_inside_the_mask() {
    let src = Rgb32f::from_fn(W, H, |x, y| {
        let n = 0.25 * noise(x, y, 2);
        [0.18 * (1.0 + n), 0.18, 0.18 * (1.0 - n)]
    });
    let plain = shot(&src, &DevelopSettings::default());
    let base = spread(&plain, 10, 90, chroma);
    let on = shot(&src, &left_mask(LocalAdjustments { moire: 100.0, ..Default::default() }));
    let reduced = spread(&on, 10, 90, chroma);
    assert!(reduced < 0.4 * base, "moiré +100: {reduced} vs {base}");
    assert_eq!(spread(&on, 150, 230, chroma), spread(&plain, 150, 230, chroma));
    // luminance is kept
    assert!((spread(&on, 10, 90, luma) - spread(&plain, 10, 90, luma)).abs() < 1.0);
    // positive Noise also calms colour noise (less than Moiré)
    let n = spread(&shot(&src, &left_mask(LocalAdjustments { noise: 100.0, ..Default::default() })), 10, 90, chroma);
    assert!(n < 0.8 * base && n > reduced, "{n}");
}

#[test]
fn local_defringe_desaturates_purple_edges_only() {
    // dark | bright edge at x = 40 (inside the mask) and x = 200 (outside), each with a purple
    // fringe column; a flat purple patch (no edge) at x 60..80
    let src = Rgb32f::from_fn(W, H, |x, _| {
        let fringe = x == 39 || x == 40 || x == 199 || x == 200;
        if fringe {
            [0.25, 0.04, 0.3]
        } else if (60..80).contains(&x) {
            [0.2, 0.08, 0.22]
        } else if (40..120).contains(&x) || x >= 200 {
            [0.7; 3]
        } else {
            [0.02; 3]
        }
    });
    let s = left_mask(LocalAdjustments { defringe: 100.0, ..Default::default() });
    let (plain, on) = (shot(&src, &DevelopSettings::default()), shot(&src, &s));
    let sat = |p: [u8; 4]| p[0].max(p[2]) as f32 - p[1] as f32;
    let at = |img: &Rgba8, x: usize| img.data[60 * W + x];
    assert!(sat(at(&plain, 40)) > 40.0, "a purple fringe to start with: {:?}", at(&plain, 40));
    assert!(sat(at(&on, 40)) < 0.5 * sat(at(&plain, 40)), "fringe in the mask: {:?} vs {:?}", at(&on, 40), at(&plain, 40));
    assert_eq!(at(&on, 200), at(&plain, 200), "outside the mask");
    let flat = |img: &Rgba8| sat(at(img, 70));
    assert!((flat(&on) - flat(&plain)).abs() <= 2.0, "flat purple kept: {} vs {}", flat(&on), flat(&plain));
}
