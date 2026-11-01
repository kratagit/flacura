//! Turns what the device is playing into how the background moves.
//!
//! Low-frequency energy and bass onsets drive most of the background motion;
//! broadband loudness adds a smaller response. The detector stays fast while
//! a separate visual envelope smooths movement and beat tails.

use std::collections::VecDeque;
use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use crate::audio::{Player, VIS_LEN};

const COLS_PER_SEC: u32 = 100;
const BANDS: usize = 32;
const F_LO: f32 = 40.0;
const F_HI: f32 = 12_000.0;
/// RMS loudness (dBFS) mapped to energy 0 and 1.
const QUIET_DB: f32 = -42.0;
const LOUD_DB: f32 = -12.0;

/// Smooth visual motion independently of the fast audio/onset detector.
pub struct BackgroundMotion {
    pub energy: f32,
    pub pulse: f32,
    pub speed: f32,
}

impl Default for BackgroundMotion {
    fn default() -> Self {
        Self {
            energy: 0.0,
            pulse: 0.0,
            speed: 0.3,
        }
    }
}

impl BackgroundMotion {
    pub fn update(&mut self, dt: f32, energy: f32, pulse: f32, tempo: f32) {
        let follow = |value: &mut f32, target: f32, attack: f32, release: f32| {
            let tau = if target > *value { attack } else { release };
            *value += (target - *value) * (1.0 - (-dt / tau).exp());
        };
        // Fast-paced tracks get snappier attack and release envelopes.
        let tempo = tempo.clamp(0.0, 1.0);
        let responsiveness = 1.0 + 0.85 * tempo;
        follow(
            &mut self.energy,
            energy,
            0.018 / responsiveness,
            0.095 / responsiveness,
        );
        follow(
            &mut self.pulse,
            pulse,
            0.020 / responsiveness,
            0.16 / responsiveness,
        );
        let speed = 0.3
            + (2.5 * self.energy + 7.5 * self.energy * self.energy) * (1.0 + 0.9 * tempo)
            + (3.0 + 3.0 * tempo) * self.pulse
            + 3.5 * tempo * self.energy.max(0.25);
        follow(
            &mut self.speed,
            speed,
            0.055 / responsiveness,
            0.14 / responsiveness,
        );
    }
}

pub struct Motion {
    rate: u32,
    fft_len: usize,
    hop: usize,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    norm2: f32,
    bass_norm: f32,
    bass_bins: std::ops::Range<usize>,
    bass_db: f32,
    bass_short_db: f32,
    bass_long_db: Option<f32>,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    bands: Vec<(usize, usize)>,
    band_db: Vec<f32>,
    prev_db: Vec<f32>,
    flux: VecDeque<f32>,
    since_onset: f32,
    onset_rate: f32,
    last_count: Option<u64>,
    raw_count: u64,
    /// Seconds since new audio last arrived.
    idle: f32,
    /// Loudness of the latest column (dBFS).
    column_db: f32,
    /// Smoothed loudness: fast attack, slower release (dBFS).
    short_db: f32,
    /// Loudness over the last few seconds (dBFS); unset until audio arrives.
    long_db: Option<f32>,

    /// How loud / intense the music is right now (0..1).
    pub energy: f32,
    /// Low-frequency energy (roughly 35–250 Hz), independent of treble level.
    pub bass_energy: f32,
    /// Short push on beats and attacks, decays in ~0.25 s.
    pub pulse: f32,
    /// Estimated rhythmic pace / density of the music (0..1).
    pub tempo: f32,
}

impl Motion {
    pub fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(1024);
        let mut m = Motion {
            rate: 0,
            fft_len: 0,
            hop: 1,
            fft,
            window: Vec::new(),
            norm2: 1.0,
            bass_norm: 1.0,
            bass_bins: 1..2,
            bass_db: -120.0,
            bass_short_db: -120.0,
            bass_long_db: None,
            input: Vec::new(),
            output: Vec::new(),
            scratch: Vec::new(),
            buf_l: Vec::new(),
            buf_r: Vec::new(),
            bands: Vec::new(),
            band_db: vec![-120.0; BANDS],
            prev_db: vec![-120.0; BANDS],
            flux: VecDeque::with_capacity(64),
            since_onset: 1.0,
            onset_rate: 0.0,
            last_count: None,
            raw_count: 0,
            idle: 1.0,
            column_db: -120.0,
            short_db: -120.0,
            long_db: None,
            energy: 0.0,
            bass_energy: 0.0,
            pulse: 0.0,
            tempo: 0.0,
        };
        m.configure(48_000);
        m
    }

    fn configure(&mut self, rate: u32) {
        self.rate = rate.max(8_000);
        // ~23 ms window: 1024 at 44.1/48 kHz, 2048 at 96 kHz, ...
        let mut n = 512;
        while (n as f32) < self.rate as f32 * 0.021 && n * 2 <= VIS_LEN / 2 {
            n *= 2;
        }
        self.fft_len = n;
        self.hop = (self.rate / COLS_PER_SEC).max(1) as usize;
        self.fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
        self.input = self.fft.make_input_vec();
        self.output = self.fft.make_output_vec();
        self.scratch = self.fft.make_scratch_vec();
        self.window = (0..n)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())
            .collect();
        let norm = 2.0 / self.window.iter().sum::<f32>();
        self.norm2 = norm * norm;
        self.bass_norm = 2.0 / (n as f32 * self.window.iter().map(|x| x * x).sum::<f32>());
        self.buf_l = vec![0.0; n];
        self.buf_r = vec![0.0; n];
        let bin_hz = self.rate as f32 / n as f32;
        let bins = self.output.len();
        let bass_lo = (35.0 / bin_hz).floor().max(1.0) as usize;
        let bass_hi = ((250.0 / bin_hz).ceil() as usize + 1).min(bins);
        self.bass_bins = bass_lo..bass_hi;
        let f_hi = F_HI.min(self.rate as f32 * 0.45);
        let ratio = f_hi / F_LO;
        self.bands = (0..BANDS)
            .map(|i| {
                let f0 = F_LO * ratio.powf(i as f32 / BANDS as f32);
                let f1 = F_LO * ratio.powf((i + 1) as f32 / BANDS as f32);
                let lo = ((f0 / bin_hz) as usize).clamp(1, bins - 1);
                let hi = ((f1 / bin_hz) as usize).clamp(lo, bins - 1);
                (lo, hi)
            })
            .collect();
        self.last_count = None;
    }

    /// True while something still changes on screen.
    pub fn is_moving(&self) -> bool {
        self.energy > 0.005 || self.bass_energy > 0.005 || self.pulse > 0.005 || self.tempo > 0.005
    }

    /// Consumes newly played audio and advances the smoothing by `dt` seconds.
    pub fn update(&mut self, player: &Player, dt: f32, playing: bool) {
        let rate = player.out_rate();
        if rate != self.rate {
            self.configure(rate);
        }
        let count = player.vis_count();
        let hop = self.hop as u64;
        let oldest = count.saturating_sub((VIS_LEN - self.fft_len) as u64);
        let mut last = match self.last_count {
            Some(c) if count >= c => c.max(oldest),
            _ => count,
        };
        // After a stall, only catch up on the most recent second.
        last = last.max(count.saturating_sub(COLS_PER_SEC as u64 * hop));
        while last + hop <= count {
            last += hop;
            let n = self.fft_len;
            player.read_vis(last, &mut self.buf_l[..n], &mut self.buf_r[..n]);
            for i in 0..n {
                self.input[i] = (self.buf_l[i] + self.buf_r[i]) * 0.5;
            }
            self.column();
        }
        self.last_count = Some(last);
        if count != self.raw_count {
            self.raw_count = count;
            self.idle = 0.0;
        } else {
            self.idle += dt;
        }
        let active = playing && self.idle < 0.25;
        self.advance(dt, active);
    }

    fn advance(&mut self, dt: f32, active: bool) {
        let db = if active { self.column_db } else { -120.0 };
        // Rise within ~15 ms, fall within ~120 ms: follows hits and drops
        // almost immediately without flickering on every waveform cycle.
        let tau = if db > self.short_db { 0.015 } else { 0.12 };
        self.short_db += (db - self.short_db) * (1.0 - (-dt / tau).exp());
        let long = match self.long_db {
            Some(l) if active => l + (self.short_db - l) * (1.0 - (-dt / 4.0).exp()),
            Some(l) => l,
            None if active => self.column_db,
            None => self.short_db,
        };
        self.long_db = Some(long);
        // Absolute loudness carries most of it (calm music is usually quiet);
        // the change against the last few seconds makes swells and drops show.
        let absolute = smoothstep(QUIET_DB, LOUD_DB, self.short_db);
        let relative = (0.5 + (self.short_db - long) / 12.0).clamp(0.0, 1.0);
        let target = if active {
            (0.7 * absolute + 0.3 * relative).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.energy += (target - self.energy) * (1.0 - (-dt / 0.02).exp());
        let bass_db = if active { self.bass_db } else { -120.0 };
        let bass_tau = if bass_db > self.bass_short_db {
            0.015
        } else {
            0.12
        };
        self.bass_short_db += (bass_db - self.bass_short_db) * (1.0 - (-dt / bass_tau).exp());
        let bass_long = match self.bass_long_db {
            Some(l) if active && self.bass_db > -65.0 => {
                l + (self.bass_short_db - l) * (1.0 - (-dt / 3.5).exp())
            }
            Some(l) => l,
            None if active && self.bass_db > -65.0 => self.bass_db,
            None => self.bass_short_db,
        };
        self.bass_long_db = Some(bass_long);
        // Absolute bass loudness plus dynamic swings (swells and drops relative
        // to the recent average), so sustained loud passages don't stay pinned at max.
        let bass_abs = smoothstep(-48.0, -10.0, self.bass_short_db);
        let bass_rel = if self.bass_short_db > -55.0 {
            (0.35 + (self.bass_short_db - bass_long) / 14.0).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let bass_target = if active {
            (0.45 * bass_abs + 0.55 * bass_rel).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.bass_energy += (bass_target - self.bass_energy) * (1.0 - (-dt / 0.018).exp());
        if self.energy < 0.003 && !active {
            self.energy = 0.0;
        }
        if self.bass_energy < 0.003 && !active {
            self.bass_energy = 0.0;
        }
        self.pulse *= (-dt / 0.15).exp();
        if self.pulse < 0.005 {
            self.pulse = 0.0;
        }
        if !active {
            self.onset_rate *= (-dt / 0.35).exp();
        }
        let target_tempo = if active {
            ((self.onset_rate - 0.8) / 4.2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let tempo_tau = if target_tempo > self.tempo {
            0.12
        } else {
            0.40
        };
        self.tempo += (target_tempo - self.tempo) * (1.0 - (-dt / tempo_tau).exp());
        if self.tempo < 0.003 && !active {
            self.tempo = 0.0;
        }
    }

    /// Analyzes one hop of mono audio already placed in `self.input`.
    fn column(&mut self) {
        let n = self.fft_len;
        let mut sq = 0.0;
        for i in 0..n {
            sq += self.input[i] * self.input[i];
            self.input[i] *= self.window[i];
        }
        self.column_db = 10.0 * (sq / n as f32 + 1e-12).log10();
        if self
            .fft
            .process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch)
            .is_err()
        {
            return;
        }
        let bass_power = self.output[self.bass_bins.clone()]
            .iter()
            .map(|bin| bin.norm_sqr())
            .sum::<f32>()
            * self.bass_norm;
        self.bass_db = 10.0 * (bass_power + 1e-12).log10();
        for (b, &(lo, hi)) in self.bands.iter().enumerate() {
            let p = self.output[lo..=hi]
                .iter()
                .map(|c| c.norm_sqr())
                .fold(0.0f32, f32::max);
            self.band_db[b] = 10.0 * (p * self.norm2 + 1e-14).log10();
        }

        // Bass/kick pulse: positive low-band spectral flux above its baseline,
        // plus broadband rhythmic flux to estimate the track's tempo/pace.
        let floor = self.column_db - 45.0;
        let mut flux = 0.0;
        let mut broad_flux = 0.0;
        let mut bass_bands = 0;
        for b in 0..BANDS {
            let d = self.band_db[b].max(floor) - self.prev_db[b].max(floor);
            if d > 0.0 {
                broad_flux += d;
                if self.bands[b].0 as f32 * self.rate as f32 / n as f32 <= 250.0 {
                    flux += d * (1.5 - b as f32 / BANDS as f32);
                }
            }
            if self.bands[b].0 as f32 * self.rate as f32 / n as f32 <= 250.0 {
                bass_bands += 1;
            }
        }
        flux /= bass_bands.max(1) as f32;
        broad_flux /= BANDS as f32;
        let col_dt = 1.0 / COLS_PER_SEC as f32;
        self.onset_rate *= (-col_dt / 1.4).exp();
        self.prev_db.copy_from_slice(&self.band_db);
        let (mean, sd) = mean_sd(&self.flux);
        self.since_onset += col_dt;
        let bass_onset = self.flux.len() >= 20
            && flux > mean + 1.15 * sd
            && flux > 0.42
            && self.since_onset > 0.08
            && self.bass_db > -52.0;
        let rhythm_onset = self.flux.len() >= 20
            && broad_flux > 0.55
            && self.since_onset > 0.08
            && self.column_db > -48.0;
        if bass_onset {
            let strength = ((flux - mean) / (3.0 * sd + 1e-3)).clamp(0.4, 1.0);
            self.pulse = self.pulse.max(strength);
            self.onset_rate = (self.onset_rate + 1.15 * strength).min(8.0);
            self.since_onset = 0.0;
        } else if rhythm_onset && flux > mean + 0.8 * sd {
            self.pulse = self.pulse.max(0.35);
            self.onset_rate = (self.onset_rate + 0.85).min(8.0);
            self.since_onset = 0.0;
        }
        if self.flux.len() == 60 {
            self.flux.pop_front();
        }
        self.flux.push_back(flux);
    }

    /// Feeds raw mono samples directly (tests and calibration), returning the
    /// energy after every 10 ms column.
    #[cfg(test)]
    pub fn analyze(&mut self, samples: &[f32], rate: u32) -> Vec<f32> {
        if rate != self.rate {
            self.configure(rate);
        }
        let (n, hop) = (self.fft_len, self.hop);
        let mut trace = Vec::new();
        let mut end = n;
        let mut col = 0;
        while end <= samples.len() {
            self.input.copy_from_slice(&samples[end - n..end]);
            self.column();
            self.advance(hop as f32 / rate as f32, true);
            col += 1;
            let _ = col;
            trace.push(self.energy);
            end += hop;
        }
        trace
    }
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mean_sd(v: &VecDeque<f32>) -> (f32, f32) {
    if v.is_empty() {
        return (0.0, 0.0);
    }
    let n = v.len() as f32;
    let mean = v.iter().sum::<f32>() / n;
    let var = v.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n;
    (mean, var.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_reacts_quickly_without_an_instant_speed_jump() {
        let mut background = BackgroundMotion::default();
        background.update(1.0 / 60.0, 1.0, 1.0, 0.0);
        assert!(background.energy > 0.5);
        assert!(background.speed > 0.3 && background.speed < 3.0);
        for _ in 0..2 {
            background.update(1.0 / 60.0, 1.0, 1.0, 0.0);
        }
        assert!(background.energy > 0.9, "must respond within 50 ms");
        for _ in 0..120 {
            background.update(1.0 / 60.0, 0.0, 0.0, 0.0);
        }
        assert!(background.energy < 0.001 && background.pulse < 0.001);
        assert!((background.speed - 0.3).abs() < 0.001);
    }

    #[test]
    fn faster_tempo_increases_background_speed_and_response() {
        let mut slow_bg = BackgroundMotion::default();
        let mut fast_bg = BackgroundMotion::default();
        slow_bg.update(1.0 / 60.0, 0.6, 0.5, 0.0);
        fast_bg.update(1.0 / 60.0, 0.6, 0.5, 0.9);
        assert!(fast_bg.energy > slow_bg.energy);
        assert!(fast_bg.speed > slow_bg.speed * 1.4);

        // Pulse train at 4 Hz (fast 240 BPM eighth-notes) vs 1 Hz (slow 60 BPM).
        let pulse_train = |hz: f32| -> Vec<f32> {
            (0..(3.0 * RATE as f32) as usize)
                .map(|i| {
                    let t = i as f32 / RATE as f32;
                    let phase = (t * hz).fract();
                    let env = if phase < 0.18 { 0.6 } else { 0.02 };
                    env * (std::f32::consts::TAU * 85.0 * t).sin()
                })
                .collect()
        };
        let mut slow = Motion::new();
        slow.analyze(&pulse_train(1.0), RATE);
        let mut fast = Motion::new();
        fast.analyze(&pulse_train(4.0), RATE);
        assert!(
            fast.tempo > slow.tempo + 0.3,
            "fast tempo {} vs slow {}",
            fast.tempo,
            slow.tempo
        );
    }

    const RATE: u32 = 44_100;

    fn tone(amp: f32, secs: f32) -> Vec<f32> {
        tone_at(220.0, amp, secs)
    }

    fn tone_at(hz: f32, amp: f32, secs: f32) -> Vec<f32> {
        (0..(secs * RATE as f32) as usize)
            .map(|i| amp * (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin())
            .collect()
    }

    #[test]
    fn bass_detector_prefers_low_tones_at_equal_playback_level() {
        let mut bass = Motion::new();
        bass.analyze(&tone_at(80.0, 0.5, 1.0), RATE);
        let mut treble = Motion::new();
        treble.analyze(&tone_at(3000.0, 0.5, 1.0), RATE);
        assert!(bass.bass_energy > 0.5, "bass: {}", bass.bass_energy);
        assert!(treble.bass_energy < 0.05, "treble: {}", treble.bass_energy);
    }

    #[test]
    fn bass_reacts_within_eighty_milliseconds() {
        let mut bass = Motion::new();
        let mut signal = tone_at(80.0, 0.02, 1.0);
        signal.extend(tone_at(80.0, 0.5, 0.08));
        bass.analyze(&signal, RATE);
        assert!(bass.bass_energy > 0.8, "bass: {}", bass.bass_energy);
    }

    #[test]
    fn loud_is_more_energetic_than_quiet() {
        let quiet = *Motion::new()
            .analyze(&tone(0.02, 3.0), RATE)
            .last()
            .unwrap();
        let loud = *Motion::new().analyze(&tone(0.5, 3.0), RATE).last().unwrap();
        assert!(quiet < 0.3, "quiet {quiet}");
        assert!(loud > 0.8, "loud {loud}");
    }

    #[test]
    fn reacts_within_tens_of_milliseconds() {
        let mut signal = tone(0.02, 3.0);
        signal.extend(tone(0.5, 1.0));
        let trace = Motion::new().analyze(&signal, RATE);
        // Columns are 10 ms: the loud part starts at column 300.
        let before = trace[295];
        let jump = trace.iter().skip(300).position(|&e| e - before > 0.4);
        println!("rise: +0.4 after {jump:?} columns of 10 ms");
        assert!(
            jump.is_some_and(|c| c <= 5),
            "before {before}, reached +0.4 after {jump:?} columns"
        );
        // And falls back as quickly when the music drops.
        let mut drop = tone(0.5, 2.0);
        drop.extend(tone(0.02, 1.0));
        let trace = Motion::new().analyze(&drop, RATE);
        let high = trace[195];
        let fall = trace.iter().skip(200).position(|&e| high - e > 0.4);
        println!("fall: -0.4 after {fall:?} columns of 10 ms");
        assert!(
            fall.is_some_and(|c| c <= 25),
            "high {high}, fell after {fall:?} columns"
        );
    }

    /// Prints the energy every second for every file in `$FLACURA_TEST_DIR`
    /// (first 30 s). Run with:
    /// `cargo test --release probe_motion -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_motion() {
        let Some(dir) = std::env::var_os("FLACURA_TEST_DIR") else {
            return;
        };
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| crate::decode::is_audio_file(p))
            .collect();
        files.sort();
        for f in files {
            let Ok(mut o) = crate::decode::open(&f) else {
                continue;
            };
            let rate = o.info.sample_rate;
            let mut mono = Vec::new();
            let mut buf: Vec<f32> = Vec::new();
            while mono.len() < rate as usize * 30 {
                let Ok(Some(p)) = o.format.next_packet() else {
                    break;
                };
                let Ok(d) = o.decoder.decode(&p) else {
                    continue;
                };
                let ch = d.spec().channels().count().max(1);
                d.copy_to_vec_interleaved(&mut buf);
                mono.extend(
                    buf.chunks_exact(ch)
                        .map(|fr| fr.iter().sum::<f32>() / ch as f32),
                );
            }
            let trace = Motion::new().analyze(&mono, rate);
            let per_sec: Vec<String> = trace
                .iter()
                .step_by(100)
                .map(|e| format!("{:.0}", e * 9.0))
                .collect();
            println!(
                "{:<36} {}",
                f.file_name().unwrap().to_string_lossy(),
                per_sec.join("")
            );
        }
    }
}
