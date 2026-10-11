//! Presets and copy/paste subsets.
//!
//! A preset is a *partial* settings object (JSON) — only the groups it includes. Applying merges it
//! into the photo's settings; `amount` (0..200 %) interpolates numeric values between the current
//! settings and the preset's. Copy/paste/sync use the same group selection ([`SettingsGroup`]).
//!
//! The rendering process ([`crate::ProcessVersion`]) is in no group: copy, paste, sync and presets
//! made from a photo never carry it, so the photos they change keep their own. A partial that names
//! `process` itself (Apply Settings JSON) sets it like any other field.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::DevelopSettings;

/// The selected look, kept in the edit recipe. Only values changed by the preset are recorded;
/// pixels and a second complete copy of the settings are never stored here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetSelection {
    pub id: String,
    pub amount: f64,
    pub before: Value,
    pub applied: Value,
}

fn changed_values(before: &Value, after: &Value, depth: usize) -> (Value, Value) {
    if depth < 32
        && let (Some(b), Some(a)) = (before.as_object(), after.as_object())
    {
        let mut old = Map::new();
        let mut new = Map::new();
        for (key, value) in a {
            let previous = b.get(key).unwrap_or(&Value::Null);
            if value != previous {
                let (left, right) = changed_values(previous, value, depth + 1);
                old.insert(key.clone(), left);
                new.insert(key.clone(), right);
            }
        }
        (Value::Object(old), Value::Object(new))
    } else {
        (before.clone(), after.clone())
    }
}

fn restore_values(current: &mut Value, before: &Value, applied: &Value, depth: usize) {
    if depth < 32
        && let (Some(c), Some(b), Some(a)) = (current.as_object_mut(), before.as_object(), applied.as_object())
    {
        for (key, value) in a {
            if let (Some(now), Some(old)) = (c.get_mut(key), b.get(key)) {
                if key == "masks" {
                    restore_masks(now, old, value);
                } else {
                    restore_values(now, old, value, depth + 1);
                }
            }
        }
    } else if current == applied {
        *current = before.clone();
    }
}

fn restore_masks(current: &mut Value, before: &Value, applied: &Value) {
    let (Some(c), Some(b), Some(a)) = (current.as_array_mut(), before.as_array(), applied.as_array()) else { return };
    // Remove the previous preset's own masks, preserving added or manually edited components.
    c.retain_mut(|mask| {
        let id = mask.get("id");
        if a.iter().any(|m| m.get("id") == id && m == mask) {
            if let Some(old) = b.iter().find(|m| m.get("id") == id) {
                *mask = old.clone();
            } else {
                return false;
            }
        }
        true
    });
}

impl DevelopSettings {
    /// Remove the previous selected look, retaining subsequent manual changes.
    pub fn preset_base(&self) -> Self {
        let mut base = self.clone();
        let Some(selection) = base.preset_selection.take() else { return base };
        let mut value = base.to_json();
        restore_values(&mut value, &selection.before, &selection.applied, 0);
        Self::from_json(&value).unwrap_or(base)
    }
}

/// Groups shown in "Choose settings to copy" / "Create preset" (Lightroom's checklist).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingsGroup {
    Profile,
    Treatment,
    WhiteBalance,
    Light,
    ToneCurve,
    Color,
    ColorMixer,
    ColorGrading,
    Effects,
    Vignette,
    Grain,
    Detail,
    Optics,
    Geometry,
    Crop,
    Masks,
    Spots,
    RedEye,
    LensBlur,
    Calibration,
}

impl SettingsGroup {
    pub const ALL: [SettingsGroup; 20] = [
        SettingsGroup::Profile,
        SettingsGroup::Treatment,
        SettingsGroup::WhiteBalance,
        SettingsGroup::Light,
        SettingsGroup::ToneCurve,
        SettingsGroup::Color,
        SettingsGroup::ColorMixer,
        SettingsGroup::ColorGrading,
        SettingsGroup::Effects,
        SettingsGroup::Vignette,
        SettingsGroup::Grain,
        SettingsGroup::Detail,
        SettingsGroup::Optics,
        SettingsGroup::Geometry,
        SettingsGroup::Crop,
        SettingsGroup::Masks,
        SettingsGroup::Spots,
        SettingsGroup::RedEye,
        SettingsGroup::LensBlur,
        SettingsGroup::Calibration,
    ];

    /// Groups included by default when copying (Lightroom excludes crop, masks and spots by default).
    pub fn default_copy() -> Vec<SettingsGroup> {
        SettingsGroup::ALL
            .into_iter()
            .filter(|g| !matches!(g, SettingsGroup::Crop | SettingsGroup::Masks | SettingsGroup::Spots | SettingsGroup::RedEye))
            .collect()
    }

    pub fn label(self) -> &'static str {
        match self {
            SettingsGroup::Profile => "Profile",
            SettingsGroup::Treatment => "Treatment",
            SettingsGroup::WhiteBalance => "White Balance",
            SettingsGroup::Light => "Light",
            SettingsGroup::ToneCurve => "Tone Curve",
            SettingsGroup::Color => "Color",
            SettingsGroup::ColorMixer => "Color Mixer",
            SettingsGroup::ColorGrading => "Color Grading",
            SettingsGroup::Effects => "Effects",
            SettingsGroup::Vignette => "Vignette",
            SettingsGroup::Grain => "Grain",
            SettingsGroup::Detail => "Detail",
            SettingsGroup::Optics => "Optics",
            SettingsGroup::Geometry => "Geometry",
            SettingsGroup::Crop => "Crop",
            SettingsGroup::Masks => "Masking",
            SettingsGroup::Spots => "Remove",
            SettingsGroup::RedEye => "Red Eye",
            SettingsGroup::LensBlur => "Lens Blur",
            SettingsGroup::Calibration => "Calibration",
        }
    }

    /// Top-level JSON keys of `DevelopSettings` that belong to the group.
    pub fn keys(self) -> &'static [&'static str] {
        match self {
            SettingsGroup::Profile => &["profile"],
            SettingsGroup::Treatment => &["treatment"],
            SettingsGroup::WhiteBalance => &["wb"],
            SettingsGroup::Light => &["light", "hdr"],
            SettingsGroup::ToneCurve => &["curve"],
            SettingsGroup::Color => &["color"],
            SettingsGroup::ColorMixer => &["mixer", "bw_mix", "point_colors"],
            SettingsGroup::ColorGrading => &["grading"],
            SettingsGroup::Effects => &["effects"],
            SettingsGroup::Vignette => &["vignette"],
            SettingsGroup::Grain => &["grain"],
            SettingsGroup::Detail => &["detail", "enhance"],
            SettingsGroup::Optics => &["optics"],
            SettingsGroup::Geometry => &["geometry"],
            SettingsGroup::Crop => &["crop", "orientation"],
            SettingsGroup::Masks => &["masks"],
            SettingsGroup::Spots => &["spots"],
            SettingsGroup::RedEye => &["red_eye"],
            SettingsGroup::LensBlur => &["lens_blur"],
            SettingsGroup::Calibration => &["calibration"],
        }
    }
}

/// The subset of `s` covering `groups`, as a partial JSON object.
pub fn extract_groups(s: &DevelopSettings, groups: &[SettingsGroup]) -> Value {
    let full = s.to_json();
    let mut out = Map::new();
    for g in groups {
        for k in g.keys() {
            if let Some(v) = full.get(*k) {
                out.insert((*k).to_string(), v.clone());
            }
        }
    }
    Value::Object(out)
}

/// Recursively merge `patch` into `base` (objects merge; everything else replaces).
pub fn deep_merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(bv) if bv.is_object() && v.is_object() => deep_merge(bv, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

/// Interpolate numbers in `patch` towards/away from `base` by `t` (1 = patch as-is); non-numeric
/// values are taken from the patch when `t >= 0.5`.
fn scale_patch(base: &Value, patch: &Value, t: f64) -> Value {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            let mut out = Map::new();
            for (k, pv) in p {
                let bv = b.get(k).unwrap_or(&Value::Null);
                out.insert(k.clone(), scale_patch(bv, pv, t));
            }
            Value::Object(out)
        }
        // Integer fields (the grain seed, ids) are discrete like strings: an interpolated seed would
        // not deserialize as an integer, and the whole preset would silently not apply.
        (Value::Number(b), Value::Number(p)) if b.is_f64() || p.is_f64() => {
            let (b, p) = (b.as_f64().unwrap_or(0.0), p.as_f64().unwrap_or(0.0));
            serde_json::Number::from_f64(b + (p - b) * t).map(Value::Number).unwrap_or(Value::Null)
        }
        (b, p) => {
            if t >= 0.5 {
                p.clone()
            } else {
                b.clone()
            }
        }
    }
}

/// Apply a partial settings object with an amount (1.0 = 100 %). Out-of-range values are clamped
/// through the control specs.
pub fn apply_partial(s: &DevelopSettings, partial: &Value, amount: f64) -> DevelopSettings {
    let base = s.to_json();
    let patch = if (amount - 1.0).abs() < 1e-9 { partial.clone() } else { scale_patch(&base, partial, amount) };
    let mut out = s.merged(&patch).unwrap_or_else(|_| s.clone());
    for c in crate::controls::CONTROLS {
        if let Some(v) = crate::controls::get(&out, c.id) {
            crate::controls::set(&mut out, c.id, v);
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub group: String,
    /// Partial settings.
    pub settings: Value,
    #[serde(default)]
    pub favorite: bool,
    /// Built-in presets ship with the app and can't be deleted.
    #[serde(default)]
    pub builtin: bool,
}

impl Preset {
    /// Select a look instead of stacking it on the last one. The description survives save,
    /// reload, undo and redo, and also provides the baseline for hover previews and Amount.
    pub fn select(&self, settings: &DevelopSettings, amount: f64) -> DevelopSettings {
        let base = settings.preset_base();
        let mut out = self.apply(&base, amount);
        out.preset_selection = None;
        let (before, applied) = changed_values(&base.to_json(), &out.to_json(), 0);
        out.preset_selection = Some(PresetSelection { id: self.id.clone(), amount: amount * 100.0, before, applied });
        out
    }

    pub fn from_settings(id: &str, name: &str, group: &str, s: &DevelopSettings, groups: &[SettingsGroup]) -> Preset {
        Preset { id: id.into(), name: name.into(), group: group.into(), settings: extract_groups(s, groups), favorite: false, builtin: false }
    }
    /// Apply at `amount` (1.0 = 100 %). Masks in the preset are added to the photo's own (ones it
    /// already has are skipped), their effect scaled by the amount.
    pub fn apply(&self, s: &DevelopSettings, amount: f64) -> DevelopSettings {
        let Some(masks) = self.settings.get("masks").and_then(Value::as_array).filter(|m| !m.is_empty()) else {
            return apply_partial(s, &self.settings, amount);
        };
        let mut rest = self.settings.clone();
        if let Some(o) = rest.as_object_mut() {
            o.remove("masks");
        }
        let mut out = apply_partial(s, &rest, amount);
        let mut next = out.masks.iter().map(|m| m.id).max().unwrap_or(0);
        for m in masks {
            let Ok(mut m) = serde_json::from_value::<crate::Mask>(m.clone()) else { continue };
            if out.masks.iter().any(|q| q.name == m.name && q.components == m.components) {
                continue;
            }
            next += 1;
            m.id = next;
            m.adjust.amount = (m.adjust.amount * amount).clamp(0.0, 200.0);
            out.masks.push(m);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn look(id: &str, settings: Value) -> Preset {
        Preset { id: id.into(), name: id.into(), group: "test".into(), settings, favorite: false, builtin: false }
    }

    #[test]
    fn choosing_another_look_replaces_the_previous_one_after_serialization() {
        let mut base = DevelopSettings::default();
        base.light.exposure = 0.7;
        base.crop.geometry.angle = 3.0;
        let a = look("bw", json!({"treatment":"bw", "effects":{"dehaze":40.0}, "light":{"exposure":2.0}}));
        let b = look("colour", json!({"color":{"saturation":12.0}}));
        let first = a.select(&base, 1.0);
        let restored = DevelopSettings::from_json(&first.to_json()).unwrap();
        assert_eq!(b.select(&restored, 1.0), b.select(&base, 1.0));
        assert_eq!(a.select(&first, 1.0), first, "reselecting is idempotent");
        assert_eq!(a.select(&first, 0.5), a.select(&base, 0.5), "Amount uses the same baseline");
        assert_eq!(a.select(&first, 0.0).preset_base(), base);
        assert!(first.preset_selection.as_ref().unwrap().before.get("crop").is_none(), "the recipe stores only changed fields");
    }

    #[test]
    fn switching_looks_preserves_manual_settings_and_masks() {
        let mut base = DevelopSettings::default();
        let own = crate::Mask { id: 1, name: "Own mask".into(), ..Default::default() };
        base.masks.push(own.clone());
        let preset_mask = crate::Mask { id: 8, name: "Preset mask".into(), ..Default::default() };
        let a = look("a", json!({"effects":{"dehaze":40.0}, "masks":[preset_mask]}));
        let b = look("b", json!({"color":{"saturation":12.0}}));
        let mut current = a.select(&base, 1.0);
        current.effects.dehaze = 17.0;
        current.light.exposure = 1.2;
        current.masks.push(crate::Mask { id: 3, name: "New manual mask".into(), ..Default::default() });
        let result = b.select(&current, 1.0);
        assert_eq!(result.effects.dehaze, 17.0);
        assert_eq!(result.light.exposure, 1.2);
        assert_eq!(result.masks.len(), 2);
        assert_eq!(result.masks[0], own);
        assert_eq!(result.masks[1].name, "New manual mask");
    }

    #[test]
    fn extract_and_apply_roundtrip() {
        let mut a = DevelopSettings::default();
        a.light.exposure = 1.0;
        a.effects.clarity = 30.0;
        a.crop.geometry.angle = 5.0;
        let p = Preset::from_settings("p", "P", "User", &a, &SettingsGroup::default_copy());
        assert!(p.settings.get("crop").is_none());
        let b = p.apply(&DevelopSettings::default(), 1.0);
        assert_eq!(b.light.exposure, 1.0);
        assert_eq!(b.effects.clarity, 30.0);
        assert_eq!(b.crop.geometry.angle, 0.0);
    }

    #[test]
    fn amount_scales_numbers() {
        let base = DevelopSettings::default();
        let partial = json!({"light": {"exposure": 1.0, "contrast": 40}});
        let half = apply_partial(&base, &partial, 0.5);
        assert!((half.light.exposure - 0.5).abs() < 1e-9);
        assert!((half.light.contrast - 20.0).abs() < 1e-9);
        let more = apply_partial(&base, &partial, 2.0);
        assert!((more.light.contrast - 80.0).abs() < 1e-9);
        // clamped through specs
        let lots = apply_partial(&base, &json!({"light": {"exposure": 4.0}}), 2.0);
        assert_eq!(lots.light.exposure, 5.0);
    }

    /// A preset with the Grain group (its seed is an integer) used to do nothing at any amount
    /// other than 100 %: the interpolated seed failed to deserialize and the preset was dropped.
    #[test]
    fn amount_keeps_integer_fields_whole() {
        let mut a = DevelopSettings::default();
        a.light.exposure = 1.0;
        a.grain.amount = 40.0;
        a.grain.seed = 12345;
        let p = Preset::from_settings("p", "P", "User", &a, &SettingsGroup::default_copy());
        assert!(p.settings.pointer("/grain/seed").is_some_and(Value::is_u64), "the default copy groups include the grain seed");
        let base = DevelopSettings::default();
        for (amount, seed) in [(0.3, 0), (0.5, 12345), (1.5, 12345), (2.0, 12345)] {
            let out = p.apply(&base, amount);
            assert!((out.light.exposure - amount.min(5.0)).abs() < 1e-9, "{amount}: exposure {}", out.light.exposure);
            assert!((out.grain.amount - (40.0 * amount).min(100.0)).abs() < 1e-9, "{amount}: grain {}", out.grain.amount);
            assert_eq!(out.grain.seed, seed, "{amount}: seed");
        }
    }

    #[test]
    fn deep_merge_objects() {
        let mut a = json!({"x": {"a": 1, "b": 2}, "y": [1]});
        deep_merge(&mut a, &json!({"x": {"b": 3}, "y": [2, 3]}));
        assert_eq!(a, json!({"x": {"a": 1, "b": 3}, "y": [2, 3]}));
    }
}
