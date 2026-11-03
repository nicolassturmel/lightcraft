//! Durable export bookkeeping. Rendering, failed writes and skipped files never mark a photo.

use std::collections::{BTreeMap, BTreeSet};

use lightcraft_catalog::{Op, PhotoId, Source};
use serde_json::Value;

use crate::Session;

/// The identities at preparation time, so a late export cannot tag a different library/photo.
#[derive(Clone)]
pub struct ExportTracking {
    generation: u64,
    sources: BTreeMap<PhotoId, Source>,
}

impl ExportTracking {
    pub fn new(session: &Session, ids: impl IntoIterator<Item = PhotoId>) -> Self {
        Self {
            generation: session.library_generation(),
            sources: ids.into_iter().filter_map(|id| session.catalog.photo(id).map(|p| (id, p.source.clone()))).collect(),
        }
    }

    pub fn record_files(&self, session: &mut Session, files: &[Value]) -> crate::Result<()> {
        let ids = files
            .iter()
            .filter(|f| f["path"].is_string() && f.get("error").is_none() && f.get("skipped").is_none())
            .filter_map(|f| f["photo"].as_u64().map(PhotoId));
        self.record(session, ids)
    }

    /// Used after PhotoKit confirms the edited copy was created.
    pub fn record_all(&self, session: &mut Session) -> crate::Result<()> {
        self.record(session, self.sources.keys().copied())
    }

    fn record(&self, session: &mut Session, ids: impl IntoIterator<Item = PhotoId>) -> crate::Result<()> {
        if session.library_generation() != self.generation {
            return Ok(());
        }
        let when = (session.clock)();
        let ops: Vec<_> = ids
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|id| self.sources.get(id).is_some_and(|source| session.catalog.photo(*id).is_some_and(|p| &p.source == source)))
            .map(|id| Op::SetExported { id, exported: Some(when.clone()) })
            .collect();
        if !ops.is_empty() {
            session.apply_system(Op::Batch { ops })?;
            session.persist()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_successful_writes_are_tagged_and_undo_keeps_receipts() {
        let mut s = Session::with_demo();
        let ids: Vec<_> = s.visible_cloned().into_iter().take(3).collect();
        s.execute("photo.rate", &json!({"ids":[ids[0].0],"rating":4})).unwrap();
        let undo = s.undo.len();
        let tracker = ExportTracking::new(&s, ids.iter().copied());
        tracker
            .record_files(
                &mut s,
                &[
                    json!({"photo":ids[0].0,"path":"ok.jpg"}),
                    json!({"photo":ids[1].0,"error":"disk full"}),
                    json!({"photo":ids[2].0,"skipped":"exists.jpg"}),
                    json!({"photo":99999,"path":"unrelated.jpg"}),
                ],
            )
            .unwrap();
        assert_eq!(s.undo.len(), undo);
        assert!(s.catalog.photo(ids[0]).unwrap().exported.is_some());
        assert!(ids[1..].iter().all(|id| s.catalog.photo(*id).unwrap().exported.is_none()));
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(s.catalog.photo(ids[0]).unwrap().exported.is_some());
        s.execute("library.filter", &json!({"exported":true})).unwrap();
        assert_eq!(s.visible_cloned(), vec![ids[0]]);
        s.execute("library.filter", &json!({"exported":false})).unwrap();
        assert!(!s.visible_cloned().contains(&ids[0]));
    }

    #[test]
    fn late_completion_cannot_tag_a_replaced_source_or_library() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        let tracker = ExportTracking::new(&s, [id]);
        s.catalog.apply(Op::SetFile { id, file_name: "changed.jpg".into(), source: Source::File { path: "changed.jpg".into() } }).unwrap();
        tracker.record_all(&mut s).unwrap();
        assert!(s.catalog.photo(id).unwrap().exported.is_none());
        let mut other = Session::with_demo();
        tracker.record_all(&mut other).unwrap();
        assert!(other.catalog.photos().all(|p| p.exported.is_none()));
    }
}
