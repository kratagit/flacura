//! Whole-track waveform, computed in the background by several threads that
//! each decode one slice of the file. Buckets appear as soon as they are ready.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::*};
use std::thread;

use symphonia::core::formats::{SeekMode, SeekTo};
use symphonia::core::units::Time;

use crate::decode::{self, TrackInfo};

pub const BUCKETS: usize = 1200;

pub struct Waveform {
    /// Peak absolute amplitude per bucket (f32 bits). Negative = not computed yet.
    pub peak: Box<[AtomicU32]>,
    /// RMS amplitude per bucket (f32 bits).
    pub rms: Box<[AtomicU32]>,
    /// Duration measured by decoding, for files whose header lacks it (f64 bits, 0 = unknown).
    pub measured_secs: AtomicU64,
    cancel: AtomicBool,
    running: AtomicUsize,
}

impl Waveform {
    pub fn get(&self, i: usize) -> Option<(f32, f32)> {
        let p = f32::from_bits(self.peak[i].load(Relaxed));
        (p >= 0.0).then(|| (p, f32::from_bits(self.rms[i].load(Relaxed))))
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Relaxed) > 0
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Relaxed);
    }
}

pub fn spawn(info: &TrackInfo, wake: Arc<dyn Fn() + Send + Sync>) -> Arc<Waveform> {
    let wf = Arc::new(Waveform {
        peak: (0..BUCKETS)
            .map(|_| AtomicU32::new((-1.0f32).to_bits()))
            .collect(),
        rms: (0..BUCKETS).map(|_| AtomicU32::new(0)).collect(),
        measured_secs: AtomicU64::new(0),
        cancel: AtomicBool::new(false),
        running: AtomicUsize::new(0),
    });

    let total = info.total_frames.filter(|&t| t > 0);
    let workers = match total {
        Some(_) => thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(1, 6),
        None => 1,
    };
    wf.running.store(workers, Relaxed);
    for k in 0..workers {
        let wf = wf.clone();
        let path = info.path.clone();
        let rate = info.sample_rate;
        let wake = wake.clone();
        let _ = thread::Builder::new()
            .name("waveform".into())
            .spawn(move || {
                match total {
                    Some(t) => slice_worker(&wf, path, rate, t, k, workers),
                    None => unknown_length_worker(&wf, path, rate),
                }
                wf.running.fetch_sub(1, Relaxed);
                wake();
            });
    }
    wf
}

/// Accumulates per-bucket peak and RMS.
struct Acc {
    bucket: usize,
    peak: f32,
    sq: f64,
    n: u64,
}

impl Acc {
    fn store(&mut self, wf: &Waveform) {
        if self.n > 0 && self.bucket < BUCKETS {
            wf.rms[self.bucket].store(((self.sq / self.n as f64).sqrt() as f32).to_bits(), Relaxed);
            wf.peak[self.bucket].store(self.peak.to_bits(), Relaxed);
        }
        self.peak = 0.0;
        self.sq = 0.0;
        self.n = 0;
    }
}

fn slice_worker(wf: &Waveform, path: PathBuf, rate: u32, total: u64, k: usize, workers: usize) {
    let b0 = BUCKETS * k / workers;
    let b1 = BUCKETS * (k + 1) / workers;
    let Ok(mut o) = decode::open(&path) else {
        return;
    };

    let start_frame = total * b0 as u64 / BUCKETS as u64;
    let mut frame = 0u64;
    if start_frame > 0 {
        let secs = start_frame as f64 / rate as f64;
        let Some(time) = Time::try_from_secs_f64(secs) else {
            return;
        };
        let to = SeekTo::Time {
            time,
            track_id: Some(o.track_id),
        };
        match o.format.seek(SeekMode::Coarse, to) {
            Ok(s) => {
                frame = o
                    .time_base
                    .and_then(|tb| tb.calc_time(s.actual_ts))
                    .map(|t| (t.as_secs_f64() * rate as f64) as u64)
                    .unwrap_or(start_frame);
            }
            Err(_) => return,
        }
    }

    let mut acc = Acc {
        bucket: usize::MAX,
        peak: 0.0,
        sq: 0.0,
        n: 0,
    };
    let mut buf: Vec<f32> = Vec::new();
    while !wf.cancel.load(Relaxed) {
        let packet = match o.format.next_packet() {
            Ok(Some(p)) => p,
            _ => break,
        };
        if packet.track_id != o.track_id {
            continue;
        }
        let Ok(decoded) = o.decoder.decode(&packet) else {
            continue;
        };
        let ch = decoded.spec().channels().count().max(1);
        decoded.copy_to_vec_interleaved(&mut buf);
        for f in buf.chunks_exact(ch) {
            let b = (frame * BUCKETS as u64 / total) as usize;
            frame += 1;
            if b < b0 {
                continue;
            }
            if b != acc.bucket {
                acc.store(wf);
                acc.bucket = b;
                if b >= b1 {
                    return;
                }
            }
            let mut m = 0.0f32;
            for &s in f {
                m = m.max(s.abs());
            }
            acc.peak = acc.peak.max(m);
            acc.sq += (m * m) as f64;
            acc.n += 1;
        }
    }
    acc.store(wf);
}

fn unknown_length_worker(wf: &Waveform, path: PathBuf, rate: u32) {
    let Ok(mut o) = decode::open(&path) else {
        return;
    };
    const BLOCK: usize = 2048;
    let mut blocks: Vec<(f32, f64, u64)> = Vec::new();
    let (mut peak, mut sq, mut n) = (0.0f32, 0.0f64, 0u64);
    let mut frames = 0u64;
    let mut buf: Vec<f32> = Vec::new();
    while !wf.cancel.load(Relaxed) {
        let packet = match o.format.next_packet() {
            Ok(Some(p)) => p,
            _ => break,
        };
        if packet.track_id != o.track_id {
            continue;
        }
        let Ok(decoded) = o.decoder.decode(&packet) else {
            continue;
        };
        let ch = decoded.spec().channels().count().max(1);
        decoded.copy_to_vec_interleaved(&mut buf);
        for f in buf.chunks_exact(ch) {
            let m = f.iter().fold(0.0f32, |a, s| a.max(s.abs()));
            peak = peak.max(m);
            sq += (m * m) as f64;
            n += 1;
            frames += 1;
            if n as usize == BLOCK {
                blocks.push((peak, sq, n));
                (peak, sq, n) = (0.0, 0.0, 0);
            }
        }
    }
    if n > 0 {
        blocks.push((peak, sq, n));
    }
    if blocks.is_empty() || wf.cancel.load(Relaxed) {
        return;
    }
    wf.measured_secs
        .store((frames as f64 / rate as f64).to_bits(), Relaxed);
    for b in 0..BUCKETS {
        let i0 = b * blocks.len() / BUCKETS;
        let i1 = ((b + 1) * blocks.len() / BUCKETS)
            .max(i0 + 1)
            .min(blocks.len());
        let (mut p, mut s, mut c) = (0.0f32, 0.0f64, 0u64);
        for &(bp, bs, bn) in &blocks[i0.min(blocks.len() - 1)..i1] {
            p = p.max(bp);
            s += bs;
            c += bn;
        }
        wf.rms[b].store(((s / c.max(1) as f64).sqrt() as f32).to_bits(), Relaxed);
        wf.peak[b].store(p.to_bits(), Relaxed);
    }
}
