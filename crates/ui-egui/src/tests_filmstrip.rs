//! Headless test of the filmstrip's file-name labels (#266): a name with multi-byte (CJK)
//! characters is drawn without panicking.

use std::time::Duration;

use lightcraft_catalog::Op;
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

#[test]
fn filmstrip_shows_a_cjk_file_name() {
    let mut session = lightcraft_engine::Session::with_demo();
    let first = session.visible_cloned()[0];
    let source = session.catalog.photo(first).expect("demo photo").source.clone();
    // the name from #266: cutting it at byte 13 lands inside '限'
    session.catalog.apply(Op::SetFile { id: first, file_name: "202407層三限定訂閱圖(4).jpg".into(), source }).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.settings.film_names);
    let film = format!("film:{}", first.0);
    assert!(h.app.widgets.iter().any(|(w, _)| *w == film), "{film} drawn in the filmstrip");
}

#[test]
fn filmstrip_filters_ratings_flags_and_exports_without_changing_photo_annotations() {
    use lightcraft_catalog::Flag;
    let mut session = lightcraft_engine::Session::with_demo();
    let ids: Vec<_> = session.visible_cloned().into_iter().take(2).collect();
    for (id, rating, flag) in [(ids[0], 5, Flag::Pick), (ids[1], 2, Flag::Reject)] {
        session.catalog.apply(Op::SetRating { id, rating }).unwrap();
        session.catalog.apply(Op::SetFlag { id, flag }).unwrap();
    }
    session.catalog.apply(Op::SetExported { id: ids[0], exported: Some("2026-11-03T10:00:00Z".into()) }).unwrap();
    session.filter.only = ids.clone();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    h.request("ui.set", json!({"view":"detail","filmstrip":true}), T);
    h.settle(SETTLE);
    assert!(h.app.widgets.iter().any(|(id, _)| id == &format!("badge:exported:{}", ids[0].0)));
    for widget in ["filter:star5", "filter:pick"] {
        let reply = h.request("ui.clickWidget", json!({"id":widget}), T);
        assert_eq!(reply["ok"], true, "{reply}");
        h.step();
    }
    assert_eq!(h.app.session.visible_cloned(), vec![ids[0]]);
    h.request("ui.clickWidget", json!({"id":"filter:reject"}), T);
    h.step();
    assert!(h.app.session.visible_cloned().is_empty());
    assert!(h.app.widgets.iter().any(|(id, _)| id == "button:filmFilterClear"), "empty results must keep their filters reachable");
    h.request("ui.clickWidget", json!({"id":"button:filmFilterClear"}), T);
    h.step();
    assert_eq!(h.app.session.filter, Default::default());
    h.request("ui.clickWidget", json!({"id":"filter:exported"}), T);
    h.step();
    assert_eq!(h.app.session.visible_cloned(), vec![ids[0]]);
    assert_eq!(h.app.session.catalog.photo(ids[0]).unwrap().rating, 5);
    assert_eq!(h.app.session.catalog.photo(ids[0]).unwrap().flag, Flag::Pick);
    assert_eq!(h.app.session.catalog.photo(ids[1]).unwrap().rating, 2);
    assert_eq!(h.app.session.catalog.photo(ids[1]).unwrap().flag, Flag::Reject);
}
