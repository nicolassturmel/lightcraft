use lightcraft_develop::{DevelopSettings, ToneCurve};
use serde_json::json;

use crate::Session;

fn settings(s: &Session) -> DevelopSettings {
    (*s.develop_of(s.active().unwrap()).unwrap()).clone()
}

#[test]
fn mask_curves_are_independent_undoable_duplicated_and_persistent() {
    let dir = std::env::temp_dir().join(format!("lc-mask-curves-{}-{}", std::process::id(), lightcraft_preview::next_tick()));
    let mut s = Session::new();
    s.open_library(&dir, true).unwrap();
    s.execute("mask.add", &json!({"kind":"linear"})).unwrap();
    s.execute("mask.adjust", &json!({"values":{"exposure":0.5}})).unwrap();
    s.execute("mask.add", &json!({"kind":"radial"})).unwrap();
    let original = settings(&s);
    s.execute("develop.beginInteraction", &json!({"label":"Mask Curve"})).unwrap();
    let undo_before = s.undo.len();
    for y in [0.55, 0.6, 0.7] {
        s.execute("mask.curve", &json!({"id":1,"channel":"red","points":[[0,0],[0.5,y],[1,1]]})).unwrap();
    }
    s.execute("develop.endInteraction", &json!({})).unwrap();
    assert_eq!(s.undo.len(), undo_before + 1);
    let edited = settings(&s);
    assert_eq!(edited.curve, original.curve, "global curve stays untouched");
    assert_eq!(edited.masks[1], original.masks[1], "other masks stay untouched");
    assert_eq!(edited.masks[0].adjust, original.masks[0].adjust);
    assert_eq!(edited.masks[0].components, original.masks[0].components);
    assert_eq!(edited.masks[0].curve.red[1].y, 0.7);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(settings(&s), original);
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(settings(&s), edited);
    s.execute("mask.duplicate", &json!({"id":1})).unwrap();
    let duplicate = s.active_mask.unwrap();
    assert_ne!(duplicate, 1);
    assert_eq!(settings(&s).masks.iter().find(|m| m.id == duplicate).unwrap().curve, edited.masks[0].curve);
    s.execute("mask.curve.reset", &json!({"id":1,"channel":"red"})).unwrap();
    assert_eq!(settings(&s).masks[0].curve, ToneCurve::default());
    let expected = settings(&s);
    let photo = s.active().unwrap();
    drop(s);
    let mut reopened = Session::new();
    reopened.open_library(&dir, false).unwrap();
    assert_eq!(*reopened.develop_of(photo).unwrap(), expected);
    drop(reopened);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn mask_curve_errors_do_not_change_settings_or_history() {
    let mut s = Session::with_demo();
    s.execute("mask.add", &json!({"kind":"linear"})).unwrap();
    let original = settings(&s);
    let undo = s.undo.len();
    let cases = [
        json!({}),
        json!({"channel":"yellow","points":[[0,0],[1,1]]}),
        json!({"points":[[0,0]]}),
        json!({"points":[[0,0],[0,1]]}),
        json!({"points":[[0,null],[1,1]]}),
        json!({"points":[[0,0,1],[1,1]]}),
        json!({"id":4_294_967_297u64,"points":[]}),
        json!({"id":"1","points":[]}),
        json!({"points":vec![[0.0,0.0];65]}),
        json!({"refineSaturation":"oops"}),
    ];
    for params in cases {
        assert!(s.execute("mask.curve", &params).is_err(), "{params}");
        assert_eq!(settings(&s), original);
        assert_eq!(s.undo.len(), undo);
    }
}
