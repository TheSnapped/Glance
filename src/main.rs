#![windows_subsystem = "windows"]
//! Glance: a lightweight video player. `glance.exe <file>` plays a file;
//! `--register` / `--unregister` add or remove it as a Windows video player.

mod player;
mod probe;
mod register;
mod settings;
mod video;
mod widgets;

use eframe::egui::{self, pos2, vec2, Align2, Color32, CursorIcon, FontId, Key, Modifiers, Rect, Sense, TextureHandle, ViewportCommand};
use player::{Engine, Frame};
use probe::{Info, Thumbnailer};
use settings::{EndAction, Settings};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use widgets::*;

const SPEEDS: &[f64] = &[0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 3.0];
/// Height of the control overlay at the bottom of the window.
const BAR: f32 = 88.0;

fn main() -> eframe::Result {
    let arg = std::env::args_os().nth(1);
    match arg.as_ref().and_then(|a| a.to_str()) {
        Some("--register") => {
            match register::register() {
                Ok(()) => {
                    register::open_default_apps();
                    register::message("Glance is now registered as a video player.\n\nIn Default apps, click \"Video player\" and choose Glance.");
                }
                Err(e) => register::message(&format!("Registration failed: {e}")),
            }
            return Ok(());
        }
        Some("--unregister") => {
            register::unregister();
            register::message("Glance's file associations were removed.");
            return Ok(());
        }
        _ => {}
    }

    let settings = Settings::load();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Glance")
        .with_inner_size(settings.size)
        .with_min_inner_size([320.0, 180.0])
        .with_maximized(settings.maximized)
        .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/glance.png")).expect("valid icon"));
    if let Some(pos) = settings.pos {
        viewport = viewport.with_position(pos);
    }
    if settings.on_top {
        viewport = viewport.with_always_on_top();
    }
    // No vsync: NVIDIA's OpenGL driver busy-waits for it, burning a core. Redraws are
    // instead timed to each frame's due time; in a window, DWM composition prevents tearing.
    // A 1 ms timer resolution makes those timed wake-ups accurate.
    unsafe { windows_sys::Win32::Media::timeBeginPeriod(1) };
    let options = eframe::NativeOptions { viewport, renderer: eframe::Renderer::Glow, vsync: false, ..Default::default() };
    let path = arg.map(PathBuf::from);
    eframe::run_native("Glance", options, Box::new(move |cc| Ok(Box::new(App::new(cc, path, settings)))))
}

#[derive(Default)]
struct Gpu {
    renderer: Option<video::Renderer>,
    pending: Option<Frame>,
    clear: bool,
}

struct Scrub {
    t: f64,
    was_paused: bool,
    last_seek: Instant,
}

/// Timeline preview images for the open file, decoded on demand and cached per time bucket.
#[derive(Default)]
struct Thumbs {
    source: Option<Thumbnailer>,
    cache: HashMap<u32, TextureHandle>,
    requested: Option<u32>,
    latest: Option<TextureHandle>,
}

struct App {
    engine: Engine,
    gpu: Arc<Mutex<Gpu>>,
    settings: Settings,
    path: Option<PathBuf>,
    /// Videos in the current folder, natural-sorted (scanned on first next/previous).
    playlist: Option<Vec<PathBuf>>,
    pos: f64,
    speed: f64,
    ended: bool,
    need_preview: bool,
    scrub: Option<Scrub>,
    thumbs: Thumbs,
    info: Option<Info>,
    show_settings: bool,
    show_info: bool,
    on_top: bool,
    last_activity: Instant,
    /// Wheel movement not yet turned into volume steps.
    scroll: f32,
    osd: Option<(String, Instant)>,
    error: Option<String>,
}

impl App {
    fn new(cc: &eframe::CreationContext, path: Option<PathBuf>, settings: Settings) -> Self {
        let ctx = cc.egui_ctx.clone();
        let mut gpu = Gpu::default();
        let mut error = None;
        match cc.gl.as_ref().map(|gl| video::Renderer::new(gl)) {
            Some(Ok(r)) => gpu.renderer = Some(r),
            Some(Err(e)) => error = Some(format!("OpenGL setup failed: {e}")),
            None => error = Some("OpenGL is not available".into()),
        }
        let mut app = App {
            engine: Engine::new(move || ctx.request_repaint()),
            gpu: Arc::new(Mutex::new(gpu)),
            on_top: settings.on_top,
            settings,
            path: None,
            playlist: None,
            pos: 0.0,
            speed: 1.0,
            ended: false,
            need_preview: false,
            scrub: None,
            thumbs: Thumbs::default(),
            info: None,
            show_settings: false,
            show_info: false,
            last_activity: Instant::now(),
            scroll: 0.0,
            osd: None,
            error,
        };
        app.apply_gain();
        if let Some(p) = path {
            app.open(&cc.egui_ctx, p);
        }
        app
    }

    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.remember_position();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        ctx.send_viewport_cmd(ViewportCommand::Title(format!("{name} - Glance")));
        if self.path.as_ref().and_then(|p| p.parent()) != path.parent() {
            self.playlist = None;
        }
        let start = if self.settings.resume { settings::resume_position(&path) } else { None };
        self.engine.open(path.clone(), start.unwrap_or(0.0));
        self.engine.set_paused(false);
        if let Some(t) = start {
            self.show_osd(&format!("Resumed at {} (Home restarts)", fmt_time(t, t)));
        }
        self.path = Some(path);
        self.pos = start.unwrap_or(0.0);
        self.ended = false;
        self.need_preview = true;
        self.scrub = None;
        self.thumbs = Thumbs::default();
        self.info = None;
        self.error = None;
        let mut gpu = self.gpu.lock().unwrap();
        gpu.pending = None;
        gpu.clear = true;
    }

    /// Saves where the current video was left, for "resume where I left off".
    fn remember_position(&self) {
        let (Some(path), true) = (&self.path, self.settings.resume) else { return };
        let d = self.engine.duration();
        let partway = d > 60.0 && self.pos > 10.0 && self.pos < d - 15.0 && !self.ended;
        settings::store_position(path, partway.then_some(self.pos));
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new().add_filter("Videos", register::EXTENSIONS).add_filter("All files", &["*"]);
        if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(p) = dialog.pick_file() {
            self.open(ctx, p);
        }
    }

    /// Opens the next (`dir` = 1) or previous (-1) video in the folder; false if there is none.
    fn step(&mut self, ctx: &egui::Context, dir: isize) -> bool {
        let Some(cur) = self.path.clone() else { return false };
        let list = self.playlist.get_or_insert_with(|| folder_videos(&cur));
        let next = list.iter().position(|p| p == &cur).and_then(|i| list.get(i.checked_add_signed(dir)?)).cloned();
        let Some(next) = next else {
            self.show_osd(if dir > 0 { "Last video in folder" } else { "First video in folder" });
            return false;
        };
        self.open(ctx, next);
        true
    }

    fn seek(&mut self, t: f64, accurate: bool) {
        let t = t.clamp(0.0, self.engine.duration().max(0.0));
        self.engine.seek(t, accurate);
        self.pos = t;
        self.ended = false;
        self.need_preview = true;
    }

    fn toggle_pause(&mut self) {
        if self.path.is_none() {
            return;
        }
        if self.ended {
            self.seek(0.0, true);
            self.engine.set_paused(false);
        } else {
            self.engine.set_paused(!self.engine.paused());
        }
    }

    fn set_volume(&mut self, v: f32) {
        self.settings.volume = v.clamp(0.0, 1.0);
        self.settings.muted = false;
        self.apply_gain();
        self.show_osd(&format!("Volume {:.0}%", self.settings.volume * 100.0));
    }

    fn toggle_mute(&mut self) {
        self.settings.muted = !self.settings.muted;
        self.apply_gain();
        self.show_osd(if self.settings.muted { "Muted" } else { "Unmuted" });
    }

    fn apply_gain(&self) {
        // Squared so the slider feels even to the ear.
        let v = if self.settings.muted { 0.0 } else { self.settings.volume };
        self.engine.set_gain(v * v);
    }

    fn toggle_loop(&mut self) {
        let looping = self.settings.end != EndAction::Loop;
        self.settings.end = if looping { EndAction::Loop } else { EndAction::Stop };
        self.show_osd(if looping { "Loop on" } else { "Loop off" });
    }

    /// Steps the playback speed up (1), down (-1) or back to normal (0).
    fn change_speed(&mut self, dir: i32) {
        let i = SPEEDS.iter().position(|&s| s >= self.speed - 1e-6).unwrap_or(3) as i32;
        let speed = if dir == 0 { 1.0 } else { SPEEDS[(i + dir).clamp(0, SPEEDS.len() as i32 - 1) as usize] };
        if speed != self.speed {
            self.speed = speed;
            self.engine.set_speed(speed, self.pos);
            self.need_preview = true;
        }
        self.show_osd(&format!("Speed {}", fmt_speed(self.speed)));
    }

    fn set_fullscreen(&self, ctx: &egui::Context, on: bool) {
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(on));
    }

    fn register_default(&mut self) {
        match register::register() {
            Ok(()) => {
                register::open_default_apps();
                self.show_osd("Registered: pick Glance under \"Video player\"");
            }
            Err(e) => self.show_osd(&format!("Registration failed: {e}")),
        }
    }

    /// Hands the video to the linked Kerf editor at the current position, and pauses here.
    fn open_in_kerf(&mut self) {
        let (Some(kerf), Some(path)) = (self.settings.kerf.clone(), self.path.clone()) else { return };
        let launched = std::process::Command::new(&kerf).arg(&path).arg("--at").arg(format!("{:.3}", self.pos)).current_dir(kerf.parent().unwrap_or(Path::new("."))).spawn();
        match launched {
            Ok(_) => {
                self.engine.set_paused(true);
                self.show_osd(&format!("Opened in Kerf at {}", fmt_time(self.pos, self.engine.duration())));
            }
            Err(e) => self.show_osd(&format!("Can't start Kerf ({e}): relink it in Settings (#)")),
        }
    }

    fn show_osd(&mut self, text: &str) {
        self.osd = Some((text.to_owned(), Instant::now()));
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return; // typing into a settings field
        }
        let key = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
        let ctrl = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, k));
        // Matched as typed text so they work on any layout (German: # has its own key,
        // [ and ] need AltGr).
        let typed = |c: &str| ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if t == c)));
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        let step = self.settings.seek_step as f64;
        let vstep = self.settings.volume_step / 100.0;

        if typed("#") {
            self.show_settings = !self.show_settings;
        }
        if key(Key::I) {
            self.show_info = !self.show_info;
        }
        if key(Key::E) {
            self.open_in_kerf();
        }
        if key(Key::Escape) {
            if self.show_settings || self.show_info {
                (self.show_settings, self.show_info) = (false, false);
            } else if fullscreen {
                self.set_fullscreen(ctx, false);
            }
        }
        if key(Key::Space) || key(Key::K) {
            self.toggle_pause();
        }
        for (pressed, delta) in [(ctrl(Key::ArrowLeft), -6.0 * step), (ctrl(Key::ArrowRight), 6.0 * step), (key(Key::ArrowLeft), -step), (key(Key::ArrowRight), step)] {
            if pressed {
                self.seek(self.pos + delta, true);
            }
        }
        if key(Key::Home) {
            self.seek(0.0, true);
        }
        if key(Key::ArrowUp) {
            self.set_volume(self.settings.volume + vstep);
        }
        if key(Key::ArrowDown) {
            self.set_volume(self.settings.volume - vstep);
        }
        if key(Key::M) {
            self.toggle_mute();
        }
        if key(Key::F) || key(Key::F11) || key(Key::Enter) {
            self.set_fullscreen(ctx, !fullscreen);
        }
        if key(Key::PageDown) || key(Key::N) {
            self.step(ctx, 1);
        }
        if key(Key::PageUp) || key(Key::P) {
            self.step(ctx, -1);
        }
        if key(Key::L) {
            self.toggle_loop();
        }
        if typed("]") || typed("+") {
            self.change_speed(1);
        }
        if typed("[") || typed("-") {
            self.change_speed(-1);
        }
        if key(Key::Backspace) {
            self.change_speed(0);
        }
        if ctrl(Key::O) {
            self.open_dialog(ctx);
        }
    }

    fn remember_window(&mut self, ctx: &egui::Context) {
        ctx.input(|i| {
            let vp = i.viewport();
            let (max, full) = (vp.maximized.unwrap_or(false), vp.fullscreen.unwrap_or(false));
            if !full {
                self.settings.maximized = max;
            }
            if !max && !full && !vp.minimized.unwrap_or(false) {
                if let Some(r) = vp.inner_rect {
                    self.settings.size = [r.width(), r.height()];
                }
                if let Some(r) = vp.outer_rect {
                    self.settings.pos = Some([r.min.x, r.min.y]);
                }
            }
        });
    }

    /// Collects finished thumbnails and returns the best one for time `t` (requesting it if needed).
    fn thumbnail(&mut self, ctx: &egui::Context, t: f64) -> Option<TextureHandle> {
        let path = self.path.clone()?;
        let th = &mut self.thumbs;
        let source = th.source.get_or_insert_with(|| {
            let ctx = ctx.clone();
            Thumbnailer::new(path, move || ctx.request_repaint())
        });
        // Buckets: about one preview per 0.5% of the video, at least one per second.
        let step = (self.engine.duration() / 200.0).max(1.0);
        if let Some((at, image)) = source.take() {
            let tex = ctx.load_texture("thumb", image, egui::TextureOptions::LINEAR);
            if th.cache.len() > 300 {
                th.cache.clear();
            }
            th.cache.insert((at / step) as u32, tex.clone());
            th.latest = Some(tex);
        }
        let bucket = (t / step) as u32;
        if let Some(tex) = th.cache.get(&bucket) {
            return Some(tex.clone());
        }
        if th.requested != Some(bucket) {
            th.requested = Some(bucket);
            source.request(((bucket as f64 + 0.5) * step).min(self.engine.duration() - 0.05).max(0.0));
        }
        th.latest.clone()
    }

    /// Draws the bottom overlay: timeline with previews, centred transport, and the side controls.
    fn controls(&mut self, ui: &mut egui::Ui, full: Rect) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        gradient(&painter, Rect::from_min_max(pos2(full.left(), full.bottom() - BAR), full.max));
        let duration = self.engine.duration();
        let (white, dim) = (Color32::from_gray(235), Color32::from_gray(170));

        // Timeline.
        let line = Rect::from_min_max(pos2(full.left() + 16.0, full.bottom() - 62.0), pos2(full.right() - 16.0, full.bottom() - 44.0));
        let resp = ui.interact(line, ui.id().with("timeline"), Sense::click_and_drag());
        let to_time = |x: f32| ((x - line.left()) / line.width()).clamp(0.0, 1.0) as f64 * duration;
        if let Some(p) = resp.interact_pointer_pos() {
            let t = to_time(p.x);
            let engine = &self.engine;
            let scrub = self.scrub.get_or_insert_with(|| {
                let was_paused = engine.paused();
                engine.set_paused(true);
                Scrub { t, was_paused, last_seek: Instant::now() - Duration::from_secs(1) }
            });
            scrub.t = t;
            if scrub.last_seek.elapsed() > Duration::from_millis(70) {
                scrub.last_seek = Instant::now();
                self.engine.seek(t, false);
                self.need_preview = true;
            }
            self.pos = t;
        } else if let Some(sc) = self.scrub.take() {
            self.seek(sc.t, true);
            self.engine.set_paused(sc.was_paused);
        }
        let hot = resp.hovered() || self.scrub.is_some();
        let track = Rect::from_center_size(line.center(), vec2(line.width(), if hot { 6.0 } else { 4.0 }));
        let frac = if duration > 0.0 { (self.pos / duration).clamp(0.0, 1.0) as f32 } else { 0.0 };
        let played_x = track.left() + track.width() * frac;
        let s = &self.settings;
        painter.rect_filled(track, 3.0, s.track);
        painter.rect_filled(Rect::from_min_max(track.min, pos2(played_x, track.bottom())), 3.0, s.played);
        if hot {
            painter.circle_filled(pos2(played_x, track.center().y), 7.0, s.handle);
        }
        if let Some(p) = resp.hover_pos().or(resp.interact_pointer_pos()).filter(|_| hot && duration > 0.0) {
            let t = to_time(p.x);
            let time = fmt_time(t, duration);
            let thumb = if self.settings.thumbnails { self.thumbnail(&ctx, t) } else { None };
            match thumb {
                Some(tex) => {
                    let size = tex.size_vec2() * (200.0 / tex.size_vec2().x).min(130.0 / tex.size_vec2().y);
                    let x = p.x.clamp(full.left() + size.x / 2.0 + 8.0, full.right() - size.x / 2.0 - 8.0);
                    let img = Rect::from_center_size(pos2(x, line.top() - 30.0 - size.y / 2.0), size);
                    painter.rect_filled(img.expand(2.0), 6.0, Color32::from_black_alpha(220));
                    painter.image(tex.id(), img, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    label(&painter, pos2(x, img.bottom() + 14.0), time, 13.0);
                }
                None => {
                    label(&painter, pos2(p.x.clamp(line.left() + 24.0, line.right() - 24.0), line.top() - 8.0), time, 13.0);
                }
            }
        }

        // Transport, centred.
        let cy = full.bottom() - 22.0;
        let cx = full.center().x;
        if icon_button(ui, &painter, pos2(cx - 50.0, cy), 16.0, "prev", |p, c, col| draw_skip(p, c, col, -1.0)).clicked() {
            self.step(&ctx, -1);
        }
        let paused = self.engine.paused() || self.ended;
        painter.circle_filled(pos2(cx, cy), 19.0, Color32::from_white_alpha(22));
        if icon_button(ui, &painter, pos2(cx, cy), 19.0, "play", |p, c, col| if paused { draw_play(p, c + vec2(1.5, 0.0), col, 1.25) } else { draw_pause(p, c, col, 1.15) }).clicked() {
            self.toggle_pause();
        }
        if icon_button(ui, &painter, pos2(cx + 50.0, cy), 16.0, "next", |p, c, col| draw_skip(p, c, col, 1.0)).clicked() {
            self.step(&ctx, 1);
        }

        // Time on the left; click toggles elapsed/remaining.
        let time = if self.settings.remaining {
            format!("{} / -{}", fmt_time(self.pos, duration), fmt_time(duration - self.pos, duration))
        } else {
            format!("{} / {}", fmt_time(self.pos, duration), fmt_time(duration, duration))
        };
        let galley = painter.layout_no_wrap(time, FontId::proportional(14.0), white);
        let trect = Rect::from_min_size(pos2(full.left() + 18.0, cy - galley.size().y / 2.0), galley.size());
        painter.galley(trect.min, galley, white);
        if ui.interact(trect, ui.id().with("time"), Sense::click()).clicked() {
            self.settings.remaining = !self.settings.remaining;
        }

        // Right side, from the edge inwards; less important items drop out in small windows.
        let roomy = full.width() >= 640.0;
        let mut x = full.right() - 26.0;
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if icon_button(ui, &painter, pos2(x, cy), 16.0, "full", |p, c, col| draw_fullscreen(p, c, col, fullscreen)).clicked() {
            self.set_fullscreen(&ctx, !fullscreen);
        }
        if full.width() >= 480.0 {
            x -= 112.0;
            let vol = Rect::from_min_size(pos2(x, cy - 8.0), vec2(80.0, 16.0));
            let vresp = ui.interact(vol, ui.id().with("volume"), Sense::click_and_drag());
            if let Some(p) = vresp.interact_pointer_pos() {
                self.set_volume((p.x - vol.left()) / vol.width());
            }
            let level = if self.settings.muted { 0.0 } else { self.settings.volume };
            let vtrack = Rect::from_center_size(vol.center(), vec2(vol.width(), 4.0));
            painter.rect_filled(vtrack, 2.0, Color32::from_white_alpha(60));
            painter.rect_filled(Rect::from_min_max(vtrack.min, pos2(vtrack.left() + vtrack.width() * level, vtrack.bottom())), 2.0, white);
            painter.circle_filled(pos2(vtrack.left() + vtrack.width() * level, vtrack.center().y), if vresp.hovered() { 6.0 } else { 5.0 }, white);
            x -= 20.0;
        } else {
            x -= 36.0;
        }
        let level = if self.settings.muted { 0.0 } else { self.settings.volume };
        if icon_button(ui, &painter, pos2(x, cy), 16.0, "mute", |p, c, col| draw_speaker(p, c, col, level)).clicked() {
            self.toggle_mute();
        }
        if roomy {
            x -= 44.0;
            let looping = self.settings.end == EndAction::Loop;
            if text_button(ui, &painter, pos2(x, cy), "loop", "LOOP", if looping { self.settings.played } else { dim }).clicked() {
                self.toggle_loop();
            }
            x -= 48.0;
            let sresp = text_button(ui, &painter, pos2(x, cy), "speed", &fmt_speed(self.speed), if self.speed != 1.0 { self.settings.played } else { dim });
            if sresp.clicked() {
                self.change_speed(if self.speed >= 2.0 { 0 } else { 1 });
            }
            if sresp.secondary_clicked() {
                self.change_speed(0);
            }
            x -= 40.0;
            if icon_button(ui, &painter, pos2(x, cy), 16.0, "info", draw_info).on_hover_text("Media info (I)").clicked() {
                self.show_info = !self.show_info;
            }
            if self.settings.kerf.is_some() {
                x -= 44.0;
                if text_button(ui, &painter, pos2(x, cy), "kerf", "Kerf", white).on_hover_text("Open in Kerf at this moment (E)").clicked() {
                    self.open_in_kerf();
                }
            }
        }
    }

    fn draw_video(&self, ui: &egui::Ui, full: Rect) {
        let aspect = self.engine.aspect().max(0.01);
        let mut size = vec2(full.width(), full.width() / aspect);
        if size.y > full.height() {
            size = vec2(full.height() * aspect, full.height());
        }
        let gpu = self.gpu.clone();
        let callback = eframe::egui_glow::CallbackFn::new(move |_info, painter| {
            let mut guard = gpu.lock().unwrap_or_else(|e| e.into_inner());
            let g = &mut *guard;
            if let Some(r) = g.renderer.as_mut() {
                if std::mem::take(&mut g.clear) {
                    r.has_frame = false;
                }
                if let Some(f) = g.pending.take() {
                    r.upload(painter.gl(), &f);
                }
                r.paint(painter.gl());
            }
        });
        ui.painter().add(egui::PaintCallback { rect: Rect::from_center_size(full.center(), size), callback: Arc::new(callback) });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        self.keyboard(ctx);
        // Mouse wheel over the video: one volume step per notch (touchpads accumulate).
        // Over the settings/info windows the wheel scrolls them instead.
        let over_video = ctx.pointer_hover_pos().and_then(|p| ctx.layer_id_at(p)).is_none_or(|l| l.order == egui::Order::Background);
        self.scroll += ctx.input(|i| i.raw_scroll_delta.y);
        if !over_video || self.path.is_none() {
            self.scroll = 0.0;
        }
        while self.scroll.abs() >= 40.0 {
            let dir = self.scroll.signum();
            self.scroll -= dir * 40.0;
            self.set_volume(self.settings.volume + dir * self.settings.volume_step / 100.0);
        }

        let (frame, status) = self.engine.tick(self.need_preview);
        if let Some(f) = frame {
            self.need_preview = false;
            self.gpu.lock().unwrap().pending = Some(f);
        }
        if let (Some(t), None) = (status.now, &self.scrub) {
            self.pos = t;
        }
        if status.ended && !self.ended && self.scrub.is_none() {
            let end = self.settings.end;
            if end == EndAction::Loop {
                self.seek(0.0, true);
            } else if !(end == EndAction::Next && self.step(ctx, 1)) {
                self.ended = true;
                self.pos = self.engine.duration();
                self.engine.set_paused(true);
            }
        }
        if self.error.is_none() {
            self.error = self.engine.error();
        }
        if self.info.is_none() && self.show_info {
            self.info = self.engine.info();
        }
        self.remember_window(ctx);
        if self.on_top != self.settings.on_top {
            self.on_top = self.settings.on_top;
            let level = if self.on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(level));
        }

        if ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO || i.pointer.any_down() || i.raw_scroll_delta != egui::Vec2::ZERO) {
            self.last_activity = now;
        }
        let paused = self.engine.paused();
        let hide_after = Duration::from_secs_f32(self.settings.hide_after);
        let idle = now - self.last_activity;
        let panels = self.show_settings || self.show_info;

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(Color32::BLACK)).show(ctx, |ui| {
            let full = ui.max_rect();
            let over_bar = ctx.pointer_hover_pos().is_some_and(|p| p.y > full.bottom() - BAR);
            let show = self.path.is_none() || paused || self.ended || self.scrub.is_some() || panels || over_bar || idle < hide_after;

            let surface = ui.interact(full, ui.id().with("video"), Sense::click());
            if surface.double_clicked() {
                let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                self.set_fullscreen(ctx, !fs);
            } else if surface.clicked() {
                if panels {
                    (self.show_settings, self.show_info) = (false, false);
                } else {
                    self.toggle_pause();
                }
            }

            if self.path.is_some() {
                self.draw_video(ui, full);
            }
            let painter = ui.painter().clone();
            if let Some(e) = &self.error {
                painter.text(full.center(), Align2::CENTER_CENTER, e, FontId::proportional(16.0), Color32::from_rgb(255, 120, 110));
            } else if self.path.is_none() {
                empty_screen(self, ui, full);
            }
            if let Some((text, at)) = &self.osd {
                let left = Duration::from_millis(1500).saturating_sub(at.elapsed());
                if left.is_zero() {
                    self.osd = None;
                } else {
                    let galley = painter.layout_no_wrap(text.clone(), FontId::proportional(18.0), Color32::WHITE);
                    let bg = Rect::from_min_size(full.min + vec2(16.0, 16.0), galley.size() + vec2(20.0, 12.0));
                    painter.rect_filled(bg, 6.0, Color32::from_black_alpha(170));
                    painter.galley(bg.min + vec2(10.0, 6.0), galley, Color32::WHITE);
                    repaint_in(ctx, left);
                }
            }

            if self.path.is_some() {
                if show {
                    self.controls(ui, full);
                    if idle < hide_after {
                        repaint_in(ctx, hide_after - idle); // to hide the controls
                    }
                } else if ctx.pointer_hover_pos().is_some() {
                    ctx.set_cursor_icon(CursorIcon::None);
                }
            }
        });

        let was_open = self.show_settings;
        if let SettingsAction::RegisterDefault = settings_window(ctx, &mut self.show_settings, &mut self.settings) {
            self.register_default();
        }
        if was_open && !self.show_settings {
            self.settings.save();
        }
        info_window(ctx, &mut self.show_info, self.info.as_ref());

        // Playing: wake up when the next frame is due. Paused: sleep until input.
        if self.path.is_some() && self.error.is_none() {
            if status.now.is_none() || self.need_preview {
                repaint_in(ctx, Duration::from_millis(10));
            } else if !self.engine.paused() {
                repaint_in(ctx, Duration::from_secs_f64(status.next_in.unwrap_or(0.01).min(0.1)));
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.remember_position();
        self.settings.save();
    }
}

/// Schedules a repaint after `delay`. egui shortens every delay by its predicted
/// frame time (~16 ms), which would turn frame-paced waits into a busy loop.
fn repaint_in(ctx: &egui::Context, delay: Duration) {
    let dt = ctx.input(|i| i.predicted_dt);
    ctx.request_repaint_after(delay + Duration::from_secs_f32(dt.clamp(0.0, 0.1)));
}

fn empty_screen(app: &mut App, ui: &mut egui::Ui, full: Rect) {
    let ctx = ui.ctx().clone();
    let c = full.center();
    let painter = ui.painter();
    painter.text(c - vec2(0.0, 50.0), Align2::CENTER_CENTER, "Glance", FontId::proportional(32.0), Color32::from_gray(230));
    painter.text(c - vec2(0.0, 18.0), Align2::CENTER_CENTER, "Open a video to start (Ctrl+O)  -  Settings: #", FontId::proportional(14.0), Color32::from_gray(150));
    if ui.put(Rect::from_center_size(c + vec2(-90.0, 30.0), vec2(160.0, 30.0)), egui::Button::new("Open video...")).clicked() {
        app.open_dialog(&ctx);
    }
    if ui.put(Rect::from_center_size(c + vec2(90.0, 30.0), vec2(160.0, 30.0)), egui::Button::new("Set as default player")).clicked() {
        app.register_default();
    }
}

fn fmt_time(t: f64, total: f64) -> String {
    let s = t.max(0.0) as u64;
    if total >= 3600.0 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn fmt_speed(s: f64) -> String {
    format!("{}x", (s * 100.0).round() / 100.0)
}

fn folder_videos(file: &Path) -> Vec<PathBuf> {
    let is_video = |p: &Path| p.extension().and_then(|e| e.to_str()).is_some_and(|e| register::EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)));
    let mut list: Vec<PathBuf> = file
        .parent()
        .and_then(|d| std::fs::read_dir(d).ok())
        .map(|rd| rd.filter_map(|e| Some(e.ok()?.path())).filter(|p| is_video(p)).collect())
        .unwrap_or_default();
    if !list.iter().any(|p| p == file) {
        list.push(file.to_path_buf());
    }
    list.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
    list
}

/// Compares like Explorer does: "ep2" before "ep10", case-insensitively.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    let number = |it: &mut std::iter::Peekable<std::str::Chars>| {
        let mut s = String::new();
        while let Some(c) = it.next_if(char::is_ascii_digit) {
            s.push(c);
        }
        s.trim_start_matches('0').to_owned()
    };
    loop {
        let ord = match (a.peek(), b.peek()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (na, nb) = (number(&mut a), number(&mut b));
                na.len().cmp(&nb.len()).then_with(|| na.cmp(&nb))
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                a.next();
                b.next();
                ord
            }
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
}
