//! Drawing helpers for the overlay controls, and the settings and media-info windows.

use crate::probe::Info;
use crate::settings::{EndAction, Settings};
use eframe::egui::{self, vec2, Color32, FontId, Painter, Pos2, Rect, Response, Sense, Stroke, Ui};

pub fn icon_button(ui: &mut Ui, painter: &Painter, center: Pos2, radius: f32, id: &str, draw: impl FnOnce(&Painter, Pos2, Color32)) -> Response {
    let resp = ui.interact(Rect::from_center_size(center, vec2(radius, radius) * 2.0), ui.id().with(id), Sense::click());
    if resp.hovered() {
        painter.circle_filled(center, radius, Color32::from_white_alpha(28));
    }
    draw(painter, center, if resp.hovered() { Color32::WHITE } else { Color32::from_gray(225) });
    resp
}

pub fn text_button(ui: &mut Ui, painter: &Painter, center: Pos2, id: &str, text: &str, color: Color32) -> Response {
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::proportional(13.0), color);
    let rect = Rect::from_center_size(center, galley.size() + vec2(14.0, 10.0));
    let resp = ui.interact(rect, ui.id().with(id), Sense::click());
    if resp.hovered() {
        painter.rect_filled(rect, 4.0, Color32::from_white_alpha(28));
    }
    painter.galley(rect.center() - galley.size() / 2.0, galley, color);
    resp
}

/// Text on a dark rounded backing, centred on `center`.
pub fn label(painter: &Painter, center: Pos2, text: String, size: f32) -> Rect {
    let galley = painter.layout_no_wrap(text, FontId::proportional(size), Color32::WHITE);
    let rect = Rect::from_center_size(center, galley.size() + vec2(12.0, 6.0));
    painter.rect_filled(rect, 4.0, Color32::from_black_alpha(200));
    painter.galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
    rect
}

pub fn gradient(painter: &Painter, rect: Rect) {
    let mut mesh = egui::Mesh::default();
    let (top, bottom) = (Color32::TRANSPARENT, Color32::from_black_alpha(190));
    for (p, c) in [(rect.left_top(), top), (rect.right_top(), top), (rect.left_bottom(), bottom), (rect.right_bottom(), bottom)] {
        mesh.colored_vertex(p, c);
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    painter.add(mesh);
}

pub fn draw_play(p: &Painter, c: Pos2, col: Color32, s: f32) {
    let pts = vec![c + vec2(-5.0, -8.0) * s, c + vec2(9.0, 0.0) * s, c + vec2(-5.0, 8.0) * s];
    p.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
}

pub fn draw_pause(p: &Painter, c: Pos2, col: Color32, s: f32) {
    for dx in [-4.5, 4.5] {
        p.rect_filled(Rect::from_center_size(c + vec2(dx, 0.0) * s, vec2(4.0, 15.0) * s), 1.0, col);
    }
}

pub fn draw_skip(p: &Painter, c: Pos2, col: Color32, dir: f32) {
    let mut pts = vec![c + vec2(-5.0 * dir, -7.0), c + vec2(5.0 * dir, 0.0), c + vec2(-5.0 * dir, 7.0)];
    if dir < 0.0 {
        pts.reverse(); // keep the winding clockwise
    }
    p.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
    p.rect_filled(Rect::from_center_size(c + vec2(6.5 * dir, 0.0), vec2(2.5, 14.0)), 0.5, col);
}

pub fn draw_speaker(p: &Painter, c: Pos2, col: Color32, level: f32) {
    let body = [(-8.0, -3.0), (-4.0, -3.0), (1.0, -8.0), (1.0, 8.0), (-4.0, 3.0), (-8.0, 3.0)];
    p.add(egui::Shape::convex_polygon(body.iter().map(|&(x, y)| c + vec2(x, y)).collect(), col, Stroke::NONE));
    let stroke = Stroke::new(1.6_f32, col);
    if level <= 0.0 {
        p.line_segment([c + vec2(4.0, -4.0), c + vec2(10.0, 4.0)], stroke);
        p.line_segment([c + vec2(4.0, 4.0), c + vec2(10.0, -4.0)], stroke);
        return;
    }
    for k in 0..if level > 0.5 { 2 } else { 1 } {
        let r = 5.0 + 4.0 * k as f32;
        let arc = (0..=8).map(|i| {
            let a = (i as f32 / 8.0 - 0.5) * 1.6;
            c + vec2(1.0 + r * a.cos(), r * a.sin())
        });
        p.add(egui::Shape::line(arc.collect(), stroke));
    }
}

pub fn draw_fullscreen(p: &Painter, c: Pos2, col: Color32, active: bool) {
    let stroke = Stroke::new(1.8_f32, col);
    // Corners point outwards to enter fullscreen, inwards to leave it.
    let (o, l) = if active { (2.0, 5.0) } else { (7.0, -5.0) };
    for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        let corner = c + vec2(sx, sy) * o;
        p.line_segment([corner, corner + vec2(sx * l, 0.0)], stroke);
        p.line_segment([corner, corner + vec2(0.0, sy * l)], stroke);
    }
}

pub fn draw_info(p: &Painter, c: Pos2, col: Color32) {
    p.circle_stroke(c, 8.0, Stroke::new(1.6_f32, col));
    p.circle_filled(c + vec2(0.0, -3.8), 1.3, col);
    p.rect_filled(Rect::from_center_size(c + vec2(0.0, 2.0), vec2(1.9, 6.0)), 0.5, col);
}

/// What the settings window asks the app to do.
pub enum SettingsAction {
    None,
    RegisterDefault,
}

pub fn settings_window(ctx: &egui::Context, open: &mut bool, s: &mut Settings) -> SettingsAction {
    let mut action = SettingsAction::None;
    egui::Window::new("Settings")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, -20.0))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            section(ui, "Timeline");
            egui::Grid::new("timeline").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
                for (name, color) in [("Played part", &mut s.played), ("Remaining part", &mut s.track), ("Handle", &mut s.handle)] {
                    ui.label(name);
                    egui::color_picker::color_edit_button_srgba(ui, color, egui::color_picker::Alpha::OnlyBlend);
                    ui.end_row();
                }
            });
            ui.checkbox(&mut s.thumbnails, "Show preview images when hovering the timeline");

            section(ui, "Playback");
            ui.horizontal(|ui| {
                ui.label("At the end of a video:");
                ui.selectable_value(&mut s.end, EndAction::Stop, "Stop");
                ui.selectable_value(&mut s.end, EndAction::Loop, "Loop");
                ui.selectable_value(&mut s.end, EndAction::Next, "Play next");
            });
            ui.checkbox(&mut s.resume, "Resume videos where I left off");
            ui.add(egui::Slider::new(&mut s.seek_step, 1.0..=60.0).step_by(1.0).suffix(" s").text("Arrow-key seek step"));
            ui.add(egui::Slider::new(&mut s.volume_step, 1.0..=20.0).step_by(1.0).suffix(" %").text("Volume step"));

            section(ui, "Interface");
            ui.add(egui::Slider::new(&mut s.hide_after, 0.5..=10.0).step_by(0.5).suffix(" s").text("Hide controls after"));
            ui.checkbox(&mut s.remaining, "Show remaining time");
            ui.checkbox(&mut s.on_top, "Keep window on top");

            section(ui, "Kerf video editor");
            match s.kerf.clone() {
                Some(k) => {
                    ui.horizontal(|ui| {
                        ui.label("Linked:");
                        ui.add(egui::Label::new(egui::RichText::new(k.display().to_string()).weak()).truncate());
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Change...").clicked() {
                            s.kerf = pick_kerf(Some(&k)).or(Some(k));
                        }
                        if ui.button("Unlink").clicked() {
                            s.kerf = None;
                        }
                    });
                    ui.weak("\"Kerf\" button in the controls, or E, opens the video in Kerf at the current time.");
                }
                None => {
                    if ui.button("Link Kerf...").clicked() {
                        s.kerf = pick_kerf(None);
                    }
                    ui.weak("Select kerf.exe to get an \"Open in Kerf\" button.");
                }
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Reset to defaults").clicked() {
                    s.reset_preferences();
                }
                if ui.button("Set Glance as default video player").clicked() {
                    action = SettingsAction::RegisterDefault;
                }
            });
            ui.weak("# or Esc closes this window");
        });
    action
}

pub fn info_window(ctx: &egui::Context, open: &mut bool, info: Option<&Info>) {
    egui::Window::new("Media info")
        .open(open)
        .collapsible(false)
        .default_width(380.0)
        .max_height(ctx.screen_rect().height() - 80.0)
        .anchor(egui::Align2::RIGHT_TOP, vec2(-16.0, 16.0))
        .show(ctx, |ui| {
            let Some(info) = info else {
                ui.label("Reading file...");
                return;
            };
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (title, rows) in info {
                    section(ui, title);
                    egui::Grid::new(title).num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                        for (k, v) in rows {
                            ui.weak(k);
                            ui.add(egui::Label::new(v).wrap());
                            ui.end_row();
                        }
                    });
                }
            });
        });
}

/// Asks for kerf.exe, starting in the current link's folder or a likely install location.
fn pick_kerf(current: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    // Kerf usually sits next to Glance, or in its sibling project's dist folder.
    let guesses = [exe_dir.join("kerf.exe"), exe_dir.join("../kerf.exe"), exe_dir.join("../../video-editor/dist/kerf.exe"), exe_dir.join("../video-editor/dist/kerf.exe")];
    let start = current.map(std::path::Path::to_path_buf).or_else(|| guesses.into_iter().find(|p| p.is_file()));
    let mut dialog = rfd::FileDialog::new().set_title("Select kerf.exe").add_filter("Kerf (kerf.exe)", &["exe"]);
    if let Some(dir) = start.as_ref().and_then(|p| p.parent()).and_then(|d| d.canonicalize().ok()) {
        dialog = dialog.set_directory(dir.to_string_lossy().trim_start_matches(['\\', '?']));
    }
    dialog.pick_file()
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(title).strong().size(15.0));
}
