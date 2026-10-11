//! Preset choice is one reversible look in the edit description, not a stack of looks.
use crate::{Selection, Session};
use serde_json::json;

#[test]
fn switching_presets_uses_each_photos_own_baseline_and_supports_undo_redo() {
    let mut s = Session::with_demo();
    let ids: Vec<_> = s.visible_cloned().into_iter().take(2).collect();
    let mut bases = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        s.selection = Selection::single(*id);
        s.execute("develop.set", &json!({"control":"light.exposure", "value":0.5-index as f64})).unwrap();
        bases.push(s.develop_of(*id).unwrap());
    }
    s.selection = Selection { ids: ids.clone(), active: Some(ids[0]) };
    s.execute("preset.apply", &json!({"id":"lc.bw-high-contrast"})).unwrap();
    let first: Vec<_> = ids.iter().map(|id| s.develop_of(*id).unwrap()).collect();
    s.execute("preset.apply", &json!({"id":"lc.warm-glow"})).unwrap();
    let preset = s.presets.iter().find(|p| p.id == "lc.warm-glow").unwrap().clone();
    let second: Vec<_> = ids.iter().map(|id| s.develop_of(*id).unwrap()).collect();
    for (d, base) in second.iter().zip(&bases) {
        assert_eq!(**d, preset.select(base, 1.0));
        assert_eq!(d.treatment, base.treatment, "previous B&W look must be removed");
    }
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(ids.iter().map(|id| s.develop_of(*id).unwrap()).collect::<Vec<_>>(), first);
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(ids.iter().map(|id| s.develop_of(*id).unwrap()).collect::<Vec<_>>(), second);
    s.execute("preset.apply", &json!({"id":"lc.warm-glow", "amount":50.0})).unwrap();
    for (id, base) in ids.iter().zip(&bases) {
        assert_eq!(*s.develop_of(*id).unwrap(), preset.select(base, 0.5));
    }
}

#[test]
fn legacy_preset_chain_recovers_the_recorded_manual_baseline_without_rewriting_history() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    s.execute("develop.set", &json!({"control":"light.exposure", "value":0.8})).unwrap();
    let base = s.develop_of(id).unwrap();
    for pid in ["lc.bw-high-contrast", "lc.warm-glow"] {
        let preset = s.presets.iter().find(|p| p.id == pid).unwrap();
        let value = preset.apply(&s.develop_of(id).unwrap(), 1.0);
        s.set_develop(id, value, &format!("Preset: {}", preset.name)).unwrap();
        if pid == "lc.bw-high-contrast" {
            s.execute("develop.set", &json!({"control":"effects.texture", "value":19.0})).unwrap();
        }
    }
    let history = s.catalog.photo(id).unwrap().history.clone();
    let mut expected = (*base).clone();
    expected.effects.texture = 19.0;
    assert_eq!(*s.preset_settings_of(id).unwrap(), expected);
    assert_eq!(s.catalog.photo(id).unwrap().history, history, "baseline lookup is read-only");
    s.execute("preset.apply", &json!({"id":"lc.warm-glow"})).unwrap();
    let preset = s.presets.iter().find(|p| p.id == "lc.warm-glow").unwrap();
    assert_eq!(*s.develop_of(id).unwrap(), preset.select(&expected, 1.0));
}
