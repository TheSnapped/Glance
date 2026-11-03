//! Preferences in %APPDATA%\Glance\settings.ini, and resume positions in positions.txt.

use eframe::egui::Color32;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq)]
pub enum EndAction {
    Stop,
    Loop,
    Next,
}

#[derive(Clone, PartialEq)]
pub struct Settings {
    pub volume: f32,
    pub muted: bool,
    pub end: EndAction,
    pub size: [f32; 2],
    pub pos: Option<[f32; 2]>,
    pub maximized: bool,
    pub played: Color32,
    pub track: Color32,
    pub handle: Color32,
    pub thumbnails: bool,
    pub resume: bool,
    pub seek_step: f32,
    pub volume_step: f32,
    pub hide_after: f32,
    pub remaining: bool,
    pub on_top: bool,
    /// The Kerf video editor's exe, once linked: enables "Open in Kerf".
    pub kerf: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            volume: 0.8,
            muted: false,
            end: EndAction::Stop,
            size: [960.0, 540.0],
            pos: None,
            maximized: false,
            played: Color32::from_rgb(80, 170, 255),
            track: Color32::from_white_alpha(60),
            handle: Color32::from_gray(235),
            thumbnails: true,
            resume: true,
            seek_step: 5.0,
            volume_step: 5.0,
            hide_after: 2.0,
            remaining: false,
            on_top: false,
            kerf: None,
        }
    }
}

fn dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Glance"))
}

fn write(name: &str, text: &str) {
    if let Some(d) = dir() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join(name), text);
    }
}

fn read(name: &str) -> String {
    dir().and_then(|d| std::fs::read_to_string(d.join(name)).ok()).unwrap_or_default()
}

impl Settings {
    /// Resets what the settings window changes, except the Kerf link (window placement, volume etc. excluded).
    pub fn reset_preferences(&mut self) {
        let d = Settings::default();
        let keep = Settings { volume: self.volume, muted: self.muted, size: self.size, pos: self.pos, maximized: self.maximized, kerf: self.kerf.take(), ..d };
        *self = keep;
    }

    pub fn load() -> Self {
        let mut s = Settings::default();
        let (mut x, mut y) = (None, None);
        for line in read("settings.ini").lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            let num = |lo: f32, hi: f32, cur: f32| v.parse::<f32>().ok().filter(|n| n.is_finite()).map_or(cur, |n| n.clamp(lo, hi));
            let color = |cur: Color32| Color32::from_hex(v).unwrap_or(cur);
            let flag = v == "true";
            match k.trim() {
                "volume" => s.volume = num(0.0, 1.0, s.volume),
                "muted" => s.muted = flag,
                "end" => s.end = match v { "loop" => EndAction::Loop, "next" => EndAction::Next, _ => EndAction::Stop },
                "width" => s.size[0] = num(320.0, 10000.0, s.size[0]),
                "height" => s.size[1] = num(180.0, 10000.0, s.size[1]),
                "x" => x = v.parse::<f32>().ok(),
                "y" => y = v.parse::<f32>().ok(),
                "maximized" => s.maximized = flag,
                "played" => s.played = color(s.played),
                "track" => s.track = color(s.track),
                "handle" => s.handle = color(s.handle),
                "thumbnails" => s.thumbnails = flag,
                "resume" => s.resume = flag,
                "seek_step" => s.seek_step = num(1.0, 120.0, s.seek_step),
                "volume_step" => s.volume_step = num(1.0, 25.0, s.volume_step),
                "hide_after" => s.hide_after = num(0.5, 30.0, s.hide_after),
                "remaining" => s.remaining = flag,
                "on_top" => s.on_top = flag,
                "kerf" if !v.is_empty() => s.kerf = Some(PathBuf::from(v)),
                _ => {}
            }
        }
        s.pos = x.zip(y).map(|(x, y)| [x, y]);
        s
    }

    pub fn save(&self) {
        let end = match self.end {
            EndAction::Stop => "stop",
            EndAction::Loop => "loop",
            EndAction::Next => "next",
        };
        let mut text = format!(
            "volume={}\nmuted={}\nend={end}\nwidth={}\nheight={}\nmaximized={}\nplayed={}\ntrack={}\nhandle={}\n\
             thumbnails={}\nresume={}\nseek_step={}\nvolume_step={}\nhide_after={}\nremaining={}\non_top={}\n",
            self.volume,
            self.muted,
            self.size[0],
            self.size[1],
            self.maximized,
            self.played.to_hex(),
            self.track.to_hex(),
            self.handle.to_hex(),
            self.thumbnails,
            self.resume,
            self.seek_step,
            self.volume_step,
            self.hide_after,
            self.remaining,
            self.on_top,
        );
        if let Some([x, y]) = self.pos {
            text += &format!("x={x}\ny={y}\n");
        }
        if let Some(k) = &self.kerf {
            text += &format!("kerf={}\n", k.display());
        }
        write("settings.ini", &text);
    }
}

/// Where playback of `file` stopped last time, if it was left part-way through.
pub fn resume_position(file: &Path) -> Option<f64> {
    let key = file.to_string_lossy();
    read("positions.txt").lines().find_map(|l| l.split_once('\t').filter(|(_, p)| *p == key)?.0.parse().ok())
}

/// Remembers (or with `None` forgets) where playback of `file` stopped. Keeps the 200 most recent.
pub fn store_position(file: &Path, pos: Option<f64>) {
    let key = file.to_string_lossy();
    let old = read("positions.txt");
    if pos.is_none() && !old.contains(&*key) {
        return;
    }
    let mut lines: Vec<String> = pos.map(|p| format!("{p:.1}\t{key}")).into_iter().collect();
    lines.extend(old.lines().filter(|l| l.split_once('\t').is_some_and(|(_, p)| p != key)).take(199).map(String::from));
    write("positions.txt", &(lines.join("\n") + "\n"));
}
