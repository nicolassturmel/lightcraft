//! Large edit snapshots live beside local drafts; PhotoKit receives only a bounded reference.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const MAX_RECIPE: u64 = 16_000_000;
const MAX_ADJUSTMENT: usize = 64_000;

fn directory() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is unavailable for Photos adjustment recipes")?;
    Ok(PathBuf::from(home).join("Library/Application Support/LightCraft/Photos Adjustments/Export Recipes"))
}

fn original(recipe: &Value) -> Result<&str, String> {
    recipe
        .get("originalAssetId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 1024)
        .ok_or_else(|| "The Photos recipe has no valid original".into())
}

pub(super) fn encode(recipe: &Value) -> Result<Vec<u8>, String> {
    encode_in(recipe, &directory()?)
}

fn encode_in(recipe: &Value, directory: &Path) -> Result<Vec<u8>, String> {
    let asset = original(recipe)?;
    if recipe.get("schema").and_then(Value::as_u64) != Some(1) {
        return Err("Unsupported Photos recipe".into());
    }
    let data = serde_json::to_vec(recipe).map_err(|e| e.to_string())?;
    if data.len() as u64 > MAX_RECIPE {
        return Err("Photos adjustment recipe exceeds the size limit".into());
    }
    if data.len() <= MAX_ADJUSTMENT {
        return Ok(data);
    }
    let digest = format!("{:x}", Sha256::digest(&data));
    fs::create_dir_all(directory).map_err(|e| format!("Saving the Photos adjustment snapshot: {e}"))?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    let path = directory.join(format!("{digest}.json"));
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(|e| e.to_string())?;
    file.write_all(&data).and_then(|()| file.as_file().sync_all()).map_err(|e| e.to_string())?;
    match file.persist_noclobber(&path) {
        Ok(_) => (),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            if super::native::bounded_read(&path, MAX_RECIPE)? != data {
                return Err("The saved Photos adjustment snapshot is corrupt; it was not overwritten".into());
            }
        }
        Err(error) => return Err(format!("Saving the Photos adjustment snapshot: {error}")),
    }
    // No caller-supplied path is embedded or followed when this reference is reopened.
    serde_json::to_vec(&json!({"schema":2, "originalAssetId":asset, "storage":"lightcraft-local",
        "recipeSha256":digest, "recipeBytes":data.len()}))
    .map_err(|e| e.to_string())
}

pub(super) fn resolve(metadata: Value) -> Result<Value, String> {
    resolve_in(metadata, &directory()?)
}

fn resolve_in(metadata: Value, directory: &Path) -> Result<Value, String> {
    if metadata.get("schema").and_then(Value::as_u64) == Some(1) {
        original(&metadata)?;
        return Ok(metadata);
    }
    if metadata.get("schema").and_then(Value::as_u64) != Some(2) || metadata.get("storage").and_then(Value::as_str) != Some("lightcraft-local") {
        return Err("Unsupported Photos adjustment reference".into());
    }
    let asset = original(&metadata)?;
    let digest = metadata
        .get("recipeSha256")
        .and_then(Value::as_str)
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .ok_or("Invalid Photos adjustment snapshot identifier")?;
    let length = metadata
        .get("recipeBytes")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0 && *n <= MAX_RECIPE)
        .ok_or("Invalid Photos adjustment snapshot size")?;
    let data = super::native::bounded_read(&directory.join(format!("{digest}.json")), MAX_RECIPE)
        .map_err(|e| format!("The full Photos adjustment snapshot is unavailable on this Mac. Restore Photos Adjustments/Export Recipes: {e}"))?;
    if data.len() as u64 != length || format!("{:x}", Sha256::digest(&data)) != digest {
        return Err("The Photos adjustment snapshot failed its integrity check".into());
    }
    let recipe: Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    if recipe.get("schema").and_then(Value::as_u64) != Some(1) || original(&recipe)? != asset {
        return Err("The Photos adjustment snapshot belongs to a different original".into());
    }
    Ok(recipe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_masks_and_repairs_round_trip_through_a_small_immutable_reference() {
        let root = tempfile::tempdir().unwrap();
        let recipe = json!({"schema":1, "originalAssetId":"synthetic-original", "adjustments":{"mask":"a".repeat(8_000_000)}, "aiRemoveAssets":{"patch":[1,2,3]}});
        let metadata = encode_in(&recipe, root.path()).unwrap();
        assert!(metadata.len() < 2048);
        let reference: Value = serde_json::from_slice(&metadata).unwrap();
        assert_eq!(resolve_in(reference.clone(), root.path()).unwrap(), recipe);
        assert_eq!(encode_in(&recipe, root.path()).unwrap(), metadata);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        let path = root.path().join(format!("{}.json", reference["recipeSha256"].as_str().unwrap()));
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        fs::write(&path, b"corrupt").unwrap();
        assert!(resolve_in(reference.clone(), root.path()).unwrap_err().contains("integrity"));
        assert!(encode_in(&recipe, root.path()).unwrap_err().contains("not overwritten"));
        assert_eq!(fs::read(&path).unwrap(), b"corrupt");
        fs::remove_file(path).unwrap();
        assert!(resolve_in(reference, root.path()).unwrap_err().contains("unavailable on this Mac"));
    }

    #[test]
    fn small_legacy_recipes_stay_embedded_and_references_cannot_redirect_or_change_originals() {
        let root = tempfile::tempdir().unwrap();
        let recipe = json!({"schema":1, "originalAssetId":"source", "adjustments":{}});
        let data = encode_in(&recipe, root.path()).unwrap();
        assert_eq!(resolve_in(serde_json::from_slice(&data).unwrap(), root.path()).unwrap(), recipe);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        let large = json!({"schema":1, "originalAssetId":"source", "adjustments":{"mask":"x".repeat(64_000)}});
        let mut reference: Value = serde_json::from_slice(&encode_in(&large, root.path()).unwrap()).unwrap();
        reference["originalAssetId"] = json!("another");
        assert!(resolve_in(reference.clone(), root.path()).unwrap_err().contains("different original"));
        for digest in ["../outside", &"z".repeat(64), &"a".repeat(65)] {
            reference["recipeSha256"] = json!(digest);
            assert!(resolve_in(reference.clone(), root.path()).unwrap_err().contains("identifier"));
        }
        reference["recipeSha256"] = json!("a".repeat(64));
        reference["recipeBytes"] = json!(u64::MAX);
        assert!(resolve_in(reference, root.path()).unwrap_err().contains("size"));
        assert!(encode_in(&json!({"schema":1,"originalAssetId":"source","mask":"x".repeat(16_000_000)}), root.path()).is_err());
    }
}
