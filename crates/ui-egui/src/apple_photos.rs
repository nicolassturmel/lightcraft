//! The main Apple Photos library, edit-copy saves and native mask jobs.
use crate::LightcraftApp;
use lightcraft_engine::catalog::PhotoId;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct Browser {
    pub open: bool,
    pub error: Option<String>,
    pub last_copy: Option<Value>,
    pub download_errors: BTreeMap<PhotoId, String>,
    attempted: bool,
    focused: bool,
    pub(crate) native_focused: bool,
    refreshed_at: f64,
    refresh_requested: bool,
    pending_refresh: Option<(u64, lightcraft_engine::apple::LoadedLibrary)>,
    refreshes_started: u64,
}

impl Browser {
    pub(crate) fn refresh_status(&self) -> Value {
        json!({"started":self.refreshes_started,"requested":self.refresh_requested,
            "waitingForGesture":self.pending_refresh.is_some(),"lastCompletedAt":self.refreshed_at})
    }
}

const READ: &str = "Reading Apple Photos";
const SAVE: &str = "Saving edited copy to Photos";
const IMPORT: &str = "Importing into Apple Photos";
const RECIPE: &str = "Reading Photos adjustments";
const MASK: &str = "Selecting with Apple Vision";

pub fn intercept(app: &mut LightcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    match id {
        "library.applePhotos" => Some(start_refresh(app)),
        "photo.saveApplePhotosCopy" => Some(start_copy(app, params)),
        "photo.downloadApplePhotosOriginal" => Some(start_download(app, params)),
        "photo.reopenApplePhotosRecipe" => Some(start_recipe(app)),
        "library.importIntoApplePhotos" => Some(start_import(app, params)),
        "mask.add" | "mask.addComponent" if lightcraft_engine::apple::wants_vision(&app.session, params) => Some(start_mask(app, id, params)),
        "mask.objectPoint" if lightcraft_engine::apple::uses_vision(&app.session) => Some(start_object_click(app, params)),
        "mask.reselectSky" if lightcraft_engine::apple::uses_vision(&app.session) => Some(start_sky_update(app, params)),
        _ => None,
    }
}

pub fn ui_allowed(app: &LightcraftApp, id: &str) -> Result<(), String> {
    lightcraft_engine::apple::command_allowed(&app.session, id)?;
    if app.session.apple_library.is_some()
        && matches!(
            id,
            "app.openLibrary"
                | "file.addFolder"
                | "file.addFromDevice"
                | "file.importLightroom"
                | "file.findMissing"
                | "file.backupLibrary"
                | "file.restoreLibrary"
                | "dialog.newAlbum"
                | "dialog.newSmartAlbum"
                | "dialog.newFolder"
                | "dialog.rename"
                | "dialog.captureTime"
        )
    {
        return Err("LightCraft uses the System Photo Library. Manage albums and original files in Photos.".into());
    }
    Ok(())
}

pub fn open(app: &mut LightcraftApp) -> Result<Value, String> {
    lightcraft_engine::apple::primary(&app.session)?;
    app.apple_photos.open = true;
    if !app.tasks.is_running(READ) { start_refresh(app) } else { Ok(json!({"pending":true})) }
}

fn start_refresh(app: &mut LightcraftApp) -> Result<Value, String> {
    lightcraft_engine::apple::primary(&app.session)?;
    if app.tasks.is_running(READ) || app.apple_photos.pending_refresh.is_some() {
        return Err("Apple Photos is already being read".into());
    }
    if app.session.interaction.is_some() {
        return Err("Finish the current adjustment before refreshing Photos".into());
    }
    let directory = app.session.apple_library.as_ref().and_then(|l| l.directory.clone());
    let generation = app.session.library_generation();
    app.apple_photos.attempted = true;
    app.apple_photos.refresh_requested = false;
    app.apple_photos.error = None;
    crate::tasks::spawn(
        app,
        READ,
        Some("applePhotos"),
        move || lightcraft_engine::apple::load_snapshot(directory.as_deref()),
        move |app, ctx, result| {
            if generation != app.session.library_generation() || app.session.apple_library.is_none() {
                return;
            }
            finish_refresh(app, ctx, result);
        },
    )?;
    app.apple_photos.refreshes_started = app.apple_photos.refreshes_started.saturating_add(1);
    Ok(json!({"pending":true, "backend":"applePhotos"}))
}

fn finish_refresh(app: &mut LightcraftApp, ctx: &egui::Context, result: Result<lightcraft_engine::apple::LoadedLibrary, String>) {
    app.apple_photos.refreshed_at = app.last_time;
    let result = result.and_then(|loaded| {
        if app.session.interaction.is_some() {
            // Keep this completed response until the gesture ends; never rescan the library
            // just because a slider was held while PhotoKit answered.
            app.apple_photos.pending_refresh = Some((app.session.library_generation(), loaded));
            Ok(json!({"pending":true}))
        } else {
            app.session.apply_photos_snapshot(loaded)
        }
    });
    match result {
        Ok(report) => {
            if let Some(warnings) = report.get("warnings").and_then(Value::as_array) {
                for warning in warnings.iter().filter_map(Value::as_str) {
                    app.notices.push(warning.into());
                }
            }
            app.apple_photos.error = None;
        }
        Err(error) => {
            if let Some(state) = &mut app.session.apple_library {
                state.error = Some(error.clone());
            }
            app.apple_photos.error = Some(error.clone());
            app.toast_error(ctx, error);
        }
    }
}

fn start_copy(app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    if app.tasks.is_running(SAVE) {
        return Err("A Photos copy is already being saved".into());
    }
    let job = lightcraft_engine::apple::prepare_copy(&mut app.session, params)?;
    crate::tasks::spawn(
        app,
        SAVE,
        Some("applePhotosCopy"),
        move || job.run(),
        |app, ctx, result| match result {
            Ok(saved) => {
                if let Some(warning) = saved.get("warning").and_then(Value::as_str) {
                    app.toast_error(ctx, warning);
                } else {
                    app.toast(ctx, "Edited copy saved in Photos > LightCraft. Original unchanged.");
                }
                app.apple_photos.last_copy = Some(saved);
                app.apple_photos.refresh_requested = true;
            }
            Err(error) => app.toast_error(ctx, error),
        },
    )?;
    Ok(json!({"pending":true}))
}

fn download_label(id: PhotoId) -> String {
    format!("Downloading Photos original {}", id.0)
}

fn start_download(app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    let job = lightcraft_engine::apple::prepare_download(&app.session, params)?;
    let label = download_label(job.id);
    if app.tasks.is_running(&label) {
        return Err("This photo's original is already being downloaded".into());
    }
    let id = job.id;
    let generation = app.session.library_generation();
    app.apple_photos.download_errors.remove(&id);
    crate::tasks::spawn(
        app,
        &label,
        Some("applePhotosDownload"),
        move || {
            let result = job.run();
            (job, result)
        },
        move |app, ctx, (job, result)| {
            if generation != app.session.library_generation() || app.session.apple_library.is_none() {
                return;
            }
            match result.and_then(|downloaded| lightcraft_engine::apple::accept_download(&mut app.session, &job, downloaded)) {
                Ok(_) => {
                    app.renderer.retry_photo(id);
                    app.apple_photos.download_errors.remove(&id);
                    app.toast(ctx, "Original ready to edit");
                }
                Err(error) => {
                    app.apple_photos.download_errors.insert(id, error.clone());
                    app.toast_error(ctx, error);
                }
            }
        },
    )?;
    Ok(json!({"pending":true, "id":id.0}))
}

fn download_button(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId, rect: Option<egui::Rect>, key: &str) {
    if !app.session.catalog.photo(id).is_some_and(|p| matches!(p.source, lightcraft_engine::catalog::Source::ApplePhotos { .. })) {
        return;
    }
    let running = app.tasks.is_running(&download_label(id));
    let error = app.apple_photos.download_errors.get(&id).cloned();
    let ready = error.is_none() && app.session.media.has_source(id, lightcraft_engine::media::SourceLevel::Full);
    let label = if running {
        "Downloading…"
    } else if ready {
        "Original ready"
    } else if error.is_some() {
        "Retry download"
    } else {
        "Download original"
    };
    let label = if rect.is_some_and(|r| r.width() < 130.0) && !running && !ready { "Download" } else { label };
    let button = egui::Button::new(egui::RichText::new(crate::i18n::tr(label)).size(11.5));
    let response = ui
        .add_enabled_ui(!running && !ready, |ui| match rect {
            Some(rect) => ui.put(rect, button),
            None => ui.add(button),
        })
        .inner
        .on_hover_text(
            error.unwrap_or_else(|| "Load only this original from Photos, using iCloud if needed. Other photos will not be downloaded.".into()),
        );
    crate::widgets::register(ui.ctx(), format!("button:photosDownload:{key}:{}", id.0), response.rect);
    if response.clicked()
        && let Err(error) = app.run("photo.downloadApplePhotosOriginal", json!({"id":id.0}))
    {
        app.toast_error(ui.ctx(), error);
    }
}

pub fn grid_download_button(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId, image: egui::Rect) {
    let rect =
        egui::Rect::from_center_size(egui::pos2(image.center().x, image.bottom() - 38.0), egui::vec2((image.width() - 12.0).clamp(1.0, 170.0), 24.0));
    download_button(app, ui, id, Some(rect), "grid");
}

fn start_recipe(app: &mut LightcraftApp) -> Result<Value, String> {
    lightcraft_engine::apple::has_photo(&app.session)?;
    if app.tasks.is_running(RECIPE) {
        return Err("A Photos recipe is already being read".into());
    }
    let photo = app.session.active().and_then(|id| app.session.catalog.photo(id)).ok_or("select a Photos copy")?;
    let lightcraft_engine::catalog::Source::ApplePhotos { asset, .. } = &photo.source else {
        return Err("select a Photos copy".into());
    };
    let asset = asset.clone();
    let generation = app.session.library_generation();
    crate::tasks::spawn(
        app,
        RECIPE,
        Some("applePhotos"),
        move || lightcraft_engine::apple::load_recipe(&asset, false),
        move |app, ctx, result| {
            if generation != app.session.library_generation() || app.session.apple_library.is_none() {
                return;
            }
            let result = result.and_then(|recipe| {
                app.session
                    .execute_fn("photo.reopenApplePhotosRecipe", |s| lightcraft_engine::apple::apply_recipe(s, &recipe))
                    .map_err(|e| e.to_string())
            });
            match result {
                Ok(_) => {
                    app.ui.view = crate::state::ViewMode::Detail;
                    app.toast(ctx, "Original reopened with the saved LightCraft adjustments");
                }
                Err(error) => app.toast_error(ctx, error),
            }
        },
    )?;
    Ok(json!({"pending":true}))
}

fn start_import(app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    lightcraft_engine::apple::primary(&app.session)?;
    if app.tasks.is_running(IMPORT) {
        return Err("A Photos import is already running".into());
    }
    let paths = lightcraft_engine::apple::import_paths(params)?;
    crate::tasks::spawn(
        app,
        IMPORT,
        Some("applePhotos"),
        move || lightcraft_engine::apple::import_into_photos(&paths),
        |app, ctx, result| match result {
            Ok(_) => {
                app.toast(ctx, "Images imported into Apple Photos");
                app.apple_photos.refresh_requested = true;
            }
            Err(error) => app.toast_error(ctx, error),
        },
    )?;
    Ok(json!({"pending":true}))
}

/// Load at launch, reconcile on a real return to the app, or after an explicit Photos operation.
/// No periodic full-library scan: an unchanged focused session stays idle.
pub fn tick(app: &mut LightcraftApp, ctx: &egui::Context) {
    if app.session.apple_library.is_none() {
        return;
    }
    if app.session.interaction.is_none()
        && let Some((generation, loaded)) = app.apple_photos.pending_refresh.take()
        && generation == app.session.library_generation()
    {
        finish_refresh(app, ctx, Ok(loaded));
    }
    // Browser canvas focus is virtual editing focus, not a return from the Photos app.
    let focused = app.apple_photos.native_focused;
    let loaded = app.session.apple_library.as_ref().is_some_and(|l| l.loaded);
    let elapsed = app.last_time - app.apple_photos.refreshed_at;
    app.apple_photos.refresh_requested |=
        !app.apple_photos.attempted || (loaded && app.apple_photos.error.is_none() && focused && !app.apple_photos.focused && elapsed > 5.0);
    app.apple_photos.focused = focused;
    if app.apple_photos.refresh_requested
        && !app.tasks.is_running(READ)
        && app.apple_photos.pending_refresh.is_none()
        && app.session.interaction.is_none()
        && let Err(error) = start_refresh(app)
    {
        app.apple_photos.error = Some(error);
    }
}

/// Editing waits for an explicit original request. Browsing previews never enter development.
pub fn original_needed(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId, rect: egui::Rect) -> bool {
    if !app.session.catalog.photo(id).is_some_and(|p| matches!(p.source, lightcraft_engine::catalog::Source::ApplePhotos { .. }))
        || app.session.media.has_source(id, lightcraft_engine::media::SourceLevel::Preview)
        || app.session.media.has_source(id, lightcraft_engine::media::SourceLevel::Full)
    {
        return false;
    }
    crate::panels::empty_message(
        ui,
        rect,
        "Original required",
        "Use Download original to edit this photo. Selecting it never retrieves the original.",
    );
    let button = egui::Rect::from_center_size(rect.center() + egui::vec2(0.0, 68.0), egui::vec2(190.0, 30.0));
    download_button(app, ui, id, Some(button), "detail");
    true
}

pub(crate) fn empty_library_message(app: &LightcraftApp) -> Option<(&'static str, &'static str)> {
    let library = app.session.apple_library.as_ref()?;
    Some(if app.apple_photos.error.is_some() {
        ("Apple Photos unavailable", "Review the message in the sidebar, then use Refresh Photos.")
    } else if !library.loaded {
        ("Connecting to Apple Photos", "Allow LightCraft access when macOS asks. Your photos and albums will appear here.")
    } else {
        ("No photos in this view", "Select another album or add images in Photos, then use Refresh Photos.")
    })
}

fn start_mask(app: &mut LightcraftApp, command: &str, params: &Value) -> Result<Value, String> {
    if app.tasks.is_running(MASK) {
        return Err("Apple Vision is selecting. Wait for the selection to finish.".into());
    }
    let source = lightcraft_engine::apple::vision_source(&mut app.session)?;
    let photo = app.session.active().ok_or("no active photo")?;
    let generation = app.session.library_generation();
    let content = app.session.catalog.photo(photo).map(|p| lightcraft_engine::media::content_key(p)).ok_or("no such photo")?;
    let before = app.session.develop_of(photo).ok_or("no active photo settings")?;
    let command = command.to_string();
    let mut params = params.clone();
    let object = params.get("kind").and_then(Value::as_str) == Some("object");
    let sky = params.get("kind").and_then(Value::as_str) == Some("sky");
    let points = |key| -> Result<Vec<lightcraft_geom::Point>, String> {
        let Some(array) = params.get(key) else { return Ok(vec![]) };
        let array = array
            .as_array()
            .filter(|a| a.len() <= lightcraft_engine::segment::MAX_CLICKS)
            .ok_or("Object clicks must be at most 64 normalized point pairs")?;
        array
            .iter()
            .map(|p| {
                let coordinate = |key| p.get(key).and_then(Value::as_f64).filter(|v| v.is_finite() && (0.0..=1.0).contains(v));
                let x = coordinate(0).ok_or("invalid Object click x")?;
                let y = coordinate(1).ok_or("invalid Object click y")?;
                Ok(lightcraft_geom::Point::new(x, y))
            })
            .collect()
    };
    let include = if object { points("points")? } else { vec![] };
    let exclude = if object { points("exclude")? } else { vec![] };
    if include.len().saturating_add(exclude.len()) > lightcraft_engine::segment::MAX_CLICKS {
        return Err("an Object selection takes at most 64 clicks".into());
    }
    if command == "mask.addComponent" && params.get("id").is_none() {
        params["id"] = json!(app.session.active_mask);
    }
    crate::tasks::spawn(
        app,
        MASK,
        Some("appleVision"),
        move || {
            if object {
                lightcraft_engine::apple::vision_object_selection(source, &include, &exclude)
            } else if sky {
                lightcraft_engine::apple::vision_sky_selection(source)
            } else {
                lightcraft_engine::apple::vision_selection(source)
            }
        },
        move |app, ctx, result| {
            let current = app.session.catalog.photo(photo).map(|p| lightcraft_engine::media::content_key(p));
            if generation != app.session.library_generation()
                || app.session.active() != Some(photo)
                || current.as_ref() != Some(&content)
                || !app.session.develop_of(photo).is_some_and(|d| std::sync::Arc::ptr_eq(&d, &before))
            {
                app.toast(ctx, "Apple Vision selection discarded because the photo or mask changed");
                return;
            }
            let result = result.and_then(|seg| {
                params["seg"] = serde_json::to_value(seg).map_err(|e| e.to_string())?;
                app.session.execute(&command, &params).map_err(|e| e.to_string())
            });
            match result {
                Ok(_) => app.toast(ctx, "Apple Vision mask added"),
                Err(error) => app.toast_error(ctx, error),
            }
        },
    )?;
    Ok(json!({"pending":true, "provider":"appleVision"}))
}

fn start_sky_update(app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    if app.tasks.is_running(MASK) {
        return Err("Apple Vision is selecting. Wait for the selection to finish.".into());
    }
    let job = lightcraft_engine::apple::VisionSkyUpdate::prepare(&mut app.session, params)?;
    crate::tasks::spawn(
        app,
        MASK,
        Some("appleVision"),
        move || {
            let result = job.run();
            (job, result)
        },
        move |app, ctx, (job, result)| match result.and_then(|seg| job.accept(&mut app.session, seg)) {
            Ok(_) => app.toast(ctx, "Sky selection updated"),
            Err(error) => app.toast_error(ctx, error),
        },
    )?;
    Ok(json!({"pending":true, "provider":"appleVision"}))
}

fn start_object_click(app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    if app.tasks.is_running(MASK) {
        return Err("Apple Vision is selecting. Wait for the selection to finish.".into());
    }
    let job = lightcraft_engine::apple::VisionObjectClick::prepare(&mut app.session, params)?;
    crate::tasks::spawn(
        app,
        MASK,
        Some("appleVision"),
        move || {
            let result = job.run();
            (job, result)
        },
        move |app, ctx, (job, result)| match result.and_then(|seg| job.accept(&mut app.session, seg)) {
            Ok(_) => {
                app.ui.detail_due = None;
                app.toast(ctx, "Apple Vision mask updated");
            }
            Err(error) => app.toast_error(ctx, error),
        },
    )?;
    Ok(json!({"pending":true, "provider":"appleVision"}))
}

/// Library controls are part of the regular sidebar; the Photos grid is the normal main grid.
pub fn sidebar(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    if app.session.apple_library.is_none() {
        return;
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if app.tasks.is_running(READ) {
            ui.spinner();
            ui.label("Reading Photos…");
        } else if ui.button("Refresh Photos").clicked() {
            let _ = app.run("library.applePhotos", json!({}));
        }
    });
    if app.session.apple_library.as_ref().is_some_and(|l| l.authorization == "limited") {
        ui.label("Limited Photos access");
    }
    if let Some(error) = app.apple_photos.error.clone() {
        ui.colored_label(egui::Color32::LIGHT_RED, error);
    }
    if let Some(id) = app.session.active() {
        download_button(app, ui, id, None, "selected");
    }
    ui.small(crate::i18n::tr("Photos previews only. Download each original when needed."));
    let can_save = app.session.active().is_some() && !app.tasks.is_running(SAVE);
    let button = ui.add_enabled(can_save, egui::Button::new("Save edited copy to Photos"));
    crate::widgets::register(ui.ctx(), "button:photosSaveCopy", button.rect);
    if button.clicked()
        && let Err(error) = app.run("photo.saveApplePhotosCopy", json!({}))
    {
        app.toast_error(ui.ctx(), error);
    }
    ui.small("Edits save as settings. Copies go to Photos > LightCraft.");
    ui.add_space(8.0);
}

pub fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    if !app.apple_photos.open || app.session.apple_library.is_none() {
        return;
    }
    let mut open = app.apple_photos.open;
    egui::Window::new("Apple Photos").id(egui::Id::new("applePhotosLibrary")).open(&mut open).default_width(460.0).show(ctx, |ui| {
        ui.label("Your System Photo Library is LightCraft’s main library. Photos and albums appear directly in the main window.");
        ui.label(
            "Originals remain unchanged. Save an edited copy to place the rendered image and its adjustment recipe in the Photos album LightCraft.",
        );
        sidebar(app, ui);
    });
    app.apple_photos.open = open;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> lightcraft_engine::apple::LoadedLibrary {
        lightcraft_engine::apple::LoadedLibrary::metadata(
            serde_json::from_value(json!({
                "authorization":"authorized", "assets":[{"id":"a","name":"a.CR3","width":8,"height":6,
                "raw":true,"favorite":false,"format":"CR3","revision":"r1","captured":null}],"albums":[]
            }))
            .unwrap(),
        )
    }

    #[test]
    fn a_focused_library_stays_idle_across_minutes_and_long_refreshes() {
        let mut session = lightcraft_engine::Session::new().with_apple_photos(None);
        session.apply_photos_snapshot(metadata()).unwrap();
        let mut app = LightcraftApp::new(session, crate::Services::default());
        app.apple_photos.attempted = true;
        app.apple_photos.focused = true;
        app.apple_photos.native_focused = true;
        let ctx = egui::Context::default();
        for time in [60.0, 120.0, 600.0] {
            app.last_time = time;
            let mut output = ctx.run_ui(egui::RawInput { focused: true, time: Some(time), ..Default::default() }, |ui| tick(&mut app, ui.ctx()));
            output.textures_delta.clear();
            assert!(app.tasks.is_empty(), "elapsed time alone must never rescan Photos");
            assert!(!app.apple_photos.refresh_requested);
        }
        app.last_time = 900.0;
        finish_refresh(&mut app, &ctx, Ok(metadata()));
        assert_eq!(app.apple_photos.refreshed_at, 900.0, "throttle from completion, not the scan's start time");
        assert!(app.tasks.is_empty());
    }

    #[test]
    fn browser_editing_focus_does_not_refresh_the_native_library() {
        let mut session = lightcraft_engine::Session::new().with_apple_photos(None);
        session.apply_photos_snapshot(metadata()).unwrap();
        let mut app = LightcraftApp::new(session, crate::Services::default());
        app.apple_photos.attempted = true;
        app.last_time = 600.0;
        let ctx = egui::Context::default();
        for focused in [true, false, true] {
            app.remote_input.focused = focused;
            let mut raw = egui::RawInput { focused: false, ..Default::default() };
            app.raw_input_hook(&mut raw);
            assert_eq!(raw.focused, focused);
            let mut output = ctx.run_ui(raw, |ui| tick(&mut app, ui.ctx()));
            output.textures_delta.clear();
            assert!(app.tasks.is_empty(), "the browser's virtual focus is not a return from Photos");
            assert!(!app.apple_photos.refresh_requested);
        }
    }

    #[test]
    fn a_refresh_finishing_during_a_slider_is_applied_once_after_it_ends() {
        let mut session = lightcraft_engine::Session::new().with_apple_photos(None);
        session.apply_photos_snapshot(metadata()).unwrap();
        session.selection = lightcraft_engine::Selection::single(PhotoId(1));
        session.begin_interaction("Exposure").unwrap();
        session.execute("develop.set", &json!({"control":"light.exposure","value":1.25})).unwrap();
        let mut app = LightcraftApp::new(session, crate::Services::default());
        app.apple_photos.attempted = true;
        app.apple_photos.focused = true;
        app.apple_photos.native_focused = true;
        app.last_time = 100.0;
        let ctx = egui::Context::default();
        let mut incoming = metadata();
        incoming.snapshot.assets[0].name = "renamed.CR3".into();
        finish_refresh(&mut app, &ctx, Ok(incoming));
        assert!(app.apple_photos.pending_refresh.is_some());
        assert_eq!(app.session.catalog.photo(PhotoId(1)).unwrap().file_name, "a.CR3");
        app.session.end_interaction().unwrap();
        let mut output = ctx.run_ui(egui::RawInput { focused: true, ..Default::default() }, |ui| tick(&mut app, ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(app.session.catalog.photo(PhotoId(1)).unwrap().file_name, "renamed.CR3");
        assert_eq!(app.session.catalog.photo(PhotoId(1)).unwrap().develop.light.exposure, 1.25);
        assert!(app.apple_photos.pending_refresh.is_none());
        assert!(app.tasks.is_empty(), "a completed response must not start another scan");
        app.session.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(app.session.catalog.photo(PhotoId(1)).unwrap().develop.light.exposure, 0.0);
    }

    #[test]
    fn selecting_for_editing_shows_an_explicit_download_action_without_render_jobs() {
        let mut session = lightcraft_engine::Session::new().with_apple_photos(None);
        session.apply_photos_snapshot(metadata()).unwrap();
        session.selection = lightcraft_engine::Selection::single(PhotoId(1));
        let mut app = LightcraftApp::new(session, crate::Services::default());
        app.ui.settings.gpu = false;
        app.ui.view = crate::state::ViewMode::Detail;
        app.ui.filmstrip = false;
        app.apple_photos.attempted = true;
        app.apple_photos.refreshed_at = 999.0;
        let mut host = crate::headless::Headless::new(app, [1600.0, 1000.0], 1.0);
        for _ in 0..5 {
            host.step();
        }
        assert!(host.app.widgets.iter().any(|(id, _)| id == "button:photosDownload:detail:1"));
        assert!(host.app.tasks.is_empty());
        assert_eq!(host.app.renderer.in_flight(), 0);
        assert_eq!(host.app.renderer.queued(), 0);
        assert!(!host.app.session.media.has_source(PhotoId(1), lightcraft_engine::media::SourceLevel::Preview));
        let image = std::sync::Arc::new(lightcraft_raster::Rgb32f { width: 8, height: 6, data: vec![[0.1, 0.2, 0.3]; 48] });
        host.app.session.media.insert(PhotoId(1), lightcraft_engine::media::SourceLevel::Preview, image);
        host.step();
        assert!(!host.app.widgets.iter().any(|(id, _)| id == "button:photosDownload:detail:1"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn photos_mask_tiles_use_vision_without_a_sam_download_dialog() {
        use lightcraft_engine::{
            Selection,
            catalog::{Op, Photo, Source},
            media::SourceLevel,
        };
        use std::{sync::Arc, time::Duration};
        let mut session = lightcraft_engine::Session::new().with_apple_photos(None);
        let id = PhotoId(1);
        let photo = Photo::new(
            id,
            Source::ApplePhotos { asset: "test-mask".into(), revision: "r1".into() },
            "example.CR3",
            "CR3",
            8,
            6,
            "2026-10-11T12:00:00Z",
        );
        session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        let image = Arc::new(lightcraft_raster::Rgb32f { width: 8, height: 6, data: vec![[0.1, 0.2, 0.3]; 48] });
        for level in [SourceLevel::Thumb, SourceLevel::Preview] {
            session.media.insert(id, level, image.clone());
        }
        session.selection = Selection::single(id);
        session.apple_library.as_mut().unwrap().loaded = true;
        session.segmenter.dir = Some(std::env::temp_dir().join("lightcraft-apple-ui-no-sam"));
        let mut app = LightcraftApp::new(session, crate::Services::default());
        app.ui.settings.gpu = false;
        app.apple_photos.attempted = true;
        app.ui.view = crate::state::ViewMode::Detail;
        app.ui.right = crate::state::RightPanel::Masking;
        let mut host = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
        let t = Duration::from_secs(15);
        for _ in 0..4 {
            host.step();
        }
        let r = host.request("ui.clickWidget", json!({"id":"maskNew:object"}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(host.app.ui.tool, "object");
        assert_eq!(host.app.ui.dialog, None, "native Object must never offer SAM");
        assert_eq!(host.app.session.develop_of(id).unwrap().masks.len(), 1);
        assert!(!host.app.session.segmenter.busy() && !host.app.session.segmenter.loaded());
        assert!(!host.app.session.segmenter.download_status().running);
        host.request("ui.clickWidget", json!({"id":"maskNew:prompt"}), t);
        assert_eq!(host.app.ui.dialog, None);
        assert_eq!(host.app.ui.describe, None, "unsupported native text selection stays disabled");
        let main = host.view.ctx.clone();
        host.app.headless_screenshot(&main, false);
        let text = host.app.shadow.as_ref().unwrap().painted_text().join("\n");
        assert!(text.contains("Apple Vision"));
        assert!(!text.contains("SAM 3"));
        for op in ["add", "subtract", "intersect"] {
            crate::panels::masking::start_object(&mut host.app, &main, op);
            assert_eq!(host.app.ui.dialog, None);
        }
        host.app.ai_error(&main, lightcraft_engine::segment::NOT_INSTALLED, Some(("object", "new")));
        assert_eq!(host.app.ui.dialog, None, "error fallback must not reopen the SAM dialog");
        host.app.offer_sam_download(None);
        assert_eq!(host.app.ui.dialog, None);
        assert!(host.app.tasks.is_empty(), "starting the empty selection needs no worker or model");
    }

    #[test]
    fn photos_library_controls_are_visible_in_the_main_sidebar_without_requesting_access() {
        let session = lightcraft_engine::Session::new().with_apple_photos(None);
        let mut app = LightcraftApp::new(session, crate::Services::default());
        app.ui.settings.gpu = false;
        app.apple_photos.attempted = true;
        app.apple_photos.error = Some("Photos permission needs attention".into());
        let mut host = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
        for _ in 0..4 {
            host.step();
        }
        let main = host.view.ctx.clone();
        host.app.headless_screenshot(&main, false);
        let text = host.app.shadow.as_ref().unwrap().painted_text().join("\n");
        assert!(text.contains("Apple Photos"));
        assert!(text.contains("Photos permission needs attention"));
        assert!(text.contains("Save edited copy to Photos"));
        assert!(text.contains("Download each original when needed"));
        assert!(!text.contains("iCloud downloads"));
        assert!(text.contains("Apple Photos unavailable"));
        assert!(!text.contains("Import selected originals"));
        assert!(!text.contains("Import photos with"));
        assert!(!text.contains("Local"));
        host.app.apple_photos.error = None;
        host.step();
        host.app.headless_screenshot(&main, false);
        let text = host.app.shadow.as_ref().unwrap().painted_text().join("\n");
        assert!(text.contains("Connecting to Apple Photos"));
        assert!(!text.contains("No photos"));
    }

    #[test]
    fn photos_library_shows_a_separate_download_button_on_each_photo() {
        use lightcraft_engine::{
            catalog::{MediaKind, Op, Photo, Source},
            media::SourceLevel,
        };
        use std::{sync::Arc, time::Duration};
        let mut session = lightcraft_engine::Session::new().with_apple_photos(None);
        for n in 1..=2 {
            let id = PhotoId(n);
            let mut photo = Photo::new(
                id,
                Source::ApplePhotos { asset: format!("test-{n}"), revision: "r1".into() },
                "example.CR3",
                "CR3",
                8,
                6,
                "2026-10-11T12:00:00Z",
            );
            photo.kind = MediaKind::Raw;
            session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
            session.media.insert(
                id,
                SourceLevel::Thumb,
                Arc::new(lightcraft_raster::Rgb32f { width: 8, height: 6, data: vec![[0.1, 0.2, 0.3]; 48] }),
            );
        }
        session.apple_library.as_mut().unwrap().loaded = true;
        let mut app = LightcraftApp::new(session, crate::Services::default());
        app.ui.settings.gpu = false;
        app.apple_photos.attempted = true;
        app.apple_photos.refreshed_at = 999.0; // Synthetic library: no consent or real PhotoKit request.
        let mut host = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
        host.settle(Duration::from_secs(10));
        for n in 1..=2 {
            let button = host.app.widgets.iter().find(|(id, _)| id == &format!("button:photosDownload:grid:{n}")).unwrap().1;
            let thumb = host.app.widgets.iter().find(|(id, _)| id == &format!("thumb:{n}")).unwrap().1;
            assert!(thumb.contains_rect(button), "download action belongs to its photo");
        }
        host.app.apple_photos.download_errors.insert(PhotoId(1), "Connection unavailable".into());
        host.step();
        let ctx = host.view.ctx.clone();
        host.app.headless_screenshot(&ctx, false);
        let text = host.app.shadow.as_ref().unwrap().painted_text().join("\n");
        assert!(text.contains("Retry download"));
        assert!(text.contains("Download original"), "the other photo keeps its own download action");
        assert!(host.app.tasks.is_empty(), "drawing the buttons never starts a download");
    }
}
