//! Keep local model inference off the UI thread for desktop and its web controller.
use crate::LightcraftApp;
use serde_json::{Value, json};
const LABEL: &str = "Removing with AI";
pub fn intercept(app: &mut LightcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !matches!(id, "spot.aiRemove" | "spot.aiRegenerate") {
        return None;
    }
    Some(start(app, id, params))
}
fn start(app: &mut LightcraftApp, id: &str, params: &Value) -> Result<Value, String> {
    if app.tasks.is_running(LABEL) {
        return Err("AI Remove is already processing a repair".into());
    }
    let job = lightcraft_engine::ai_remove::Job::prepare(&mut app.session, params, id == "spot.aiRegenerate")?;
    let command = id.to_string();
    let photo = job.photo;
    crate::tasks::spawn(
        app,
        LABEL,
        Some("aiRemove"),
        move || {
            let result = job.run();
            (job, result)
        },
        move |app, ctx, (job, result)| {
            let result = result.and_then(|generated| app.session.execute_fn(&command, |s| job.accept(s, generated)).map_err(|e| e.to_string()));
            match result {
                Ok(_) => {
                    applied(app, photo);
                    app.toast(ctx, crate::i18n::tr("AI removal applied"));
                }
                Err(e) => app.toast(ctx, e),
            }
        },
    )?;
    Ok(json!({"pending":true,"provider":"coreML"}))
}

/// Leave the completed image unobstructed and the brush ready for another repair.
pub(crate) fn applied(app: &mut LightcraftApp, photo: lightcraft_catalog::PhotoId) {
    app.renderer.retry_photo(photo);
    if app.session.active() == Some(photo) {
        app.ui.remove_edits = false;
        app.session.active_spot = None;
    }
}
