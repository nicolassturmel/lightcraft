//! The main Apple Photos library, edit-copy saves and native mask jobs.
use crate::LightcraftApp;
use serde_json::{Value, json};

#[derive(Default)]
pub struct Browser {
    pub open: bool,
    pub error: Option<String>,
    pub last_copy: Option<Value>,
    attempted: bool,
    focused: bool,
    refreshed_at: f64,
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
        "photo.reopenApplePhotosRecipe" => Some(start_recipe(app)),
        "library.importIntoApplePhotos" => Some(start_import(app, params)),
        "mask.add" | "mask.addComponent" if lightcraft_engine::apple::wants_vision(&app.session, params) => Some(start_mask(app, id, params)),
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
    if app.tasks.is_running(READ) {
        return Err("Apple Photos is already being read".into());
    }
    if app.session.interaction.is_some() {
        return Err("Finish the current adjustment before refreshing Photos".into());
    }
    let directory = app.session.apple_library.as_ref().and_then(|l| l.directory.clone());
    let generation = app.session.library_generation();
    app.apple_photos.attempted = true;
    app.apple_photos.refreshed_at = app.last_time;
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
            if app.session.interaction.is_some() {
                app.apple_photos.refreshed_at = -60.0;
                return;
            }
            let result = result.and_then(|loaded| app.session.apply_photos_snapshot(loaded));
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
        },
    )?;
    Ok(json!({"pending":true, "backend":"applePhotos"}))
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
                app.apple_photos.refreshed_at = -60.0;
            }
            Err(error) => app.toast_error(ctx, error),
        },
    )?;
    Ok(json!({"pending":true}))
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
    let network = app.session.media.photos_network;
    crate::tasks::spawn(
        app,
        RECIPE,
        Some("applePhotos"),
        move || lightcraft_engine::apple::load_recipe(&asset, network),
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
                app.apple_photos.refreshed_at = -60.0;
            }
            Err(error) => app.toast_error(ctx, error),
        },
    )?;
    Ok(json!({"pending":true}))
}

/// Load at launch, reconcile when returning from Photos, and periodically while focused.
pub fn tick(app: &mut LightcraftApp, ctx: &egui::Context) {
    if app.session.apple_library.is_none() {
        return;
    }
    let focused = ctx.input(|i| i.focused);
    let loaded = app.session.apple_library.as_ref().is_some_and(|l| l.loaded);
    let elapsed = app.last_time - app.apple_photos.refreshed_at;
    let refresh = !app.apple_photos.attempted
        || (loaded && app.apple_photos.error.is_none() && ((focused && !app.apple_photos.focused && elapsed > 5.0) || elapsed > 60.0));
    app.apple_photos.focused = focused;
    if refresh
        && !app.tasks.is_running(READ)
        && app.session.interaction.is_none()
        && let Err(error) = start_refresh(app)
    {
        app.apple_photos.error = Some(error);
    }
    if loaded {
        ctx.request_repaint_after(std::time::Duration::from_secs(5));
    }
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
        return Err("Apple Vision is already selecting a subject".into());
    }
    let source = lightcraft_engine::apple::vision_source(&mut app.session)?;
    let photo = app.session.active().ok_or("no active photo")?;
    let generation = app.session.library_generation();
    let content = app.session.catalog.photo(photo).map(|p| lightcraft_engine::media::content_key(p)).ok_or("no such photo")?;
    let command = command.to_string();
    let mut params = params.clone();
    if command == "mask.addComponent" && params.get("id").is_none() {
        params["id"] = json!(app.session.active_mask);
    }
    crate::tasks::spawn(
        app,
        MASK,
        Some("appleVision"),
        move || lightcraft_engine::apple::vision_selection(source),
        move |app, ctx, result| {
            let current = app.session.catalog.photo(photo).map(|p| lightcraft_engine::media::content_key(p));
            if generation != app.session.library_generation() || app.session.active() != Some(photo) || current.as_ref() != Some(&content) {
                app.toast(ctx, "Apple Vision selection discarded because the active photo changed");
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
    let mut network = app.session.media.photos_network;
    if ui.checkbox(&mut network, "iCloud downloads").on_hover_text("Allow Photos to retrieve an original when it is stored only in iCloud").changed()
    {
        let _ = app.run("library.photosNetwork", json!({"enabled":network}));
    }
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
}
