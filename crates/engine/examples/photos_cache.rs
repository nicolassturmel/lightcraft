//! Offline cache maintenance and a local-file benchmark. Never contacts PhotoKit.
//! `cargo run -p lightcraft-engine --example photos_cache -- compact ADJUSTMENTS_DIR`
//! `cargo run -p lightcraft-engine --example photos_cache -- bench LOCAL_RAW SCRATCH_DIR`
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use lightcraft_catalog::Source;
use lightcraft_engine::{apple::OriginalCache, media::DecodedSource};
use lightcraft_preview::Hasher128;
use serde_json::json;
use std::{
    fs::File,
    io::{BufWriter, Read, Write},
    path::Path,
    sync::Arc,
    time::Instant,
};

fn same(a: &DecodedSource, b: &DecodedSource) -> Result<(), String> {
    if a.image.width != b.image.width
        || a.image.height != b.image.height
        || a.info != b.info
        || a.image.data.len() != b.image.data.len()
        || !a.image.data.iter().flatten().zip(b.image.data.iter().flatten()).all(|(a, b)| a.to_bits() == b.to_bits())
    {
        return Err("Cache changed the source pixels or decoder facts".into());
    }
    Ok(())
}

fn bench(input: &Path, scratch: &Path) -> Result<(), String> {
    if scratch.exists() {
        return Err("Use a new scratch directory".into());
    }
    let mut bytes = Vec::new();
    File::open(input).map_err(|e| e.to_string())?.take(512_000_001).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > 512_000_000 {
        return Err("Input exceeds the resource limit".into());
    }
    let suffix = input.extension().and_then(|s| s.to_str()).ok_or("Missing file suffix")?;
    let t = Instant::now();
    let mut original = lightcraft_engine::apple::decode_cached_original(&bytes, suffix, true)?;
    let initial_ms = t.elapsed().as_millis();
    let source = Source::ApplePhotos { asset: "local-benchmark-only".into(), revision: "r1".into() };
    let info = original.info.as_ref().ok_or("Missing decoder facts")?;
    let header = serde_json::to_vec(&json!({"source":source,"width":original.image.width,"height":original.image.height,
        "raw":info.raw,"temperature":info.as_shot_temp,"tint":info.as_shot_tint,"relative_wb":info.relative_wb}))
    .map_err(|e| e.to_string())?;
    let dir = scratch.join("Original Cache");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let key = Hasher128::new().str("Photos original pixels v1").str("local-benchmark-only").str("r1").finish();
    let path = dir.join(format!("{key}.lcraw"));
    let mut writer = BufWriter::new(File::create(&path).map_err(|e| e.to_string())?);
    writer
        .write_all(b"LCPRAW01")
        .and_then(|_| writer.write_all(&(header.len() as u32).to_le_bytes()))
        .and_then(|_| writer.write_all(&header))
        .map_err(|e| e.to_string())?;
    for value in original.image.data.iter().flatten() {
        writer.write_all(&value.to_le_bytes()).map_err(|e| e.to_string())?;
    }
    writer.flush().map_err(|e| e.to_string())?;
    drop(writer);
    let legacy_bytes = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    let cache = OriginalCache::scan(scratch);
    let t = Instant::now();
    let legacy = cache.get(&source).ok_or("Legacy cache missing")?.load(usize::MAX)?;
    let legacy_ms = t.elapsed().as_millis();
    same(&original, &legacy)?;
    drop(legacy);
    let t = Instant::now();
    let (_, _, deflate_bytes) = cache.compact_legacy()?;
    let compact_ms = t.elapsed().as_millis();
    let t = Instant::now();
    let deflate = cache.get(&source).ok_or("Deflate cache missing")?.load(usize::MAX)?;
    let deflate_ms = t.elapsed().as_millis();
    same(&original, &deflate)?;
    drop(deflate);
    original.original = Some(Arc::new(lightcraft_engine::media::EncodedOriginal { bytes, suffix: suffix.into() }));
    let t = Instant::now();
    cache.store(&source, &original)?;
    let encoded_write_ms = t.elapsed().as_millis();
    let encoded_bytes = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    let cache = OriginalCache::scan(scratch);
    let mut reads = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        let decoded = cache.get(&source).ok_or("Encoded cache missing")?.load(usize::MAX)?;
        reads.push(t.elapsed().as_millis());
        same(&original, &decoded)?;
        // Full-size JPEG output must be byte-identical before and after re-opening the cache.
        if reads.len() == 1 {
            let settings = lightcraft_develop::DevelopSettings::default();
            let request = lightcraft_pipeline::RenderRequest::fit(original.image.width, original.image.height);
            let options = lightcraft_engine::export::ExportOptions { quality: 100, ..Default::default() };
            let a = lightcraft_pipeline::render(&original.image, info, &settings, &request).image;
            let a = lightcraft_engine::export::encode_image(&a, &options)?;
            let b = lightcraft_pipeline::render(&decoded.image, info, &settings, &request).image;
            let b = lightcraft_engine::export::encode_image(&b, &options)?;
            if a != b {
                return Err("JPEG export changed after cache reopening".into());
            }
        }
    }
    println!(
        "{}",
        json!({"width":original.image.width,"height":original.image.height,"initialDecodeMs":initial_ms,
        "legacyBytes":legacy_bytes,"legacyReadMs":legacy_ms,"deflateBytes":deflate_bytes,"compactAndVerifyMs":compact_ms,
        "deflateReadMs":deflate_ms,"encodedBytes":encoded_bytes,"encodedWriteMs":encoded_write_ms,
        "encodedReadMs":reads,"pixelBitsIdentical":true,"fullSizeJpegIdentical":true})
    );
    Ok(())
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [command, dir] if command == "compact" => {
            let (count, before, after) = OriginalCache::scan(Path::new(dir)).compact_legacy()?;
            println!("{}", json!({"compacted":count,"beforeBytes":before,"afterBytes":after}));
            Ok(())
        }
        [command, input, dir] if command == "bench" => bench(Path::new(input), Path::new(dir)),
        _ => Err("usage: photos_cache compact ADJUSTMENTS_DIR (desktop stopped) | bench LOCAL_RAW NEW_SCRATCH_DIR".into()),
    }
}
fn main() -> std::process::ExitCode {
    let result = run();
    lightcraft_engine::apple::shutdown_services();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
