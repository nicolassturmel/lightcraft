//! Shared point-curve graph for global and masked adjustments.

use egui::{Color32, Pos2, Rect, Response, Sense, Stroke, pos2, vec2};
use lightcraft_geom::Point;

use crate::theme::Tokens;
use crate::widgets::{hex, register};

#[derive(Default)]
pub(super) struct Edit {
    pub points: Option<Vec<Point>>,
    pub started: bool,
    pub stopped: bool,
}

pub(super) fn canvas(ui: &mut egui::Ui, id: &str) -> (Rect, Response) {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let side = (w - 48.0).max(40.0);
    let (outer, _) = ui.allocate_exact_size(vec2(w, side + 12.0), Sense::hover());
    let r = Rect::from_min_size(pos2(outer.left() + 24.0, outer.top() + 6.0), vec2(side, side));
    let response = ui.interact(r, ui.make_persistent_id(id), Sense::click_and_drag());
    register(ui.ctx(), id, r);
    let p = ui.painter();
    p.rect_filled(r, 2.0, t.canvas);
    for i in 1..4 {
        let f = i as f32 / 4.0;
        p.line_segment(
            [pos2(r.left() + r.width() * f, r.top()), pos2(r.left() + r.width() * f, r.bottom())],
            Stroke::new(1.0, Color32::from_gray(52)),
        );
        p.line_segment(
            [pos2(r.left(), r.top() + r.height() * f), pos2(r.right(), r.top() + r.height() * f)],
            Stroke::new(1.0, Color32::from_gray(52)),
        );
    }
    p.line_segment([r.left_bottom(), r.right_top()], Stroke::new(1.0, Color32::from_gray(70)));
    (r, response)
}

pub(super) fn points(ui: &mut egui::Ui, r: Rect, response: &Response, channel: &str, input: &[Point]) -> Edit {
    let t = Tokens::get(ui.ctx());
    let mut pts: Vec<Point> =
        input.iter().filter(|p| p.x.is_finite() && p.y.is_finite()).map(|p| Point::new(p.x.clamp(0.0, 1.0), p.y.clamp(0.0, 1.0))).collect();
    pts.sort_by(|a, b| a.x.total_cmp(&b.x));
    pts.dedup_by(|a, b| (a.x - b.x).abs() < 1e-6);
    if pts.len() < 2 {
        pts = vec![Point::new(0.0, 0.0), Point::new(1.0, 1.0)];
    }
    let curve = lightcraft_color::spline::MonotoneCurve::new(&pts.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>());
    let color = match channel {
        "red" => hex("#dd3333"),
        "green" => hex("#33bb55"),
        "blue" => hex("#3377ee"),
        _ => Color32::from_gray(225),
    };
    let to_screen = |p: Point| pos2(r.left() + p.x as f32 * r.width(), r.bottom() - p.y as f32 * r.height());
    let from_screen =
        |p: Pos2| Point::new(((p.x - r.left()) / r.width()).clamp(0.0, 1.0) as f64, ((r.bottom() - p.y) / r.height()).clamp(0.0, 1.0) as f64);
    let p = ui.painter();
    let line = (0..=96)
        .map(|i| {
            let x = i as f64 / 96.0;
            to_screen(Point::new(x, curve.eval(x).clamp(0.0, 1.0)))
        })
        .collect();
    p.add(egui::Shape::line(line, Stroke::new(2.0, color)));
    let drag_id = response.id.with("point-drag");
    let mut dragging = ui.data(|d| d.get_temp::<Option<usize>>(drag_id)).flatten();
    if !ui.input(|i| i.pointer.primary_down()) && !response.drag_stopped() {
        dragging = None;
    }
    let nearest = |q: Pos2| pts.iter().enumerate().map(|(i, p)| (i, to_screen(*p).distance(q))).min_by(|a, b| a.1.total_cmp(&b.1));
    let hovered = response.hover_pos().and_then(nearest).filter(|(_, distance)| *distance < 10.0).map(|(i, _)| i);
    if dragging.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if hovered.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    for (i, point) in pts.iter().enumerate() {
        let center = to_screen(*point);
        let selected = dragging == Some(i) || (dragging.is_none() && hovered == Some(i));
        p.circle_filled(center, if selected { 5.5 } else { 4.0 }, color);
        if selected {
            p.circle_stroke(center, 5.5, Stroke::new(1.0, t.canvas));
        }
    }
    if let Some(point) = dragging.and_then(|i| pts.get(i)) {
        p.text(
            r.left_top() + vec2(6.0, 4.0),
            egui::Align2::LEFT_TOP,
            format!("{} / {}", (point.x * 255.0).round(), (point.y * 255.0).round()),
            t.font(11.0),
            t.text_dim,
        );
    }
    let mut edit = Edit::default();
    if response.double_clicked()
        && let Some(q) = response.interact_pointer_pos()
        && let Some((i, distance)) = nearest(q)
        && distance < 10.0
        && i != 0
        && i + 1 < pts.len()
    {
        let mut points = pts.clone();
        points.remove(i);
        edit.points = Some(points);
    } else if response.drag_started()
        && let Some(q) = ui.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos())
    {
        edit.started = true;
        match nearest(q) {
            Some((i, distance)) if distance < 10.0 => dragging = Some(i),
            _ if pts.len() < 64 => {
                let point = from_screen(q);
                let mut points = pts.clone();
                let i = points.partition_point(|p| p.x <= point.x);
                points.insert(i, point);
                dragging = Some(i);
                edit.points = Some(points);
            }
            _ => {}
        }
    } else if response.dragged()
        && let (Some(i), Some(q)) = (dragging, response.interact_pointer_pos())
    {
        let mut point = from_screen(q);
        let mut points = pts.clone();
        let left = i.checked_sub(1).and_then(|i| points.get(i)).map(|p| p.x);
        let right = points.get(i.saturating_add(1)).map(|p| p.x);
        let (lo, hi) = match (left, right) {
            (Some(left), Some(right)) => {
                let gap = ((right - left) * 0.25).clamp(0.0, 0.01);
                (left + gap, right - gap)
            }
            (None, Some(right)) => (0.0, (right - 0.01).max(0.0)),
            (Some(left), None) => ((left + 0.01).min(1.0), 1.0),
            _ => (0.0, 1.0),
        };
        if lo <= hi {
            point.x = point.x.clamp(lo, hi);
        }
        if let Some(current) = points.get_mut(i) {
            *current = point;
            edit.points = Some(points);
        }
    } else if response.clicked()
        && pts.len() < 64
        && let Some(q) = response.interact_pointer_pos()
        && nearest(q).is_none_or(|(_, distance)| distance >= 10.0)
    {
        let point = from_screen(q);
        let mut points = pts.clone();
        let i = points.partition_point(|p| p.x <= point.x);
        points.insert(i, point);
        edit.points = Some(points);
    }
    if response.drag_stopped() {
        dragging = None;
        edit.stopped = true;
    }
    ui.data_mut(|d| d.insert_temp(drag_id, dragging));
    edit
}
