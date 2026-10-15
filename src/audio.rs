//! Playback engine: a decoder thread feeding a lock-free ring buffer that the
//! audio device callback drains.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::*};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use rtrb::{Consumer, Producer, RingBuffer};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler as _};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{SeekMode, SeekTo};
use symphonia::core::units::Time;

use crate::decode::{self, Opened, TrackInfo};

/// Stereo frames kept for the visualizations (power of two).
pub const VIS_LEN: usize = 32768;

pub struct Shared {
    paused: AtomicBool,
    volume: AtomicU32,
    flush_req: AtomicU64,
    flush_ack: AtomicU64,
    /// Output frames played since the last flush.
    played: AtomicU64,
    /// Track position (seconds, f64 bits) at the last flush.
    base_pos: AtomicU64,
    out_rate: AtomicU32,
    /// Decoder has pushed the last samples of the track.
    eof: AtomicBool,
    /// Device has played everything up to the end of the track.
    finished: AtomicBool,
    stream_error: AtomicBool,
    /// Commands sent by the UI / applied by the decoder. While they differ, a
    /// load or seek is in flight and end-of-track signals are stale.
    cmd_sent: AtomicU64,
    cmd_done: AtomicU64,
    /// Bumped by every pause/play from the UI, so a slow `Load` doesn't undo a
    /// pause issued after it was requested.
    pause_gen: AtomicU64,
    vis_l: Box<[AtomicU32]>,
    vis_r: Box<[AtomicU32]>,
    /// Total frames ever written to the visualization rings (monotonic).
    vis_count: AtomicU64,
    waker: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl Shared {
    fn wake(&self) {
        if let Some(w) = self.waker.get() {
            w();
        }
    }
}

enum Cmd {
    /// Path and the `pause_gen` at the time of the request.
    Load(PathBuf, u64),
    Seek(f64),
}

pub enum Event {
    Loaded(TrackInfo),
    Failed(PathBuf, String),
    /// The output device failed (`Some(message)`) or came back (`None`).
    Device(Option<String>),
}

pub struct Player {
    tx: Sender<Cmd>,
    events: Receiver<Event>,
    shared: Arc<Shared>,
}

impl Player {
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            paused: AtomicBool::new(false),
            volume: AtomicU32::new(1.0f32.to_bits()),
            flush_req: AtomicU64::new(0),
            flush_ack: AtomicU64::new(0),
            played: AtomicU64::new(0),
            base_pos: AtomicU64::new(0f64.to_bits()),
            out_rate: AtomicU32::new(48_000),
            eof: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            stream_error: AtomicBool::new(false),
            cmd_sent: AtomicU64::new(0),
            cmd_done: AtomicU64::new(0),
            pause_gen: AtomicU64::new(0),
            vis_l: (0..VIS_LEN).map(|_| AtomicU32::new(0)).collect(),
            vis_r: (0..VIS_LEN).map(|_| AtomicU32::new(0)).collect(),
            vis_count: AtomicU64::new(0),
            waker: OnceLock::new(),
        });
        let (tx, rx) = mpsc::channel();
        let (ev_tx, events) = mpsc::channel();
        let s = shared.clone();
        thread::Builder::new()
            .name("decoder".into())
            .spawn(move || Engine::new(s, ev_tx).run(rx))
            .expect("spawn decoder thread");
        Player { tx, events, shared }
    }

    /// Called whenever the decoder thread has news for the UI.
    pub fn set_waker(&self, f: impl Fn() + Send + Sync + 'static) {
        let _ = self.shared.waker.set(Box::new(f));
    }

    pub fn load(&self, path: PathBuf) {
        let generation = self.shared.pause_gen.load(Acquire);
        self.shared.cmd_sent.fetch_add(1, AcqRel);
        let _ = self.tx.send(Cmd::Load(path, generation));
    }

    pub fn seek(&self, secs: f64) {
        self.shared.cmd_sent.fetch_add(1, AcqRel);
        let _ = self.tx.send(Cmd::Seek(secs.max(0.0)));
    }

    /// A load or seek has been requested but not applied yet.
    pub fn busy(&self) -> bool {
        self.shared.cmd_sent.load(Acquire) != self.shared.cmd_done.load(Acquire)
    }

    pub fn poll_event(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    pub fn paused(&self) -> bool {
        self.shared.paused.load(Relaxed)
    }

    pub fn set_paused(&self, p: bool) {
        self.shared.pause_gen.fetch_add(1, AcqRel);
        self.shared.paused.store(p, Relaxed);
    }

    pub fn set_volume(&self, v: f32) {
        self.shared
            .volume
            .store(v.clamp(0.0, 1.0).to_bits(), Relaxed);
    }

    pub fn position(&self) -> f64 {
        self.shared.position()
    }

    /// True once (per track end): the device played the final sample.
    /// Ignored while a load or seek is in flight, since that replaces the ending.
    pub fn take_finished(&self) -> bool {
        self.shared.finished.swap(false, AcqRel) && !self.busy()
    }

    pub fn out_rate(&self) -> u32 {
        self.shared.out_rate.load(Relaxed)
    }

    /// Total number of frames the device has been handed so far.
    pub fn vis_count(&self) -> u64 {
        self.shared.vis_count.load(Acquire)
    }

    /// Copies the played frames `[end - n, end)` (oldest first) into `l` and `r`,
    /// where `n` is the length of the shorter slice (at most `VIS_LEN`).
    pub fn read_vis(&self, end: u64, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len()).min(VIS_LEN);
        let start = end.wrapping_sub(n as u64);
        for i in 0..n {
            let idx = (start.wrapping_add(i as u64) as usize) & (VIS_LEN - 1);
            l[i] = f32::from_bits(self.shared.vis_l[idx].load(Relaxed));
            r[i] = f32::from_bits(self.shared.vis_r[idx].load(Relaxed));
        }
    }
}

impl Shared {
    fn position(&self) -> f64 {
        let rate = self.out_rate.load(Relaxed).max(1) as f64;
        f64::from_bits(self.base_pos.load(Relaxed)) + self.played.load(Relaxed) as f64 / rate
    }
}

// ---------------------------------------------------------------------------
// Decoder thread

struct Output {
    _stream: cpal::Stream,
    prod: Producer<f32>,
    rate: u32,
    channels: usize,
    /// Track sample rate this output was opened for.
    requested_rate: u32,
}

struct Current {
    opened: Opened,
    resampler: Option<Resampler>,
    src: Vec<f32>,
    mapped: Vec<f32>,
    pending: Vec<f32>,
    pending_off: usize,
    decoded_all: bool,
    done: bool,
}

struct Engine {
    shared: Arc<Shared>,
    events: Sender<Event>,
    out: Option<Output>,
    cur: Option<Current>,
    /// Output device lost: position to resume at and time of the last retry.
    lost: Option<(f64, Instant)>,
}

impl Engine {
    fn new(shared: Arc<Shared>, events: Sender<Event>) -> Self {
        Engine {
            shared,
            events,
            out: None,
            cur: None,
            lost: None,
        }
    }

    fn send(&self, ev: Event) {
        let _ = self.events.send(ev);
        self.shared.wake();
    }

    fn run(mut self, rx: Receiver<Cmd>) {
        loop {
            let busy = self.cur.as_ref().is_some_and(|c| !c.done);
            let cmd = if busy {
                match rx.try_recv() {
                    Ok(c) => Some(c),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                }
            } else {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(c) => Some(c),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            };
            if let Some(cmd) = cmd {
                match cmd {
                    Cmd::Load(path, generation) => self.load(path, generation),
                    Cmd::Seek(t) => {
                        self.seek(t);
                    }
                }
                self.shared.cmd_done.fetch_add(1, AcqRel);
                self.shared.wake();
            }
            if self.shared.stream_error.swap(false, AcqRel) {
                self.recover_output();
            }
            if let Some((pos, at)) = self.lost
                && at.elapsed() > Duration::from_secs(1)
            {
                self.retry_output(pos);
            }
            if !self.produce() {
                thread::sleep(Duration::from_millis(4));
            }
        }
    }

    fn load(&mut self, path: PathBuf, pause_gen: u64) {
        let opened = match decode::open(&path) {
            Ok(o) => o,
            Err(e) => {
                self.send(Event::Failed(path, e));
                return;
            }
        };
        // The old track's ending is no longer relevant; clear it before the
        // flush so the flushing callback can't report it.
        self.shared.eof.store(false, Release);
        self.shared.finished.store(false, Release);
        let rate = opened.info.sample_rate;
        let reopen = self.out.as_ref().is_none_or(|o| o.requested_rate != rate);
        if reopen {
            // Fade out what is playing before dropping the stream.
            self.flush();
            self.out = None;
            match open_output(&self.shared, rate) {
                Ok(o) => {
                    self.out = Some(o);
                    if self.lost.take().is_some() {
                        self.send(Event::Device(None));
                    }
                }
                Err(e) => {
                    self.cur = None;
                    self.send(Event::Failed(path, e));
                    return;
                }
            }
        } else {
            self.flush();
        }
        let out = self.out.as_ref().unwrap();
        self.shared.base_pos.store(0f64.to_bits(), Relaxed);
        self.shared.eof.store(false, Release);
        self.shared.finished.store(false, Release);
        // Start playing unless the user paused after asking for this track.
        if self.shared.pause_gen.load(Acquire) == pause_gen {
            self.shared.paused.store(false, Relaxed);
        }
        let info = opened.info.clone();
        self.cur = Some(Current {
            resampler: Resampler::new(rate, out.rate, out.channels),
            opened,
            src: Vec::new(),
            mapped: Vec::new(),
            pending: Vec::new(),
            pending_off: 0,
            decoded_all: false,
            done: false,
        });
        self.send(Event::Loaded(info));
    }

    /// Seeks the current track. Returns false if it could not.
    fn seek(&mut self, t: f64) -> bool {
        let Some(cur) = self.cur.as_mut() else {
            return false;
        };
        let mut pos = try_seek(&mut cur.opened, t);
        if pos.is_none() {
            // Some demuxers (e.g. MKV without cues) only seek forward: retry on
            // a fresh reader, and only swap it in if that works.
            if let Ok(mut fresh) = decode::open(&cur.opened.info.path) {
                let p = if t <= 0.0 {
                    Some(0.0)
                } else {
                    try_seek(&mut fresh, t)
                };
                if p.is_some() {
                    cur.opened = fresh;
                    pos = p;
                }
            }
        }
        let Some(pos) = pos else {
            if cur.done {
                // Let the callback report the end again instead of hanging.
                self.shared.eof.store(true, Release);
            }
            return false;
        };
        self.shared.eof.store(false, Release);
        self.shared.finished.store(false, Release);
        cur.opened.decoder.reset();
        if let Some(r) = cur.resampler.as_mut() {
            r.reset();
        }
        cur.pending.clear();
        cur.pending_off = 0;
        cur.decoded_all = false;
        cur.done = false;
        self.flush();
        self.shared.base_pos.store(pos.to_bits(), Relaxed);
        self.shared.eof.store(false, Release);
        self.shared.finished.store(false, Release);
        true
    }

    /// Ask the audio callback to drop everything queued and wait until it did.
    fn flush(&self) {
        if self.out.is_none() {
            return;
        }
        let req = self.shared.flush_req.fetch_add(1, AcqRel) + 1;
        let start = Instant::now();
        while self.shared.flush_ack.load(Acquire) != req
            && start.elapsed() < Duration::from_millis(250)
        {
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// The device disappeared or changed: reopen on the current default and resume.
    fn recover_output(&mut self) {
        let Some(old) = self.out.take() else { return };
        let pos = self.shared.position();
        drop(old);
        if self.cur.is_none() {
            // Nothing playing; the next load opens a fresh output.
            return;
        }
        // Give the OS a moment to settle on the new default device.
        thread::sleep(Duration::from_millis(150));
        self.retry_output(pos);
    }

    fn retry_output(&mut self, pos: f64) {
        let rate = self
            .cur
            .as_ref()
            .map(|c| c.opened.info.sample_rate)
            .unwrap_or(48_000);
        match open_output(&self.shared, rate) {
            Ok(o) => {
                if let Some(cur) = self.cur.as_mut() {
                    cur.resampler = Resampler::new(rate, o.rate, o.channels);
                    // Re-decode from `pos`, even if the track had fully decoded.
                    cur.done = false;
                }
                self.out = Some(o);
                if !self.seek(pos) {
                    self.shared.base_pos.store(pos.to_bits(), Relaxed);
                }
                if self.lost.take().is_some() {
                    self.send(Event::Device(None));
                }
            }
            Err(e) => {
                if self.lost.is_none() {
                    self.send(Event::Device(Some(format!(
                        "Audio output unavailable: {e}"
                    ))));
                }
                self.lost = Some((pos, Instant::now()));
            }
        }
    }

    /// Moves decoded audio into the ring buffer. Returns false when idle/full.
    fn produce(&mut self) -> bool {
        let (Some(cur), Some(out)) = (self.cur.as_mut(), self.out.as_mut()) else {
            return false;
        };
        if cur.done {
            return false;
        }
        if cur.pending_off < cur.pending.len() {
            let ch = out.channels;
            let n = (out.prod.slots().min(cur.pending.len() - cur.pending_off) / ch) * ch;
            if n == 0 {
                return false;
            }
            if let Ok(mut chunk) = out.prod.write_chunk(n) {
                let src = &cur.pending[cur.pending_off..cur.pending_off + n];
                let (a, b) = chunk.as_mut_slices();
                a.copy_from_slice(&src[..a.len()]);
                b.copy_from_slice(&src[a.len()..]);
                chunk.commit_all();
                cur.pending_off += n;
            }
            return true;
        }
        if cur.decoded_all {
            cur.done = true;
            self.shared.eof.store(true, Release);
            return false;
        }
        decode_next(cur, out.channels);
        true
    }
}

/// Accurate seek on `o`; returns the position actually reached, in seconds.
fn try_seek(o: &mut Opened, t: f64) -> Option<f64> {
    let time = Time::try_from_secs_f64(t)?;
    let to = SeekTo::Time {
        time,
        track_id: Some(o.track_id),
    };
    let s = o.format.seek(SeekMode::Accurate, to).ok()?;
    Some(
        o.time_base
            .and_then(|tb| tb.calc_time(s.actual_ts))
            .map(|t| t.as_secs_f64())
            .unwrap_or(t),
    )
}

fn decode_next(cur: &mut Current, out_ch: usize) {
    cur.pending.clear();
    cur.pending_off = 0;
    loop {
        let packet = match cur.opened.format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) | Err(_) => {
                if let Some(r) = cur.resampler.as_mut() {
                    r.finish(&mut cur.pending);
                }
                cur.decoded_all = true;
                return;
            }
        };
        if packet.track_id != cur.opened.track_id {
            continue;
        }
        match cur.opened.decoder.decode(&packet) {
            Ok(buf) => {
                let sch = buf.spec().channels().count().max(1);
                buf.copy_to_vec_interleaved(&mut cur.src);
                map_channels(&cur.src, sch, out_ch, &mut cur.mapped);
                match cur.resampler.as_mut() {
                    Some(r) => r.process(&cur.mapped, &mut cur.pending),
                    None => std::mem::swap(&mut cur.pending, &mut cur.mapped),
                }
                if !cur.pending.is_empty() {
                    return;
                }
            }
            Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => continue,
            Err(_) => {
                cur.decoded_all = true;
                return;
            }
        }
    }
}

/// Converts interleaved audio from `sch` to `dch` channels.
fn map_channels(src: &[f32], sch: usize, dch: usize, dst: &mut Vec<f32>) {
    let frames = src.len() / sch;
    dst.clear();
    dst.reserve(frames * dch);
    if sch == dch {
        dst.extend_from_slice(&src[..frames * sch]);
        return;
    }
    for f in src.chunks_exact(sch) {
        if sch == 1 {
            dst.extend(std::iter::repeat_n(f[0], dch));
        } else if dch == 1 {
            dst.push((f[0] + f[1]) * 0.5);
        } else if dch == 2 && sch >= 6 {
            // 5.1 (FL FR FC LFE BL BR) down to stereo.
            const K: f32 = 0.707;
            const N: f32 = 1.0 / (1.0 + K + K);
            dst.push((f[0] + K * f[2] + K * f[4]) * N);
            dst.push((f[1] + K * f[2] + K * f[5]) * N);
        } else {
            dst.extend(f.iter().copied().chain(std::iter::repeat(0.0)).take(dch));
        }
    }
}

// ---------------------------------------------------------------------------
// Resampling (only used when the device can't run at the file's rate)

struct Resampler {
    fft: Fft<f32>,
    ch: usize,
    input: Vec<f32>,
    out: Vec<f32>,
}

impl Resampler {
    fn new(from: u32, to: u32, ch: usize) -> Option<Self> {
        if from == to {
            return None;
        }
        let fft = Fft::<f32>::new(from as usize, to as usize, 1024, ch, FixedSync::Input).ok()?;
        let out = vec![0.0; fft.output_frames_max() * ch];
        Some(Resampler {
            fft,
            ch,
            input: Vec::new(),
            out,
        })
    }

    fn reset(&mut self) {
        self.fft.reset();
        self.input.clear();
    }

    fn process(&mut self, data: &[f32], dst: &mut Vec<f32>) {
        self.input.extend_from_slice(data);
        let mut off = 0;
        loop {
            let need = self.fft.input_frames_next();
            if (self.input.len() - off) / self.ch < need {
                break;
            }
            let n = self.run(off, need, None, dst);
            off += n * self.ch;
        }
        self.input.drain(..off);
    }

    /// Flushes the remaining partial chunk plus the filter delay.
    fn finish(&mut self, dst: &mut Vec<f32>) {
        let left = self.input.len() / self.ch;
        let need = self.fft.input_frames_next();
        self.input.resize(need * self.ch, 0.0);
        let idx = Indexing {
            partial_len: Some(left),
            ..Default::default()
        };
        self.run(0, need, Some(&idx), dst);
        self.input.iter_mut().for_each(|s| *s = 0.0);
        self.run(0, need, None, dst);
        self.input.clear();
    }

    fn run(
        &mut self,
        off: usize,
        frames: usize,
        idx: Option<&Indexing>,
        dst: &mut Vec<f32>,
    ) -> usize {
        let input = &self.input[off..off + frames * self.ch];
        let max = self.out.len() / self.ch;
        let (Ok(inp), Ok(mut outp)) = (
            InterleavedSlice::new(input, self.ch, frames),
            InterleavedSlice::new_mut(&mut self.out, self.ch, max),
        ) else {
            return frames;
        };
        match self.fft.process_into_buffer(&inp, &mut outp, idx) {
            Ok((nin, nout)) => {
                dst.extend_from_slice(&self.out[..nout * self.ch]);
                nin.max(1)
            }
            Err(_) => frames,
        }
    }
}

// ---------------------------------------------------------------------------
// Audio device

fn open_output(shared: &Arc<Shared>, want_rate: u32) -> Result<Output, String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or("No audio output device")?;
    let default = device.default_output_config().map_err(|e| e.to_string())?;

    // Prefer running the device at the file's native rate (no resampling).
    let mut chosen = default;
    if default.sample_rate() != want_rate
        && let Ok(ranges) = device.supported_output_configs()
    {
        let mut best: Option<cpal::SupportedStreamConfig> = None;
        for r in ranges {
            if r.channels() != default.channels()
                || r.min_sample_rate() > want_rate
                || r.max_sample_rate() < want_rate
            {
                continue;
            }
            let better = match &best {
                None => true,
                Some(b) => {
                    b.sample_format() != default.sample_format()
                        && r.sample_format() == default.sample_format()
                }
            };
            if better {
                best = Some(r.with_sample_rate(want_rate));
            }
        }
        if let Some(b) = best {
            chosen = b;
        }
    }

    let config: StreamConfig = chosen.config();
    let rate = config.sample_rate;
    let channels = config.channels as usize;
    // ~0.5 s of buffered audio.
    let (prod, cons) = RingBuffer::<f32>::new((rate as usize / 2).max(4096) * channels);

    shared.out_rate.store(rate, Relaxed);
    shared.played.store(0, Relaxed);
    let ack = shared.flush_req.load(Acquire);
    shared.flush_ack.store(ack, Release);

    let stream = match chosen.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, config, cons, shared.clone()),
        SampleFormat::F64 => build::<f64>(&device, config, cons, shared.clone()),
        SampleFormat::I16 => build::<i16>(&device, config, cons, shared.clone()),
        SampleFormat::I32 => build::<i32>(&device, config, cons, shared.clone()),
        SampleFormat::U16 => build::<u16>(&device, config, cons, shared.clone()),
        SampleFormat::U8 => build::<u8>(&device, config, cons, shared.clone()),
        SampleFormat::I8 => build::<i8>(&device, config, cons, shared.clone()),
        f => return Err(format!("Unsupported device sample format {f}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Output {
        _stream: stream,
        prod,
        rate,
        channels,
        requested_rate: want_rate,
    })
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut cons: Consumer<f32>,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let ch = config.channels as usize;
    // ~5 ms gain smoothing: no clicks on pause, seek or volume change.
    let smooth = 1.0 - (-1.0 / (0.005 * config.sample_rate as f32)).exp();
    // Length of the fade-out applied to queued audio when it is flushed.
    let ramp_frames = (config.sample_rate as usize / 250).max(1);
    let mut ack = shared.flush_req.load(Acquire);
    let mut gain = 0.0f32;
    let err_shared = shared.clone();

    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                let frames_out = data.len() / ch;
                let req = shared.flush_req.load(Acquire);
                if req != ack {
                    // Fade out a few ms of what is queued instead of cutting it
                    // (a hard cut clicks), then drop the rest.
                    let mut w = 0;
                    let n = frames_out.min(ramp_frames).min(cons.slots() / ch);
                    if gain > 1e-4
                        && n > 0
                        && let Ok(chunk) = cons.read_chunk(n * ch)
                    {
                        let (a, b) = chunk.as_slices();
                        let mut src = a.iter().chain(b.iter());
                        let step = gain / n as f32;
                        for frame in data[..n * ch].chunks_exact_mut(ch) {
                            gain = (gain - step).max(0.0);
                            for o in frame.iter_mut() {
                                *o = T::from_sample(*src.next().unwrap_or(&0.0) * gain);
                            }
                        }
                        chunk.commit_all();
                        w = n;
                    }
                    let rest = cons.slots();
                    if let Ok(c) = cons.read_chunk(rest) {
                        c.commit_all();
                    }
                    for o in data[w * ch..].iter_mut() {
                        *o = T::EQUILIBRIUM;
                    }
                    ack = req;
                    gain = 0.0;
                    shared.played.store(0, Relaxed);
                    shared.flush_ack.store(req, Release);
                    return;
                }

                let paused = shared.paused.load(Relaxed);
                let target = if paused {
                    0.0
                } else {
                    f32::from_bits(shared.volume.load(Relaxed))
                };
                let mut n = 0;
                if !(paused && gain < 1e-4) {
                    n = frames_out.min(cons.slots() / ch);
                }
                if n > 0
                    && let Ok(chunk) = cons.read_chunk(n * ch)
                {
                    let (a, b) = chunk.as_slices();
                    let mut src = a.iter().chain(b.iter());
                    let mut vpos = shared.vis_count.load(Relaxed);
                    for frame in data[..n * ch].chunks_exact_mut(ch) {
                        gain += (target - gain) * smooth;
                        let (mut l, mut r) = (0.0, 0.0);
                        for (c, o) in frame.iter_mut().enumerate() {
                            let s = *src.next().unwrap_or(&0.0);
                            match c {
                                0 => l = s,
                                1 => r = s,
                                _ => {}
                            }
                            *o = T::from_sample(s * gain);
                        }
                        if ch == 1 {
                            r = l;
                        }
                        let i = (vpos as usize) & (VIS_LEN - 1);
                        shared.vis_l[i].store(l.to_bits(), Relaxed);
                        shared.vis_r[i].store(r.to_bits(), Relaxed);
                        vpos = vpos.wrapping_add(1);
                    }
                    shared.vis_count.store(vpos, Release);
                    chunk.commit_all();
                    shared.played.fetch_add(n as u64, Relaxed);
                }
                for o in data[n * ch..].iter_mut() {
                    *o = T::EQUILIBRIUM;
                }
                if n < frames_out && !paused && shared.eof.load(Acquire) && cons.slots() == 0 {
                    shared.eof.store(false, Release);
                    shared.finished.store(true, Release);
                    shared.wake();
                }
            },
            move |err: cpal::Error| {
                use cpal::ErrorKind::*;
                if !matches!(err.kind(), Xrun | RealtimeDenied) {
                    err_shared.stream_error.store(true, Release);
                }
            },
            None,
        )
        .map_err(|e| e.to_string())
}
