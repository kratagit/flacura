//! File opening, probing and track quality information.

use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia::core::codecs::audio::well_known::*;
use symphonia::core::codecs::audio::{
    AudioCodecId, AudioCodecParameters, AudioDecoder, AudioDecoderOptions,
};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTag};
use symphonia::core::units::TimeBase;

/// Extensions the player accepts when building a folder queue or handling drops.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "mp2", "mp1", "wav", "wave", "ogg", "oga", "m4a", "m4b", "mp4", "aac", "alac",
    "aif", "aiff", "aifc", "caf", "mka", "webm",
];

pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

#[derive(Clone, Debug)]
pub struct TrackInfo {
    pub path: PathBuf,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub codec: String,
    pub sample_rate: u32,
    pub bits: Option<u32>,
    pub channels: usize,
    pub duration: Option<f64>,
    pub total_frames: Option<u64>,
    pub lossless: bool,
    pub bitrate_kbps: Option<u32>,
}

impl TrackInfo {
    /// Lossless and above CD quality (more than 16 bits or more than 48 kHz).
    pub fn hires(&self) -> bool {
        self.lossless && (self.bits.unwrap_or(16) > 16 || self.sample_rate > 48_000)
    }

    /// Human readable quality string, e.g. "FLAC · 24-bit / 96 kHz".
    pub fn quality_label(&self) -> String {
        let rate = format_rate(self.sample_rate);
        if self.lossless {
            match self.bits {
                Some(b) => format!("{} · {}-bit / {}", self.codec, b, rate),
                None => format!("{} · {}", self.codec, rate),
            }
        } else {
            match self.bitrate_kbps {
                Some(k) => format!("{} · {} kbps · {}", self.codec, k, rate),
                None => format!("{} · {}", self.codec, rate),
            }
        }
    }
}

fn format_rate(hz: u32) -> String {
    if hz.is_multiple_of(1000) {
        format!("{} kHz", hz / 1000)
    } else {
        format!("{:.1} kHz", hz as f64 / 1000.0)
    }
}

pub struct Opened {
    pub format: Box<dyn FormatReader>,
    pub decoder: Box<dyn AudioDecoder>,
    pub track_id: u32,
    pub time_base: Option<TimeBase>,
    pub info: TrackInfo,
}

pub fn open(path: &Path) -> Result<Opened, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let file_len = file.metadata().map(|m| m.len()).ok();
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("Unsupported format: {e}"))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or("No audio track")?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("No audio codec parameters")?
        .clone();
    let track_id = track.id;
    let time_base = track.time_base;
    let num_frames = track.num_frames;
    let track_duration = track.duration;

    let decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| format!("Unsupported codec: {e}"))?;

    let sample_rate = params.sample_rate.unwrap_or(44_100);
    let channels = params.channels.as_ref().map(|c| c.count()).unwrap_or(2);

    let duration = match (num_frames, track_duration, time_base) {
        (Some(n), _, _) if sample_rate > 0 => Some(n as f64 / sample_rate as f64),
        (_, Some(d), Some(tb)) => tb
            .calc_time(symphonia::core::units::Timestamp::new(d.get() as i64))
            .map(|t| t.as_secs_f64()),
        _ => None,
    };
    let total_frames = num_frames.or_else(|| duration.map(|d| (d * sample_rate as f64) as u64));

    let lossless = is_lossless(params.codec);
    let bits = if lossless {
        lossless_bits(&params)
    } else {
        None
    };
    let bitrate_kbps = match (file_len, duration) {
        (Some(len), Some(d)) if d > 0.5 => Some((len as f64 * 8.0 / d / 1000.0).round() as u32),
        _ => None,
    };

    let mut info = TrackInfo {
        path: path.to_path_buf(),
        title: String::new(),
        artist: None,
        album: None,
        codec: codec_name(params.codec, path),
        sample_rate,
        bits,
        channels,
        duration,
        total_frames,
        lossless,
        bitrate_kbps,
    };

    // Walk every metadata revision (e.g. ID3v2 at the front, Vorbis comments, ...).
    {
        let mut md = format.metadata();
        loop {
            if let Some(rev) = md.current() {
                apply_tags(&mut info, rev);
            }
            if md.is_latest() {
                break;
            }
            md.pop();
        }
    }
    if info.title.is_empty() {
        info.title = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
    }

    Ok(Opened {
        format,
        decoder,
        track_id,
        time_base,
        info,
    })
}

fn apply_tags(info: &mut TrackInfo, rev: &MetadataRevision) {
    let all = rev
        .media
        .tags
        .iter()
        .chain(rev.per_track.iter().flat_map(|t| t.metadata.tags.iter()));
    for tag in all {
        match &tag.std {
            Some(StandardTag::TrackTitle(s)) if info.title.is_empty() => info.title = s.to_string(),
            Some(StandardTag::Artist(s)) if info.artist.is_none() => {
                info.artist = Some(s.to_string())
            }
            Some(StandardTag::Album(s)) if info.album.is_none() => info.album = Some(s.to_string()),
            _ => {}
        }
    }
}

fn lossless_bits(params: &AudioCodecParameters) -> Option<u32> {
    if let Some(b) = params.bits_per_sample {
        return Some(b);
    }
    // ALAC keeps the bit depth in its magic cookie, optionally wrapped in an 'alac' atom.
    if params.codec == CODEC_ID_ALAC
        && let Some(extra) = params.extra_data.as_deref()
    {
        let off = if extra.len() >= 36 && &extra[4..8] == b"alac" {
            12
        } else {
            0
        };
        if let Some(&b) = extra.get(off + 5).filter(|&&b| b > 0) {
            return Some(b as u32);
        }
    }
    use symphonia::core::audio::sample::SampleFormat::*;
    match params.sample_format? {
        U8 | S8 => Some(8),
        U16 | S16 => Some(16),
        U24 | S24 => Some(24),
        U32 | S32 | F32 => Some(32),
        F64 => Some(64),
    }
}

fn is_lossless(id: AudioCodecId) -> bool {
    // PCM family (except the companded A-law / mu-law variants) and the lossless codec block.
    (id >= CODEC_ID_PCM_S32LE && id <= CODEC_ID_PCM_F64BE_PLANAR)
        || (id >= CODEC_ID_FLAC && id <= CODEC_ID_TRUEHD)
}

fn codec_name(id: AudioCodecId, path: &Path) -> String {
    let name = match id {
        CODEC_ID_FLAC => "FLAC",
        CODEC_ID_ALAC => "ALAC",
        CODEC_ID_WAVPACK => "WavPack",
        CODEC_ID_MONKEYS_AUDIO => "APE",
        CODEC_ID_TTA => "TTA",
        CODEC_ID_MP1 => "MP1",
        CODEC_ID_MP2 => "MP2",
        CODEC_ID_MP3 => "MP3",
        CODEC_ID_AAC => "AAC",
        CODEC_ID_VORBIS => "Vorbis",
        CODEC_ID_OPUS => "Opus",
        _ if is_lossless(id) => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            match ext.as_str() {
                "wav" | "wave" => "WAV",
                "aif" | "aiff" | "aifc" => "AIFF",
                _ => "PCM",
            }
        }
        _ => {
            return symphonia::default::get_codecs()
                .get_audio_decoder(id)
                .map(|d| d.codec.info.short_name.to_ascii_uppercase())
                .unwrap_or_else(|| "?".into());
        }
    };
    name.to_string()
}

#[cfg(test)]
mod tests {
    /// Prints quality info and waveform timing for every file in `$FLACURA_TEST_DIR`.
    /// Run with: `cargo test --release probe_dir -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_dir() {
        let Some(dir) = std::env::var_os("FLACURA_TEST_DIR") else {
            return;
        };
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort();
        for f in files {
            let t = std::time::Instant::now();
            match super::open(&f) {
                Ok(o) => {
                    let open_ms = t.elapsed().as_secs_f64() * 1e3;
                    let i = &o.info;
                    let t = std::time::Instant::now();
                    let wf = crate::waveform::spawn(i, std::sync::Arc::new(|| {}));
                    while wf.is_running() {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    let filled = (0..crate::waveform::BUCKETS)
                        .filter(|&b| wf.get(b).is_some())
                        .count();
                    println!(
                        "{:<18} open {:5.1} ms | {:<34} lossless={} hires={} dur={:?} ch={} '{}' / {:?} | waveform {:6.1} ms, {}/{} buckets",
                        f.file_name().unwrap().to_string_lossy(),
                        open_ms,
                        i.quality_label(),
                        i.lossless,
                        i.hires(),
                        i.duration.map(|d| (d * 10.0).round() / 10.0),
                        i.channels,
                        i.title,
                        i.artist,
                        t.elapsed().as_secs_f64() * 1e3,
                        filled,
                        crate::waveform::BUCKETS
                    );
                }
                Err(e) => println!("{:<18} ERROR {e}", f.file_name().unwrap().to_string_lossy()),
            }
        }
    }
}
