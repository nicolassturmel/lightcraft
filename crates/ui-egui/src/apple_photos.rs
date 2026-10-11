//! Apple Photos browsing and native mask jobs, using the regular command/background task paths.
use crate::LightcraftApp;
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Default)]
pub struct Browser {
    pub open: bool,
    pub page: Value,
    pub selected: BTreeSet<String>,
    pub album: String,
    pub offset: u64,
    pub network: bool,
    pub error: Option<String>,
}

const READ: &str = "Reading Apple Photos";
const IMPORT: &str = "Copying Apple Photos originals";
const MASK: &str = "Selecting with Apple Vision";

/// Intercept slow native commands while keeping the engine's synchronous CLI/MCP forms.
pub fn intercept(app: &mut LightcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    match id {
        "library.applePhotos" => Some(start_list(app, params)),
        "library.importApplePhotos" => Some(start_import(app, params)),
        "mask.add" | "mask.addComponent" if lightcraft_engine::apple::wants_vision(&app.session, params) => Some(start_mask(app, id, params)),
        _ => None,
    }
}

pub fn open(app: &mut LightcraftApp) -> Result<Value, String> {
    lightcraft_engine::apple::enabled(&app.session)?;
    app.apple_photos.open = true;
    let params = json!({"offset":app.apple_photos.offset, "album":app.apple_photos.album});
    start_list(app, &params)
}

fn start_list(app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    lightcraft_engine::apple::enabled(&app.session)?;
    if app.tasks.is_running(READ) {
        return Err("Apple Photos is already being read".into());
    }
    let params = params.clone();
    app.apple_photos.error = None;
    crate::tasks::spawn(
        app,
        READ,
        Some("applePhotos"),
        move || lightcraft_engine::apple::list_photos(&params),
        |app, ctx, result| match result {
            Ok(page) => app.apple_photos.page = page,
            Err(error) => {
                app.apple_photos.error = Some(error.clone());
                app.toast_error(ctx, error);
            }
        },
    )?;
    Ok(json!({"pending":true}))
}

fn start_import(app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    lightcraft_engine::apple::enabled(&app.session)?;
    if app.import.is_some() || app.tasks.is_running(IMPORT) || crate::lightroom_import::is_running(app) {
        return Err("finish the current import first".into());
    }
    let request = lightcraft_engine::apple::export_request(&app.session, params)?;
    let generation = app.session.library_generation();
    crate::tasks::spawn(
        app,
        IMPORT,
        Some("applePhotos"),
        move || lightcraft_engine::apple::export_originals(&request),
        move |app, ctx, result| {
            let result = result.and_then(|exported| {
                if generation != app.session.library_generation() {
                    return Err("The library changed while Photos originals were copied; retry in the original library".into());
                }
                if app.import.is_some() {
                    return Err("Another import started; the Photos copies are ready, retry when it finishes".into());
                }
                let params = lightcraft_engine::apple::import_params(&exported);
                let paths: Vec<String> =
                    exported.get("paths").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
                let errors = exported.get("errors").and_then(Value::as_array).map_or(0, Vec::len);
                if errors > 0 {
                    let reason = exported
                        .get("errors")
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                        .and_then(|v| v.get("error"))
                        .and_then(Value::as_str)
                        .unwrap_or("unavailable original");
                    app.apple_photos.error =
                        Some(format!("{errors} Photos original(s) could not be copied: {reason}. Enable iCloud downloads if needed."));
                    app.toast_error(ctx, app.apple_photos.error.clone().unwrap_or_default());
                }
                if !paths.is_empty() {
                    app.import = Some(crate::import::ImportTask::new(paths, params, app.session.undo.len(), false));
                    app.apple_photos.selected.clear();
                    app.apple_photos.open = false;
                }
                Ok(())
            });
            if let Err(error) = result {
                app.apple_photos.error = Some(error.clone());
                app.toast_error(ctx, error);
            }
        },
    )?;
    Ok(json!({"pending":true}))
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

pub fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    if !app.apple_photos.open {
        return;
    }
    let reading = app.tasks.is_running(READ);
    let importing = app.tasks.is_running(IMPORT) || app.import.is_some();
    let mut browser = std::mem::take(&mut app.apple_photos);
    let mut visible = browser.open;
    let mut reload = false;
    let mut import = false;
    egui::Window::new("Apple Photos")
        .id(egui::Id::new("applePhotosBrowser"))
        .open(&mut visible)
        .default_size(egui::vec2(720.0, 520.0))
        .resizable(true)
        .show(ctx, |ui| {
            ui.label("Choose photos from your System Photo Library. Original copies are kept in this LightCraft library.");
            ui.horizontal(|ui| {
                let previous = browser.album.clone();
                let albums = browser.page.get("albums").and_then(Value::as_array).cloned().unwrap_or_default();
                let label = albums
                    .iter()
                    .find(|a| a.get("id").and_then(Value::as_str) == Some(browser.album.as_str()))
                    .and_then(|a| a.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("All Photos");
                ui.add_enabled_ui(!reading && !importing, |ui| {
                    egui::ComboBox::from_id_salt("applePhotosAlbum").selected_text(label).show_ui(ui, |ui| {
                        ui.selectable_value(&mut browser.album, String::new(), "All Photos");
                        for album in &albums {
                            if let (Some(id), Some(name)) = (album.get("id").and_then(Value::as_str), album.get("name").and_then(Value::as_str)) {
                                ui.selectable_value(&mut browser.album, id.to_string(), name);
                            }
                        }
                    });
                });
                if previous != browser.album {
                    browser.offset = 0;
                    browser.selected.clear();
                    reload = true;
                }
                if ui.add_enabled(!reading, egui::Button::new("Refresh")).clicked() {
                    reload = true;
                }
                if reading {
                    ui.spinner();
                }
                ui.label(format!("{} photos", browser.page.get("total").and_then(Value::as_u64).unwrap_or(0)));
            });
            if browser.page.get("authorization").and_then(Value::as_str) == Some("limited") {
                ui.label("Photos access is limited to the selection permitted in macOS.");
            }
            if let Some(error) = &browser.error {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            ui.separator();
            egui::ScrollArea::vertical().max_height(350.0).show(ui, |ui| {
                for asset in browser.page.get("assets").and_then(Value::as_array).into_iter().flatten() {
                    let Some(id) = asset.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let name = asset.get("name").and_then(Value::as_str).unwrap_or("Photo");
                    let mut selected = browser.selected.contains(id);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!importing && (selected || browser.selected.len() < 200), egui::Checkbox::new(&mut selected, name))
                            .changed()
                        {
                            if selected {
                                browser.selected.insert(id.to_string());
                            } else {
                                browser.selected.remove(id);
                            }
                        }
                        if asset.get("raw").and_then(Value::as_bool) == Some(true) {
                            ui.label("RAW");
                        }
                        if asset.get("favorite").and_then(Value::as_bool) == Some(true) {
                            ui.label("★");
                        }
                        ui.label(format!(
                            "{} × {}",
                            asset.get("width").and_then(Value::as_u64).unwrap_or(0),
                            asset.get("height").and_then(Value::as_u64).unwrap_or(0)
                        ));
                        ui.label(asset.get("captured").and_then(Value::as_str).and_then(|s| s.get(..10)).unwrap_or(""));
                    });
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.add_enabled(!reading && browser.offset > 0, egui::Button::new("Previous")).clicked() {
                    browser.offset = browser.offset.saturating_sub(100);
                    reload = true;
                }
                let next = browser.page.get("nextOffset").and_then(Value::as_u64).unwrap_or(0);
                let total = browser.page.get("total").and_then(Value::as_u64).unwrap_or(0);
                if ui.add_enabled(!reading && next < total, egui::Button::new("Next")).clicked() {
                    browser.offset = next;
                    reload = true;
                }
                if ui.button("Clear selection").clicked() {
                    browser.selected.clear();
                }
                ui.label(format!("{} selected", browser.selected.len()));
            });
            ui.checkbox(&mut browser.network, "Download originals from iCloud when needed");
            if ui.add_enabled(!reading && !importing && !browser.selected.is_empty(), egui::Button::new("Import selected originals")).clicked() {
                import = true;
            }
        });
    browser.open = visible;
    app.apple_photos = browser;
    let result = if import {
        Some(app.run("library.importApplePhotos", json!({"ids":app.apple_photos.selected, "network":app.apple_photos.network})))
    } else if reload && !reading {
        Some(app.run("library.applePhotos", json!({"offset":app.apple_photos.offset, "album":app.apple_photos.album})))
    } else {
        None
    };
    if let Some(Err(error)) = result {
        app.apple_photos.error = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn photos_browser_is_visible_in_offscreen_screenshots_without_requesting_access() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        app.ui.settings.gpu = false;
        app.apple_photos.open = true;
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
        assert!(text.contains("Import selected originals"));
    }
}
