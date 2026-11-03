//! Things read from a file besides playback: the media-info panel's contents, and
//! timeline preview thumbnails decoded by a second, independent FFmpeg context.

use eframe::egui::ColorImage;
use ffmpeg_next as ff;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

/// Titled sections of label/value rows.
pub type Info = Vec<(String, Vec<(String, String)>)>;

pub fn media_info(ictx: &ff::format::context::Input, path: &Path) -> Info {
    let mut file = vec![
        ("Name".into(), path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
        ("Folder".into(), path.parent().map(|p| p.display().to_string()).unwrap_or_default()),
    ];
    if let Ok(m) = std::fs::metadata(path) {
        file.push(("Size".into(), fmt_bytes(m.len())));
    }
    file.push(("Container".into(), ictx.format().description().to_owned()));
    if ictx.duration() > 0 {
        file.push(("Duration".into(), fmt_duration(ictx.duration() as f64 / 1e6)));
    }
    if ictx.bit_rate() > 0 {
        file.push(("Overall bitrate".into(), fmt_bitrate(ictx.bit_rate())));
    }
    for (k, v) in ictx.metadata().iter() {
        let label = match k.to_ascii_lowercase().as_str() {
            "title" => "Title",
            "artist" => "Artist",
            "album" => "Album",
            "date" | "year" => "Date",
            "creation_time" => "Created",
            "comment" => "Comment",
            "description" => "Description",
            "genre" => "Genre",
            "encoder" => "Encoder",
            "copyright" => "Copyright",
            _ => continue,
        };
        file.push((label.into(), v.to_owned()));
    }
    if ictx.nb_chapters() > 0 {
        file.push(("Chapters".into(), ictx.nb_chapters().to_string()));
    }
    let mut sections = vec![("File".to_owned(), file)];

    let (mut v_n, mut a_n, mut s_n) = (0, 0, 0);
    for s in ictx.streams() {
        let par = s.parameters();
        let codec = codec_name(par.id());
        let mut rows = vec![("Codec".to_owned(), codec)];
        let lang = s.metadata().get("language").filter(|l| *l != "und").map(str::to_owned);
        let title = s.metadata().get("title").map(str::to_owned);
        let bitrate = par.bit_rate();
        let ctx = ff::codec::context::Context::from_parameters(par);
        let kind = match s.parameters().medium() {
            ff::media::Type::Video => {
                let Ok(dec) = ctx.and_then(|c| c.decoder().video()) else { continue };
                let (w, h) = (dec.width(), dec.height());
                rows.push(("Resolution".into(), format!("{w} x {h}")));
                let sar = dec.aspect_ratio();
                if sar.numerator() > 0 && sar.numerator() != sar.denominator() {
                    let dar = w as f64 * f64::from(sar) / h.max(1) as f64;
                    rows.push(("Display aspect".into(), format!("{dar:.3}:1")));
                }
                let fps = s.avg_frame_rate();
                if fps.numerator() > 0 && fps.denominator() > 0 {
                    rows.push(("Frame rate".into(), format!("{:.3} fps", f64::from(fps)).replace(".000", "")));
                }
                rows.push(("Pixel format".into(), dec.format().descriptor().map_or("?", |d| d.name()).to_owned()));
                let hdr = match dec.color_transfer_characteristic() {
                    ff::color::TransferCharacteristic::SMPTE2084 => " (HDR10 / PQ)",
                    ff::color::TransferCharacteristic::ARIB_STD_B67 => " (HDR / HLG)",
                    _ => "",
                };
                let range = if dec.color_range() == ff::color::Range::JPEG { "full" } else { "limited" };
                let space = match dec.color_space() {
                    ff::color::Space::Unspecified => String::new(),
                    sp => format!("{sp:?}, "),
                };
                rows.push(("Color".into(), format!("{space}{range} range{hdr}")));
                v_n += 1;
                "Video"
            }
            ff::media::Type::Audio => {
                let Ok(dec) = ctx.and_then(|c| c.decoder().audio()) else { continue };
                rows.push(("Sample rate".into(), format!("{} Hz", dec.rate())));
                let ch = dec.channels();
                let layout = match ch {
                    1 => "mono".into(),
                    2 => "stereo".into(),
                    6 => "5.1".into(),
                    8 => "7.1".into(),
                    n => format!("{n} channels"),
                };
                rows.push(("Channels".into(), layout));
                a_n += 1;
                "Audio"
            }
            ff::media::Type::Subtitle => {
                s_n += 1;
                "Subtitles"
            }
            _ => continue,
        };
        if bitrate > 0 {
            rows.push(("Bitrate".into(), fmt_bitrate(bitrate)));
        }
        if let Some(l) = lang {
            rows.push(("Language".into(), l));
        }
        if let Some(t) = title {
            rows.push(("Title".into(), t));
        }
        let n = match kind {
            "Video" => v_n,
            "Audio" => a_n,
            _ => s_n,
        };
        sections.push((if n > 1 { format!("{kind} {n}") } else { kind.to_owned() }, rows));
    }
    sections
}

fn codec_name(id: ff::codec::Id) -> String {
    match ff::decoder::find(id) {
        Some(c) if !c.description().is_empty() => format!("{} ({})", c.description(), id.name()),
        _ => id.name().to_owned(),
    }
}

fn fmt_bytes(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.2} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n => format!("{:.0} KB", n as f64 / 1024.0),
    }
}

fn fmt_bitrate(b: i64) -> String {
    if b >= 1_000_000 {
        format!("{:.2} Mb/s", b as f64 / 1e6)
    } else {
        format!("{} kb/s", b / 1000)
    }
}

fn fmt_duration(t: f64) -> String {
    let s = t as u64;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Width of preview thumbnails in pixels.
const THUMB_W: u32 = 240;

#[derive(Default)]
struct ThumbState {
    want: Option<f64>,
    done: Option<(f64, ColorImage)>,
    quit: bool,
}

/// Decodes preview images on a background thread; only the latest request is served.
pub struct Thumbnailer {
    state: Arc<(Mutex<ThumbState>, Condvar)>,
}

impl Thumbnailer {
    pub fn new(path: PathBuf, repaint: impl Fn() + Send + 'static) -> Self {
        let state = Arc::new((Mutex::new(ThumbState::default()), Condvar::new()));
        let s = state.clone();
        std::thread::spawn(move || {
            let _ = thumb_thread(&path, &s, repaint);
        });
        Thumbnailer { state }
    }

    pub fn request(&self, t: f64) {
        self.state.0.lock().unwrap().want = Some(t);
        self.state.1.notify_one();
    }

    pub fn take(&self) -> Option<(f64, ColorImage)> {
        self.state.0.lock().unwrap().done.take()
    }
}

impl Drop for Thumbnailer {
    fn drop(&mut self) {
        // The thread notices and exits by itself; don't wait for a decode in progress.
        self.state.0.lock().unwrap().quit = true;
        self.state.1.notify_one();
    }
}

fn thumb_thread(path: &Path, state: &(Mutex<ThumbState>, Condvar), repaint: impl Fn()) -> Option<()> {
    let mut ictx = ff::format::input(path).ok()?;
    let start = unsafe { (*ictx.as_ptr()).start_time };
    let offset = if start == ff::ffi::AV_NOPTS_VALUE { 0.0 } else { start as f64 / 1e6 };
    let stream = ictx.streams().best(ff::media::Type::Video)?;
    let (index, tb) = (stream.index(), f64::from(stream.time_base()));
    let mut ctx = ff::codec::context::Context::from_parameters(stream.parameters()).ok()?;
    ctx.set_threading(ff::codec::threading::Config { kind: ff::codec::threading::Type::Frame, count: 0 });
    let mut dec = ctx.decoder().video().ok()?;
    let sar = dec.aspect_ratio();
    let sar = if sar.numerator() > 0 && sar.denominator() > 0 { f64::from(sar) } else { 1.0 };
    let aspect = dec.width() as f64 * sar / dec.height().max(1) as f64;
    let th = ((THUMB_W as f64 / aspect).round() as u32).clamp(16, THUMB_W * 2) & !1;
    let mut scaler: Option<ff::software::scaling::Context> = None;
    let (mut packet, mut cur, mut last) = (ff::Packet::empty(), ff::frame::Video::empty(), ff::frame::Video::empty());

    loop {
        let t = {
            let mut s = state.0.lock().unwrap();
            loop {
                if s.quit {
                    return Some(());
                }
                if let Some(t) = s.want.take() {
                    break t;
                }
                s = state.1.wait(s).unwrap();
            }
        };
        // Decode from the keyframe before `t` up to `t` (capped, for very long keyframe intervals).
        let ts = ((t + offset) * 1e6) as i64;
        if ictx.seek(ts, ..ts).is_err() {
            continue;
        }
        dec.flush();
        // Frames nothing else refers to can be skipped until we get close to `t`. Long keyframe
        // intervals are capped by a time budget: then the closest frame so far is shown.
        let set_skip = |dec: &mut ff::decoder::Video, skip| unsafe { (*dec.as_mut_ptr()).skip_frame = skip };
        set_skip(&mut dec, ff::ffi::AVDiscard::AVDISCARD_NONREF);
        let budget = std::time::Instant::now() + std::time::Duration::from_millis(120);
        let mut got = false;
        'read: while std::time::Instant::now() < budget {
            let eof = packet.read(&mut ictx).is_err();
            if eof {
                let _ = dec.send_eof();
            } else if packet.stream() != index || dec.send_packet(&packet).is_err() {
                continue;
            }
            while dec.receive_frame(&mut cur).is_ok() {
                std::mem::swap(&mut cur, &mut last);
                got = true;
                let pts = last.timestamp().or(last.pts()).map_or(t, |p| p as f64 * tb - offset);
                if pts >= t - 0.02 {
                    break 'read;
                }
                if pts >= t - 0.5 {
                    set_skip(&mut dec, ff::ffi::AVDiscard::AVDISCARD_DEFAULT);
                }
            }
            if eof {
                break;
            }
        }
        if !got {
            continue;
        }
        let fits = scaler.as_ref().is_some_and(|s| {
            s.input().format == last.format() && s.input().width == last.width() && s.input().height == last.height()
        });
        if !fits {
            scaler = ff::software::scaling::Context::get(
                last.format(), last.width(), last.height(), ff::format::Pixel::RGBA, THUMB_W, th,
                ff::software::scaling::Flags::AREA,
            )
            .ok();
        }
        let Some(sc) = scaler.as_mut() else { continue };
        let mut rgba = ff::frame::Video::empty();
        if sc.run(&last, &mut rgba).is_err() {
            continue;
        }
        let (stride, row) = (rgba.stride(0), THUMB_W as usize * 4);
        let pixels: Vec<u8> = (0..th as usize).flat_map(|y| &rgba.data(0)[y * stride..y * stride + row]).copied().collect();
        let image = ColorImage::from_rgba_unmultiplied([THUMB_W as usize, th as usize], &pixels);
        state.0.lock().unwrap().done = Some((t, image));
        repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// `GLANCE_SAMPLE=<video> cargo test -- --ignored`
    #[test]
    #[ignore]
    fn info_and_thumbnails() {
        let path = PathBuf::from(std::env::var("GLANCE_SAMPLE").expect("set GLANCE_SAMPLE"));
        ff::init().unwrap();
        let ictx = ff::format::input(&path).unwrap();
        for (title, rows) in media_info(&ictx, &path) {
            println!("[{title}]");
            for (k, v) in rows {
                println!("  {k}: {v}");
            }
        }
        let thumbs = Thumbnailer::new(path, || {});
        for t in [1.0, 7.3, 12.0] {
            let start = Instant::now();
            thumbs.request(t);
            let got = loop {
                if let Some(r) = thumbs.take() {
                    break r;
                }
                assert!(start.elapsed() < Duration::from_secs(5), "no thumbnail for {t}");
                std::thread::sleep(Duration::from_millis(2));
            };
            println!("thumb at {t}: {:?} in {:?}", got.1.size, start.elapsed());
            assert_eq!(got.0, t);
        }
    }
}
