//! Decoding and timing. One thread demuxes and decodes the open file into a
//! video frame queue and an audio sample buffer; the audio device consumes the
//! samples and is the master clock (a wall clock stands in when there is no audio).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use ffmpeg_next as ff;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Decoded frames waiting to be shown. More would only cost memory.
const VIDEO_AHEAD: usize = 8;
/// Hard cap on queued frames when badly interleaved files starve the audio.
const VIDEO_MAX: usize = 48;

/// A decoded picture as tightly packed YUV 4:2:0 planes.
pub struct Frame {
    pub pts: f64,
    pub width: u32,
    pub height: u32,
    pub planes: [Vec<u8>; 3],
    pub bt709: bool,
    pub full_range: bool,
}

#[derive(Clone, Copy)]
enum Clock {
    /// Seeking: nothing decoded at the new position yet.
    Waiting,
    /// Driven by the samples the audio device has consumed (`Queue::audio_pos`).
    Audio,
    /// Media time `pos` at instant `at`, advancing with `speed` unless paused.
    Wall { at: Instant, pos: f64 },
}

struct Queue {
    /// Bumped by every seek and file change; the decoder only pushes data of the current generation.
    gen: u64,
    video: VecDeque<Frame>,
    /// Interleaved samples in the output device's format.
    audio: VecDeque<f32>,
    /// Media time of the next sample in `audio`.
    audio_pos: f64,
    clock: Clock,
    paused: bool,
    speed: f64,
    gain: f32,
    eof: bool,
    /// Output device format, once the audio thread has opened it (None: no device).
    out: Option<(u32, u16)>,
    out_ready: bool,
    duration: f64,
    /// Display aspect ratio (width / height, sample aspect applied).
    aspect: f32,
    info: Option<crate::probe::Info>,
    error: Option<String>,
}

struct Shared {
    q: Mutex<Queue>,
    cv: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.q.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Queue {
    fn now(&self) -> Option<f64> {
        match self.clock {
            Clock::Waiting => None,
            Clock::Audio => Some(self.audio_pos),
            Clock::Wall { pos, .. } if self.paused => Some(pos),
            Clock::Wall { at, pos } => Some(pos + at.elapsed().as_secs_f64() * self.speed),
        }
    }

    fn audio_secs(&self) -> f64 {
        match self.out {
            Some((rate, ch)) => self.audio.len() as f64 / (rate as f64 * ch as f64),
            None => 0.0,
        }
    }

    /// Re-anchors a wall clock so pausing or changing speed doesn't make it jump.
    fn rebase(&mut self) {
        if let (Clock::Wall { .. }, Some(pos)) = (self.clock, self.now()) {
            self.clock = Clock::Wall { at: Instant::now(), pos };
        }
    }
}

/// Jump to `t` (accurately, or to the keyframe before it), decoding audio for `speed`.
struct Seek {
    gen: u64,
    t: f64,
    accurate: bool,
    speed: f64,
}

struct Decoder {
    cmd: Sender<Seek>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub struct Engine {
    shared: Arc<Shared>,
    decoder: Option<Decoder>,
}

/// What the UI needs to know after each tick.
pub struct Status {
    /// Current position (None while a seek is in flight).
    pub now: Option<f64>,
    /// Wall-clock seconds until the next queued frame is due.
    pub next_in: Option<f64>,
    /// Decoded everything and played it out.
    pub ended: bool,
}

impl Engine {
    pub fn new(repaint: impl Fn() + Send + 'static) -> Self {
        ff::init().ok();
        ff::log::set_level(ff::log::Level::Fatal);
        let shared = Arc::new(Shared {
            q: Mutex::new(Queue {
                gen: 0,
                video: VecDeque::new(),
                audio: VecDeque::new(),
                audio_pos: 0.0,
                clock: Clock::Waiting,
                paused: false,
                speed: 1.0,
                gain: 1.0,
                eof: false,
                out: None,
                out_ready: false,
                duration: 0.0,
                aspect: 16.0 / 9.0,
                info: None,
                error: None,
            }),
            cv: Condvar::new(),
        });
        // Opening the audio device takes a while; don't hold up the window for it.
        let s = shared.clone();
        std::thread::spawn(move || audio_thread(s, repaint));
        Engine { shared, decoder: None }
    }

    pub fn open(&mut self, path: PathBuf, start: f64) {
        self.decoder = None; // joins the old thread
        let mut q = self.shared.lock();
        q.gen += 1;
        q.video.clear();
        q.audio.clear();
        q.clock = Clock::Waiting;
        q.eof = false;
        q.duration = 0.0;
        q.info = None;
        q.error = None;
        let first = Seek { gen: q.gen, t: start, accurate: true, speed: q.speed };
        drop(q);
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let (s, st) = (self.shared.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            if let Err(e) = decode(&path, &s, rx, &st, first) {
                s.lock().error = Some(e);
            }
        });
        self.decoder = Some(Decoder { cmd: tx, stop, thread: Some(thread) });
    }

    /// Jumps to `t`. Fast seeks land on the nearest earlier keyframe (for scrubbing).
    pub fn seek(&self, t: f64, accurate: bool) {
        let Some(d) = &self.decoder else { return };
        let mut q = self.shared.lock();
        let t = t.clamp(0.0, q.duration.max(0.0));
        q.gen += 1;
        q.video.clear();
        q.audio.clear();
        q.clock = Clock::Waiting;
        q.eof = false;
        let _ = d.cmd.send(Seek { gen: q.gen, t, accurate, speed: q.speed });
        self.shared.cv.notify_all();
    }

    pub fn set_paused(&self, paused: bool) {
        let mut q = self.shared.lock();
        q.rebase();
        q.paused = paused;
    }

    pub fn paused(&self) -> bool {
        self.shared.lock().paused
    }

    /// Changes the playback speed; audio keeps its pitch (atempo).
    pub fn set_speed(&self, speed: f64, at: f64) {
        {
            let mut q = self.shared.lock();
            q.rebase();
            q.speed = speed;
        }
        // Buffered audio was stretched for the old speed: re-decode from here.
        self.seek(at, true);
    }

    pub fn set_gain(&self, gain: f32) {
        self.shared.lock().gain = gain;
    }

    pub fn duration(&self) -> f64 {
        self.shared.lock().duration
    }

    pub fn aspect(&self) -> f32 {
        self.shared.lock().aspect
    }

    pub fn info(&self) -> Option<crate::probe::Info> {
        self.shared.lock().info.clone()
    }

    pub fn error(&self) -> Option<String> {
        self.shared.lock().error.clone()
    }

    /// Advances playback: returns the newest frame that is due (if any) and the timing state.
    pub fn tick(&self, want_preview: bool) -> (Option<Frame>, Status) {
        let mut q = self.shared.lock();
        let now = q.now();
        let mut shown = None;
        while q.video.front().is_some_and(|f| now.is_some_and(|now| f.pts <= now)) {
            shown = q.video.pop_front();
        }
        // After a seek, show the first frame at the new position right away, even if the
        // clock (e.g. paused audio that starts a few ms earlier) hasn't reached it yet.
        if want_preview && shown.is_none() {
            shown = q.video.pop_front();
        }
        if shown.is_some() {
            self.shared.cv.notify_all();
        }
        let next_in = match (now, q.video.front()) {
            (Some(now), Some(f)) => Some(((f.pts - now) / q.speed).max(0.0)),
            _ => None,
        };
        let ended = q.eof && q.video.is_empty() && q.audio.is_empty();
        let now = now.map(|t| if q.duration > 0.0 { t.clamp(0.0, q.duration) } else { t.max(0.0) });
        (shown, Status { now, next_in, ended })
    }
}

fn audio_thread(shared: Arc<Shared>, repaint: impl Fn() + Send + 'static) {
    let stream = open_audio(&shared);
    {
        let mut q = shared.lock();
        q.out_ready = true;
        if stream.is_none() {
            q.out = None;
        }
    }
    shared.cv.notify_all();
    repaint();
    // The stream stops when dropped; keep it alive for the life of the app.
    if stream.is_some() {
        loop {
            std::thread::park();
        }
    }
}

fn open_audio(shared: &Arc<Shared>) -> Option<cpal::Stream> {
    let device = cpal::default_host().default_output_device()?;
    let config = device.default_output_config().ok()?;
    let rate = config.sample_rate().0;
    let channels = config.channels();
    shared.lock().out = Some((rate, channels));
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config.into(), shared.clone()),
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config.into(), shared.clone()),
        cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config.into(), shared.clone()),
        cpal::SampleFormat::I32 => build_stream::<i32>(&device, &config.into(), shared.clone()),
        _ => None,
    }?;
    stream.play().ok()?;
    Some(stream)
}

fn build_stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: Arc<Shared>,
) -> Option<cpal::Stream> {
    let ch = config.channels as usize;
    let rate = config.sample_rate.0 as f64;
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let mut q = shared.lock();
                let mut filled = 0;
                if !q.paused && matches!(q.clock, Clock::Audio) {
                    let take = (data.len() / ch).min(q.audio.len() / ch) * ch;
                    let gain = q.gain;
                    for (out, s) in data.iter_mut().zip(q.audio.drain(..take)) {
                        *out = T::from_sample(s * gain);
                    }
                    filled = take;
                    let mut frames = take / ch;
                    // Out of audio because it ended before the video, or because a gap in the
                    // audio track filled the video queue: let time run on through the silence.
                    if q.eof || q.video.len() >= VIDEO_MAX {
                        frames = data.len() / ch;
                    }
                    q.audio_pos += frames as f64 / rate * q.speed;
                }
                drop(q);
                for out in &mut data[filled..] {
                    *out = T::from_sample(0.0f32);
                }
                shared.cv.notify_all();
            },
            |_| {},
            None,
        )
        .ok()
}

struct AudioDec {
    stream: usize,
    dec: ff::decoder::Audio,
    tb: f64,
    graph: Option<ff::filter::Graph>,
    /// Media time of the first sample fed into the filter since the last seek.
    first_pts: Option<f64>,
}

fn build_graph(dec: &ff::decoder::Audio, rate: u32, channels: u16, speed: f64) -> Result<ff::filter::Graph, ff::Error> {
    let mut g = ff::filter::Graph::new();
    let mut layout = dec.channel_layout().bits();
    if layout == 0 {
        layout = ff::ChannelLayout::default(dec.channels() as i32).bits();
    }
    let args = format!(
        "time_base=1/{r}:sample_rate={r}:sample_fmt={}:channel_layout=0x{layout:x}",
        dec.format().name(),
        r = dec.rate(),
    );
    g.add(&ff::filter::find("abuffer").ok_or(ff::Error::FilterNotFound)?, "in", &args)?;
    g.add(&ff::filter::find("abuffersink").ok_or(ff::Error::FilterNotFound)?, "out", "")?;
    let out_layout = ff::ChannelLayout::default(channels as i32).bits();
    // atempo only covers 0.5x-2x per instance; chain two for the extremes.
    let mut tempo = String::new();
    let mut s = speed;
    while (s - 1.0).abs() > 1e-3 {
        let step = s.clamp(0.5, 2.0);
        tempo += &format!("atempo={step},");
        s /= step;
    }
    let spec = format!("{tempo}aformat=sample_fmts=flt:sample_rates={rate}:channel_layouts=0x{out_layout:x}");
    g.output("in", 0)?.input("out", 0)?.parse(&spec)?;
    g.validate()?;
    Ok(g)
}

fn decode(path: &PathBuf, shared: &Shared, cmds: Receiver<Seek>, stop: &AtomicBool, first: Seek) -> Result<(), String> {
    let mut ictx = ff::format::input(path).map_err(|e| format!("Can't open file: {e}"))?;
    let start_time = unsafe { (*ictx.as_ptr()).start_time };
    let offset = if start_time == ff::ffi::AV_NOPTS_VALUE { 0.0 } else { start_time as f64 / 1e6 };
    let info = crate::probe::media_info(&ictx, path);

    let vstream = ictx.streams().best(ff::media::Type::Video).ok_or("No video stream")?;
    let (vindex, vtb) = (vstream.index(), f64::from(vstream.time_base()));
    let mut vctx = ff::codec::context::Context::from_parameters(vstream.parameters()).map_err(|e| e.to_string())?;
    vctx.set_threading(ff::codec::threading::Config { kind: ff::codec::threading::Type::Frame, count: 0 });
    let mut vdec = vctx.decoder().video().map_err(|e| format!("Unsupported video codec: {e}"))?;
    let sar = vdec.aspect_ratio();
    let sar = if sar.numerator() > 0 && sar.denominator() > 0 { f64::from(sar) } else { 1.0 };
    let astream = ictx.streams().best(ff::media::Type::Audio).map(|s| (s.index(), f64::from(s.time_base()), s.parameters()));

    // The device format is needed for the audio filter; the device is usually open by now.
    let out = {
        let mut q = shared.lock();
        q.duration = ictx.duration().max(0) as f64 / 1e6;
        q.aspect = (vdec.width() as f64 * sar / vdec.height().max(1) as f64) as f32;
        q.info = Some(info);
        while !q.out_ready && !stop.load(Relaxed) {
            q = shared.cv.wait_timeout(q, Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
        }
        q.out
    };
    let mut audio = match (astream, out) {
        (Some((stream, tb, par)), Some(_)) => ff::codec::context::Context::from_parameters(par)
            .and_then(|c| c.decoder().audio())
            .ok()
            .map(|dec| AudioDec { stream, dec, tb, graph: None, first_pts: None }),
        _ => None,
    };
    let (out_rate, out_ch) = out.unwrap_or((0, 0));
    let has_audio = audio.is_some();

    let (mut gen, mut skip_until) = (first.gen, f64::NEG_INFINITY);
    let mut seek = Some(first);
    let mut scaler = None;
    let mut vframe = ff::frame::Video::empty();
    let mut aframe = ff::frame::Audio::empty();
    let mut filtered = ff::frame::Audio::empty();
    let mut packet = ff::Packet::empty();

    loop {
        if stop.load(Relaxed) {
            return Ok(());
        }
        // A newer command supersedes the pending one.
        let pending = seek.take();
        if let Some(cmd) = cmds.try_iter().last().or(pending) {
            gen = cmd.gen;
            let ts = ((cmd.t + offset) * 1e6) as i64;
            let _ = ictx.seek(ts, ..ts);
            vdec.flush();
            if let Some(a) = audio.as_mut() {
                a.dec.flush();
                a.first_pts = None;
                a.graph = build_graph(&a.dec, out_rate, out_ch, cmd.speed).ok();
            }
            skip_until = if cmd.accurate { cmd.t } else { f64::NEG_INFINITY };
        }

        let eof = packet.read(&mut ictx).is_err();
        let mut sink = Sink { shared, stop, gen, has_audio, scaler: &mut scaler, skip_until, out_ch: out_ch as usize };
        if eof || packet.stream() == vindex {
            let _ = if eof { vdec.send_eof() } else { vdec.send_packet(&packet) };
            while vdec.receive_frame(&mut vframe).is_ok() {
                sink.video(&vframe, vtb, offset);
            }
        }
        if let Some(a) = audio.as_mut().filter(|a| eof || a.stream == packet.stream()) {
            let _ = if eof { a.dec.send_eof() } else { a.dec.send_packet(&packet) };
            while a.dec.receive_frame(&mut aframe).is_ok() {
                sink.audio(a, &aframe, &mut filtered, offset);
            }
            if eof {
                if let Some(mut src) = a.graph.as_mut().and_then(|g| g.get("in")) {
                    let _ = src.source().flush();
                }
                sink.pull_audio(a, &mut filtered);
            }
        }
        if !eof {
            continue;
        }
        {
            let mut q = shared.lock();
            if q.gen == gen {
                q.eof = true;
                if matches!(q.clock, Clock::Waiting) {
                    // Nothing (or no audio) after the seek point: run on the wall clock.
                    let pos = q.video.front().map_or(q.duration, |f| f.pts);
                    q.clock = Clock::Wall { at: Instant::now(), pos };
                }
            }
        }
        // Idle until told to seek again.
        loop {
            match cmds.recv_timeout(Duration::from_millis(50)) {
                Ok(cmd) => {
                    seek = Some(cmd);
                    break;
                }
                Err(RecvTimeoutError::Timeout) if !stop.load(Relaxed) => {}
                Err(_) => return Ok(()),
            }
        }
    }
}

/// Converts decoded frames and hands them to the shared queue.
struct Sink<'a> {
    shared: &'a Shared,
    stop: &'a AtomicBool,
    gen: u64,
    has_audio: bool,
    scaler: &'a mut Option<ff::software::scaling::Context>,
    skip_until: f64,
    out_ch: usize,
}

impl Sink<'_> {
    /// Waits for room in the queue. Returns false if a seek or stop made the data stale.
    fn wait_room(&self) -> bool {
        let mut q = self.shared.lock();
        loop {
            if q.gen != self.gen || self.stop.load(Relaxed) {
                return false;
            }
            let room = q.video.len() < VIDEO_AHEAD
                || (self.has_audio && q.audio_secs() < 0.3 && q.video.len() < VIDEO_MAX);
            if room {
                return true;
            }
            q = self.shared.cv.wait_timeout(q, Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    fn video(&mut self, f: &ff::frame::Video, tb: f64, offset: f64) {
        let Some(ts) = f.timestamp().or(f.pts()) else { return };
        let pts = ts as f64 * tb - offset;
        if pts + 0.001 < self.skip_until {
            return;
        }
        let (w, h) = (f.width(), f.height());
        if w == 0 || h == 0 {
            return;
        }
        use ff::format::Pixel;
        let full_range = f.color_range() == ff::color::Range::JPEG || f.format() == Pixel::YUVJ420P;
        let bt709 = match f.color_space() {
            ff::color::Space::BT709 => true,
            ff::color::Space::BT470BG | ff::color::Space::SMPTE170M => false,
            _ => h > 576,
        };
        let planes = if matches!(f.format(), Pixel::YUV420P | Pixel::YUVJ420P) {
            pack_planes(f)
        } else {
            let ok = self.scaler.as_ref().is_some_and(|s| {
                s.input().format == f.format() && s.input().width == w && s.input().height == h
            });
            if !ok {
                *self.scaler = ff::software::scaling::Context::get(
                    f.format(), w, h, Pixel::YUV420P, w, h, ff::software::scaling::Flags::BILINEAR,
                )
                .ok();
            }
            let Some(sc) = self.scaler.as_mut() else { return };
            let mut out = ff::frame::Video::empty();
            if sc.run(f, &mut out).is_err() {
                return;
            }
            pack_planes(&out)
        };
        if !self.wait_room() {
            return;
        }
        let mut q = self.shared.lock();
        if q.gen != self.gen {
            return;
        }
        q.video.push_back(Frame { pts, width: w, height: h, planes, bt709, full_range });
        // No audio stream, or one that yields nothing (undecodable, or starting much later):
        // without this the clock would wait forever.
        if matches!(q.clock, Clock::Waiting) && (!self.has_audio || q.video.len() >= VIDEO_MAX) {
            let pos = q.video.front().map_or(pts, |f| f.pts);
            q.clock = Clock::Wall { at: Instant::now(), pos };
        }
    }

    fn audio(&mut self, a: &mut AudioDec, f: &ff::frame::Audio, filtered: &mut ff::frame::Audio, offset: f64) {
        let Some(ts) = f.timestamp().or(f.pts()) else { return };
        let pts = ts as f64 * a.tb - offset;
        let end = pts + f.samples() as f64 / f.rate().max(1) as f64;
        if end < self.skip_until {
            return;
        }
        let Some(g) = a.graph.as_mut() else { return };
        let Some(mut src) = g.get("in") else { return };
        if src.source().add(f).is_err() {
            return;
        }
        a.first_pts.get_or_insert(pts);
        self.pull_audio(a, filtered);
    }

    fn pull_audio(&mut self, a: &mut AudioDec, filtered: &mut ff::frame::Audio) {
        let Some(g) = a.graph.as_mut() else { return };
        let Some(mut out) = g.get("out") else { return };
        while out.sink().frame(filtered).is_ok() {
            let n = filtered.samples() * self.out_ch;
            let bytes = &filtered.data(0)[..n * 4];
            let samples = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b));
            if !self.wait_room() {
                return;
            }
            let mut q = self.shared.lock();
            if q.gen != self.gen {
                return;
            }
            if matches!(q.clock, Clock::Waiting) {
                q.audio_pos = a.first_pts.unwrap_or(0.0);
                q.clock = Clock::Audio;
            }
            q.audio.extend(samples);
        }
    }
}

fn pack_planes(f: &ff::frame::Video) -> [Vec<u8>; 3] {
    let (w, h) = (f.width() as usize, f.height() as usize);
    let dims = [(w, h), (w.div_ceil(2), h.div_ceil(2)), (w.div_ceil(2), h.div_ceil(2))];
    std::array::from_fn(|i| {
        let (pw, ph) = dims[i];
        let stride = f.stride(i);
        let data = f.data(i);
        let mut out = Vec::with_capacity(pw * ph);
        for row in 0..ph {
            out.extend_from_slice(&data[row * stride..row * stride + pw]);
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays a real file headlessly (needs an audio device):
    /// `GLANCE_SAMPLE=<video with audio> cargo test -- --ignored`
    #[test]
    #[ignore]
    fn speed_and_seek_keep_time() {
        let path = PathBuf::from(std::env::var("GLANCE_SAMPLE").expect("set GLANCE_SAMPLE"));
        let mut e = Engine::new(|| {});
        e.open(path, 0.0);
        let play = |e: &Engine, secs: f64| {
            let until = Instant::now() + Duration::from_secs_f64(secs);
            let mut now = None;
            while Instant::now() < until {
                now = e.tick(false).1.now.or(now);
                std::thread::sleep(Duration::from_millis(5));
            }
            now.unwrap_or(-1.0)
        };
        let t0 = play(&e, 1.0);
        e.set_speed(1.5, t0);
        let t1 = play(&e, 2.0);
        println!("1x for 1s -> {t0:.2}, then 1.5x for 2s -> {t1:.2}");
        assert!((t1 - t0 - 3.0).abs() < 0.25, "1.5x should cover ~3s in 2s, got {:.2}", t1 - t0);
        e.set_speed(1.0, t1);
        e.seek(12.0, true);
        let (mut first, until) = (None, Instant::now() + Duration::from_secs(2));
        while first.is_none() && Instant::now() < until {
            first = e.tick(true).0.map(|f| f.pts);
            std::thread::sleep(Duration::from_millis(5));
        }
        println!("seek to 12.0 -> first frame {first:?}");
        assert!(first.is_some_and(|p| (p - 12.0).abs() < 0.05));
    }
}
