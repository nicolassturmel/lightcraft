//! Per-mask RGB curves, using the global curve editor's point interaction.

use egui::{Color32, Sense, Stroke, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_develop::{ControlSpec, Mask, Section, Track};
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::Icon;
use crate::widgets::{flyout_row, register, slider, text_button};

pub(super) fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, photo: PhotoId, mask: &Mask) {
    let open = app.ui.flyout_open("maskCurve");
    if flyout_row(ui, "maskCurve", crate::i18n::tr("Curve"), Icon::Curve, open).clicked() {
        app.ui.toggle_flyout("maskCurve");
    }
    if !open {
        return;
    }
    let channel_id = ui.make_persistent_id(("mask-curve-channel", photo, mask.id));
    let mut channel = ui.data(|d| d.get_temp::<String>(channel_id)).unwrap_or_else(|| "master".into());
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 4 }).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 6.0);
            for (ch, color, label) in [
                ("master", Color32::WHITE, "Point curve"),
                ("red", Color32::from_rgb(221, 51, 51), "Red channel"),
                ("green", Color32::from_rgb(51, 187, 85), "Green channel"),
                ("blue", Color32::from_rgb(51, 119, 238), "Blue channel"),
            ] {
                let (rect, response) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
                register(ui.ctx(), format!("maskCurveChannel:{ch}"), rect);
                ui.painter().circle_stroke(rect.center(), 7.0, Stroke::new(2.0, color));
                if channel == ch {
                    ui.painter().circle_filled(rect.center(), 3.5, color);
                }
                let response = response.on_hover_text(crate::i18n::tr_format!("{name} — double-click to reset it", name = label));
                if response.clicked() || response.double_clicked() {
                    channel = ch.into();
                }
                if response.double_clicked() {
                    let _ = app.run("mask.curve.reset", json!({"id": mask.id, "channel": ch}));
                }
            }
            if text_button(ui, "maskCurveReset", crate::i18n::tr("Reset"), false).clicked() {
                let _ = app.run("mask.curve.reset", json!({"id": mask.id}));
            }
        });
    });
    ui.data_mut(|d| d.insert_temp(channel_id, channel.clone()));
    ui.push_id(("mask-curve", photo, mask.id, &channel), |ui| {
        let (rect, response) = super::curve_graph::canvas(ui, "maskCurve");
        response.context_menu(|ui| {
            if ui.button(crate::i18n::tr("Reset Channel")).clicked() {
                let _ = app.run("mask.curve.reset", json!({"id": mask.id, "channel": channel}));
                ui.close();
            }
            if ui.button(crate::i18n::tr("Reset All Curves")).clicked() {
                let _ = app.run("mask.curve.reset", json!({"id": mask.id}));
                ui.close();
            }
        });
        let points = match channel.as_str() {
            "red" => &mask.curve.red,
            "green" => &mask.curve.green,
            "blue" => &mask.curve.blue,
            _ => &mask.curve.master,
        };
        let edit = super::curve_graph::points(ui, rect, &response, &channel, points);
        if edit.started {
            let _ = app.run("develop.beginInteraction", json!({"label": "Mask Curve"}));
        }
        if let Some(points) = edit.points {
            let points: Vec<[f64; 2]> = points.iter().map(|p| [p.x, p.y]).collect();
            let _ = app.run("mask.curve", json!({"id": mask.id, "channel": channel, "points": points}));
        }
        if edit.stopped {
            let _ = app.run("develop.endInteraction", json!({}));
        }
    });
    let spec = ControlSpec {
        id: "maskCurveRefineSaturation",
        label: "Refine Saturation",
        section: Section::Curve,
        min: 0.0,
        max: 100.0,
        default: 100.0,
        step: 1.0,
        decimals: 0,
        track: Track::Gradient { from: "#7a7a7a", to: "#d85a3a" },
    };
    let out = slider(ui, &spec, mask.curve.refine_saturation, true, None);
    super::edit::apply_slider_out(app, &spec, out, |app, value| app.run("mask.curve", json!({"id": mask.id, "refineSaturation": value})));
}
