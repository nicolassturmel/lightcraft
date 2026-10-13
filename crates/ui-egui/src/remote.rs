//! Thin browser input and bounded desktop previews. The browser owns no Session or catalogue.
use crate::LightcraftApp;
use egui::{Event, Modifiers};
use serde_json::{Value, json};
use std::hash::{Hash, Hasher};

pub const MAX_FRAME_EDGE: usize = 1920;
const MAX_EVENTS: usize = 64;

#[derive(Clone)]
pub(crate) struct BrowserConnection {
    pub peer: String,
    pub code: String,
    pub expires: std::time::Instant,
    pub reply: std::sync::mpsc::Sender<bool>,
}

pub(crate) fn request_connection(app: &mut LightcraftApp, ctx: &egui::Context, request: BrowserConnection) -> Result<Value, String> {
    app.browser_connections.retain(|r| r.expires > std::time::Instant::now());
    if request.expires <= std::time::Instant::now() || app.browser_connections.len() >= 8 {
        let _ = request.reply.send(false);
        return Err("browser connection request expired or the queue is full".into());
    }
    // Browser input is suspended while approval is visible. Clear already-queued browser
    // gestures so even an authorized browser cannot click another device's approval button.
    app.synthetic.clear();
    app.remote_input = InputState::default();
    app.browser_connections.push(request);
    ctx.request_repaint();
    Ok(json!({"pending":true}))
}

pub(crate) fn show_connections(app: &mut LightcraftApp, ctx: &egui::Context) {
    app.browser_connections.retain(|r| r.expires > std::time::Instant::now());
    let Some(request) = app.browser_connections.first() else { return };
    let mut choice = None;
    egui::Window::new("Connexion au navigateur")
        .id(egui::Id::new("browserConnection"))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(format!("Un navigateur à l’adresse {} veut piloter LightCraft.", request.peer));
            ui.add_space(8.0);
            ui.label(egui::RichText::new(format!("Code : {}", request.code)).strong().size(24.0));
            ui.label("Vérifiez que ce code apparaît sur votre navigateur. L’accès partage votre photothèque et permet de modifier vos retouches pendant cette session.");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                for (id, label, allowed) in [("webConnection:deny", "Refuser", false), ("webConnection:allow", "Autoriser", true)] {
                    let button = ui.button(label);
                    crate::widgets::register(ctx, format!("button:{id}"), button.rect);
                    if button.clicked() {
                        choice = Some(allowed);
                    }
                }
            });
        });
    ctx.request_repaint_after(std::time::Duration::from_secs(1));
    if let Some(allowed) = choice {
        let request = app.browser_connections.remove(0);
        let _ = request.reply.send(allowed);
    }
}

pub struct DesktopFrame {
    pub image: egui::ColorImage,
    /// Logical egui coordinates, independent of the preview's pixel density.
    pub viewport: [f32; 2],
    pub cursor: &'static str,
    pub state: String,
}

#[derive(Default)]
pub(crate) struct InputState {
    pub focused: bool,
    buttons: [bool; 3],
    keys: std::collections::BTreeSet<egui::Key>,
    position: egui::Pos2,
    last: f64,
}

pub(crate) fn state_key(app: &LightcraftApp) -> String {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    app.session.library_generation().hash(&mut hash);
    app.session.active().map(|id| id.0).hash(&mut hash);
    app.session.active_mask.hash(&mut hash);
    format!("{:?}:{:?}", app.ui.view, app.ui.right).hash(&mut hash);
    if let Some(photo) = app.session.active().and_then(|id| app.session.catalog.photo(id)) {
        format!("{:?}", photo.source).hash(&mut hash);
    }
    format!("{:016x}", hash.finish())
}

fn button_index(button: egui::PointerButton) -> Option<usize> {
    match button {
        egui::PointerButton::Primary => Some(0),
        egui::PointerButton::Middle => Some(1),
        egui::PointerButton::Secondary => Some(2),
        _ => None,
    }
}

pub(crate) fn expire(app: &mut LightcraftApp, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    if (app.remote_input.buttons.iter().any(|v| *v) || !app.remote_input.keys.is_empty()) && now - app.remote_input.last > 3.0 {
        for (index, button) in [egui::PointerButton::Primary, egui::PointerButton::Middle, egui::PointerButton::Secondary].into_iter().enumerate() {
            if app.remote_input.buttons.get(index).copied().unwrap_or(false) {
                app.synthetic.push(Event::PointerButton { pos: app.remote_input.position, button, pressed: false, modifiers: Modifiers::NONE });
            }
        }
        app.synthetic.push(Event::PointerGone);
        for key in std::mem::take(&mut app.remote_input.keys) {
            app.synthetic.push(Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE });
        }
        app.remote_input.buttons.fill(false);
        app.remote_input.focused = false;
        ctx.request_repaint();
    }
}

pub(crate) fn cursor(icon: egui::CursorIcon) -> &'static str {
    match icon {
        egui::CursorIcon::PointingHand => "pointer",
        egui::CursorIcon::Text => "text",
        egui::CursorIcon::Crosshair => "crosshair",
        egui::CursorIcon::Grab => "grab",
        egui::CursorIcon::Grabbing => "grabbing",
        egui::CursorIcon::ResizeHorizontal => "ew-resize",
        egui::CursorIcon::ResizeVertical => "ns-resize",
        egui::CursorIcon::ResizeNwSe => "nwse-resize",
        egui::CursorIcon::ResizeNeSw => "nesw-resize",
        egui::CursorIcon::NotAllowed => "not-allowed",
        egui::CursorIcon::Wait => "wait",
        egui::CursorIcon::Progress => "progress",
        egui::CursorIcon::None => "none",
        _ => "default",
    }
}

fn number(value: &Value, name: &str, min: f64, max: f64) -> Result<f32, String> {
    value
        .get(name)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && (min..=max).contains(n))
        .map(|n| n as f32)
        .ok_or_else(|| format!("invalid browser input {name}"))
}

fn events(params: &Value, viewport: egui::Vec2) -> Result<Vec<Event>, String> {
    let size = params.get("viewport").and_then(Value::as_array).filter(|v| v.len() == 2).ok_or("missing browser viewport")?;
    let dimension = |at: usize| size.get(at).and_then(Value::as_f64).filter(|v| v.is_finite() && (1.0..=32768.0).contains(v)).map(|v| v as f32);
    let (w, h) = (dimension(0).ok_or("invalid browser width")?, dimension(1).ok_or("invalid browser height")?);
    if (w - viewport.x).abs() > 1.0 || (h - viewport.y).abs() > 1.0 {
        return Err("The Mac window changed size; refresh the desktop preview before continuing.".into());
    }
    let input =
        params.get("events").and_then(Value::as_array).filter(|v| !v.is_empty() && v.len() <= MAX_EVENTS).ok_or("browser input needs 1–64 events")?;
    let mut result = Vec::with_capacity(input.len().saturating_mul(3));
    for event in input {
        let flag = |name| event.get(name).and_then(Value::as_bool).unwrap_or(false);
        let modifiers = Modifiers { alt: flag("alt"), ctrl: flag("ctrl"), shift: flag("shift"), mac_cmd: flag("os"), command: flag("cmd") };
        let point = || -> Result<egui::Pos2, String> { Ok(egui::pos2(number(event, "x", 0.0, 1.0)? * w, number(event, "y", 0.0, 1.0)? * h)) };
        let text = || event.get("text").and_then(Value::as_str).filter(|v| v.len() <= 32768).ok_or("invalid browser text");
        match event.get("kind").and_then(Value::as_str) {
            Some("heartbeat") => {}
            Some("focus") => result.push(Event::WindowFocused(event.get("focused").and_then(Value::as_bool).ok_or("invalid browser focus")?)),
            Some("move") => result.push(Event::PointerMoved(point()?)),
            Some("down" | "up") => {
                let button = match event.get("button").and_then(Value::as_u64).unwrap_or(0) {
                    0 => egui::PointerButton::Primary,
                    1 => egui::PointerButton::Middle,
                    2 => egui::PointerButton::Secondary,
                    _ => return Err("invalid browser pointer button".into()),
                };
                let pos = point()?;
                result.push(Event::PointerMoved(pos));
                result.push(Event::PointerButton { pos, button, pressed: event.get("kind").and_then(Value::as_str) == Some("down"), modifiers });
            }
            Some("gone") => result.push(Event::PointerGone),
            Some("scroll") => {
                result.push(Event::PointerMoved(point()?));
                result.push(Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(number(event, "dx", -4096.0, 4096.0)?, number(event, "dy", -4096.0, 4096.0)?),
                    phase: egui::TouchPhase::Move,
                    modifiers,
                });
            }
            Some("key") => {
                let key = event.get("key").and_then(Value::as_str).and_then(crate::control::key_from).ok_or("invalid browser key")?;
                let paired = event.get("pressed").is_none();
                let pressed = event.get("pressed").and_then(Value::as_bool).unwrap_or(true);
                result.push(Event::Key { key, physical_key: None, pressed, repeat: flag("repeat"), modifiers });
                if let Some(text) = event.get("text").and_then(Value::as_str).filter(|v| !v.is_empty()) {
                    if !pressed || text.len() > 64 || modifiers.command || modifiers.ctrl {
                        return Err("invalid browser key text".into());
                    }
                    result.push(Event::Text(text.to_string()));
                }
                if paired {
                    result.push(Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers });
                }
            }
            Some("text") => result.push(Event::Text(text()?.to_string())),
            Some("paste") => result.push(Event::Paste(text()?.to_string())),
            _ => return Err("unknown browser input event".into()),
        }
    }
    Ok(result)
}

pub(crate) fn input(app: &mut LightcraftApp, ctx: &egui::Context, params: &Value) -> Result<Value, String> {
    if !app.browser_connections.is_empty() {
        return Err("Confirmez la connexion directement dans LightCraft sur le Mac.".into());
    }
    if params.get("state").and_then(Value::as_str) != Some(state_key(app).as_str()) {
        return Err("The Mac view changed; wait for its updated preview before continuing.".into());
    }
    let mut input = events(params, ctx.content_rect().size())?;
    // Validate the whole batch first: an invalid late event must not apply earlier edits.
    if app.synthetic.len().saturating_add(input.len()) > 512 {
        return Err("browser input queue is busy; retry after the next preview".into());
    }
    let count = input.len();
    let zoom = ctx.zoom_factor();
    for event in &mut input {
        match event {
            Event::PointerMoved(position) | Event::PointerButton { pos: position, .. } => *position *= zoom,
            _ => {}
        }
        match event {
            Event::WindowFocused(focused) => app.remote_input.focused = *focused,
            Event::PointerMoved(position) => app.remote_input.position = *position,
            Event::PointerButton { button, pressed, .. } => {
                if let Some(index) = button_index(*button)
                    && let Some(state) = app.remote_input.buttons.get_mut(index)
                {
                    *state = *pressed;
                }
            }
            Event::Key { key, pressed, .. } => {
                if *pressed {
                    app.remote_input.keys.insert(*key);
                } else {
                    app.remote_input.keys.remove(key);
                }
            }
            _ => {}
        }
    }
    app.remote_input.last = ctx.input(|i| i.time);
    if app.remote_input.buttons.iter().any(|v| *v) || !app.remote_input.keys.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_secs(3));
    }
    app.synthetic.extend(input);
    ctx.request_repaint();
    Ok(json!({"events":count}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connection_approval_is_native_only_and_releases_waiting_browser() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        app.ui.settings.gpu = false;
        let mut h = crate::headless::Headless::new(app, [1600.0, 1000.0], 1.0);
        for _ in 0..4 {
            h.step();
        }
        let (request, decision) = crate::ControlRequest::browser_connection(
            "192.0.2.5".into(),
            "ABC123".into(),
            std::time::Instant::now() + std::time::Duration::from_secs(120),
        );
        h.app.synthetic.push(Event::PointerButton {
            pos: egui::pos2(500.0, 500.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        crate::control::handle(&mut h.app, &h.view.ctx, &request);
        assert!(h.app.synthetic.is_empty());
        assert!(decision.try_recv().is_err());
        assert!(input(&mut h.app, &h.view.ctx, &json!({"events":[{"kind":"focus","focused":false}]})).is_err());
        for _ in 0..2 {
            h.step();
        }
        let (browser, _) = crate::ControlRequest::web("ui.clickWidget", json!({"id":"button:webConnection:allow"}));
        assert!(matches!(crate::control::handle(&mut h.app, &h.view.ctx, &browser), crate::control::Outcome::Done(v) if v["ok"] == false));
        assert!(decision.try_recv().is_err());
        let response = h.request("ui.clickWidget", json!({"id":"button:webConnection:allow"}), std::time::Duration::from_secs(10));
        assert_eq!(response["ok"], true);
        for _ in 0..4 {
            h.step();
        }
        assert!(decision.recv_timeout(std::time::Duration::from_secs(1)).unwrap());
        assert!(h.app.browser_connections.is_empty());
        let (fake, _) = crate::ControlRequest::new("ui.browserConnection", json!({"peer":"192.0.2.5","code":"ABC123"}));
        assert!(matches!(crate::control::handle(&mut h.app, &h.view.ctx, &fake), crate::control::Outcome::Done(v) if v["ok"] == false));
    }

    #[test]
    fn expired_connections_never_open_an_approval_window() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        let ctx = egui::Context::default();
        let (request, decision) = crate::ControlRequest::browser_connection("192.0.2.5".into(), "ABC123".into(), std::time::Instant::now());
        assert!(matches!(crate::control::handle(&mut app, &ctx, &request), crate::control::Outcome::Done(v) if v["ok"] == false));
        assert!(!decision.recv_timeout(std::time::Duration::from_secs(1)).unwrap());
        assert!(app.browser_connections.is_empty());
    }

    #[test]
    fn browser_undo_uses_the_desktop_history_even_with_native_menu_accelerators() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        app.ui.settings.gpu = false;
        let mut h = crate::headless::Headless::new(app, [1600.0, 1000.0], 1.0);
        for _ in 0..4 {
            h.step();
        }
        h.app.session.execute("develop.set", &json!({"control":"light.exposure","value":1.0})).unwrap();
        h.app.native_shortcuts.insert("Cmd+Z".into());
        h.app.remote_input.focused = true;
        h.request("ui.key", json!({"key":"Z","cmd":true,"os":true}), std::time::Duration::from_secs(10));
        for _ in 0..3 {
            h.step();
        }
        assert_eq!(h.app.session.develop_of(h.app.session.active().unwrap()).unwrap().light.exposure, 0.0);
    }
    #[test]
    fn binary_desktop_preview_keeps_the_session_and_caps_retina_pixels() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        app.ui.settings.gpu = false;
        let mut h = crate::headless::Headless::new(app, [3200.0, 1800.0], 2.0);
        for _ in 0..4 {
            h.step();
        }
        let generation = h.app.session.library_generation();
        let revision = h.app.session.catalog.revision;
        let (tx, rx) = std::sync::mpsc::channel();
        h.app.pending_desktop_frame = Some(tx);
        h.step();
        let frame = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(frame.viewport, [3200.0, 1800.0]);
        assert!(frame.image.size.into_iter().all(|v| v > 0 && v <= MAX_FRAME_EDGE + 1));
        assert_eq!(frame.image.pixels.len(), frame.image.size[0] * frame.image.size[1]);
        assert_eq!(h.app.session.library_generation(), generation);
        assert_eq!(h.app.session.catalog.revision, revision);
        assert!(h.app.session.active().is_none(), "a view must not create a demo library");
    }
    #[test]
    fn invalid_and_stale_inputs_never_enqueue_partial_actions() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        app.ui.settings.gpu = false;
        let mut h = crate::headless::Headless::new(app, [1600.0, 1000.0], 1.0);
        for _ in 0..3 {
            h.step();
        }
        let state = state_key(&h.app);
        let invalid = json!({"state":state,"viewport":[1600,1000],"events":[{"kind":"down","x":0.5,"y":0.5},{"kind":"no such event"}]});
        assert!(input(&mut h.app, &h.view.ctx, &invalid).is_err());
        assert!(h.app.synthetic.is_empty());
        let active = h.app.session.active().unwrap().0;
        h.app.session.execute("library.select", &json!({"ids":[if active==1 {2} else {1}]})).unwrap();
        assert_ne!(state_key(&h.app), state);
        assert!(input(&mut h.app, &h.view.ctx, &json!({"state":state,"viewport":[1600,1000],"events":[{"kind":"key","key":"G"}]})).is_err());
        assert!(h.app.synthetic.is_empty());
    }
    #[test]
    fn disconnected_drags_and_held_keys_are_released() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        app.remote_input.buttons[0] = true;
        app.remote_input.keys.insert(egui::Key::Space);
        app.remote_input.focused = true;
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput { time: Some(4.0), ..Default::default() }, |ui| expire(&mut app, ui.ctx()));
        output.textures_delta.clear();
        assert!(app.remote_input.buttons.iter().all(|v| !v));
        assert!(app.remote_input.keys.is_empty());
        assert!(!app.remote_input.focused);
        assert!(app.synthetic.iter().any(|e| matches!(e, Event::PointerButton { pressed: false, .. })));
        assert!(app.synthetic.iter().any(|e| matches!(e, Event::Key { key: egui::Key::Space, pressed: false, .. })));
    }
    #[test]
    fn viewport_mapping_and_modifiers_are_independent_of_preview_resolution() {
        let parsed = events(
            &json!({"viewport":[1600,1000],"events":[{"kind":"down","x":0.25,"y":0.7,"button":2,"alt":true},
            {"kind":"key","key":"G","text":"g"}, {"kind":"scroll","x":0.5,"y":0.5,"dx":0,"dy":-80}]}),
            egui::vec2(1600.0, 1000.0),
        )
        .unwrap();
        assert!(matches!(parsed[1], Event::PointerButton { pos, button:egui::PointerButton::Secondary, modifiers, pressed:true }
            if pos==egui::pos2(400.0,700.0) && modifiers.alt));
        assert!(parsed.iter().any(|v| matches!(v, Event::Text(t) if t=="g")));
        assert!(parsed.iter().any(|v| matches!(v, Event::MouseWheel { delta,.. } if delta.y== -80.0)));
    }
    #[test]
    fn stale_viewports_and_hostile_batches_are_rejected_before_dispatch() {
        let viewport = egui::vec2(1600.0, 1000.0);
        for p in [
            json!({"viewport":[1000,1000],"events":[{"kind":"gone"}]}),
            json!({"viewport":[1600,1000],"events":vec![json!({"kind":"gone"});65]}),
            json!({"viewport":[1600,1000],"events":[{"kind":"down","x":0.5,"y":0.5},{"kind":"move","x":1.1,"y":0.5}]}),
            json!({"viewport":[1600,1000],"events":[{"kind":"key","key":"no such key"}]}),
            json!({"viewport":[1600,1000],"events":[{"kind":"paste","text":"x".repeat(32769)}]}),
        ] {
            assert!(events(&p, viewport).is_err());
        }
    }
}
