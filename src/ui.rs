//! Window, drawing and input handling.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, FontId, Key, Mesh, Pos2, Rect, Sense, Stroke, StrokeKind, TextFormat,
    ViewportCommand, pos2, text::LayoutJob, text::TextWrapping, vec2,
};
use interprocess::local_socket::Listener;

use crate::audio::{Event, Player};
use crate::aurora::{self, Aurora, STOPS};
use crate::config::Config;
use crate::decode::{TrackInfo, is_audio_file};
use crate::motion::{BackgroundMotion, Motion};
use crate::waveform::{self, BUCKETS, Waveform};

pub(crate) const BG: Color32 = Color32::from_rgb(14, 15, 18);
const TEXT: Color32 = Color32::from_rgb(232, 234, 238);
const DIM: Color32 = Color32::from_rgb(128, 133, 145);
const FAINT: Color32 = Color32::from_rgb(52, 56, 66);
const GOLD: Color32 = Color32::from_rgb(236, 186, 76);
const GOLD_LIGHT: Color32 = Color32::from_rgb(255, 222, 140);
const GOLD_DARK: Color32 = Color32::from_rgb(58, 44, 16);
const BLUE: Color32 = Color32::from_rgb(120, 170, 255);
const BLUE_LIGHT: Color32 = Color32::from_rgb(190, 215, 255);

/// Aurora color stops (sRGB 0..1), left to right.
const AURORA_GOLD: [[f32; 3]; STOPS] = [
    [0.55, 0.31, 0.06],
    [0.93, 0.73, 0.30],
    [1.00, 0.90, 0.64],
    [0.79, 0.54, 0.16],
];
const AURORA_BLUE: [[f32; 3]; STOPS] = [
    [0.17, 0.25, 0.62],
    [0.35, 0.63, 1.00],
    [0.62, 0.82, 1.00],
    [0.29, 0.36, 0.84],
];
const AURORA_IDLE: [[f32; 3]; STOPS] = [
    [0.23, 0.25, 0.33],
    [0.42, 0.45, 0.56],
    [0.50, 0.52, 0.62],
    [0.23, 0.25, 0.33],
];

/// Use Windows' native UI typeface without shipping a system-font asset.
fn configure_fonts(ctx: &egui::Context) {
    #[cfg(windows)]
    {
        let windows = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        if let Ok(bytes) = std::fs::read(windows.join("Fonts/segoeui.ttf")) {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("segoe-ui".into(), egui::FontData::from_owned(bytes).into());
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "segoe-ui".into());
            ctx.set_fonts(fonts);
        }
    }
    #[cfg(not(windows))]
    let _ = ctx;
}

/// Slider position to gain: a cubic curve tracks perceived loudness well.
pub fn volume_gain(pos: f32, muted: bool) -> f32 {
    if muted {
        0.0
    } else {
        pos.clamp(0.0, 1.0).powi(3)
    }
}

pub fn run(player: Player, listener: Option<Listener>, config: Config) -> eframe::Result {
    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(crate::APP_TITLE)
            .with_app_id("flacura")
            .with_inner_size([660.0, 360.0])
            .with_min_inner_size([480.0, 300.0])
            .with_drag_and_drop(true)
            .with_icon(Arc::new(icon())),
        centered: true,
        ..Default::default()
    };
    // winit can't receive file drops on native Wayland and draws no title bar
    // there, so prefer XWayland when it is available (FLACURA_WAYLAND=1 opts out).
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_some() && std::env::var_os("FLACURA_WAYLAND").is_none() {
        options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::x11::EventLoopBuilderExtX11;
            builder.with_x11();
        }));
    }
    eframe::run_native(
        "Flacura",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            configure_fonts(&ctx);
            ctx.set_visuals(egui::Visuals::dark());
            ctx.style_mut_of(egui::Theme::Dark, |style| {
                style.animation_time = 0.18;
                style.visuals.window_fill = Color32::from_rgba_unmultiplied(17, 22, 35, 242);
                style.visuals.panel_fill = Color32::TRANSPARENT;
                style.visuals.window_stroke = Stroke::new(1.0, Color32::from_white_alpha(35));
                style.visuals.window_corner_radius = egui::CornerRadius::same(16);
                style.spacing.item_spacing = vec2(10.0, 12.0);
                style.spacing.button_padding = vec2(14.0, 8.0);
                for (kind, size) in [
                    (egui::TextStyle::Body, 14.0),
                    (egui::TextStyle::Button, 13.0),
                    (egui::TextStyle::Small, 12.0),
                ] {
                    style.text_styles.insert(kind, FontId::proportional(size));
                }
            });
            let c = ctx.clone();
            player.set_waker(move || c.request_repaint());
            let (tx, ipc_rx) = mpsc::channel();
            if let Some(l) = listener {
                let c = ctx.clone();
                crate::ipc::listen(l, tx, move || c.request_repaint());
            }
            let c = ctx.clone();
            let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || c.request_repaint());
            Ok(Box::new(App::new(player, ipc_rx, wake, config)))
        }),
    )
}

/// Same artwork as the icon embedded in the Windows executable.
fn icon() -> egui::IconData {
    const N: usize = 64;
    egui::IconData {
        rgba: crate::icon::rgba(N),
        width: N as u32,
        height: N as u32,
    }
}

struct App {
    player: Player,
    ipc_rx: Receiver<Option<PathBuf>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    info: Option<TrackInfo>,
    error: Option<String>,
    queue: Vec<PathBuf>,
    index: usize,
    /// Consecutive unplayable files skipped (stops skipping through a folder of junk).
    skips: usize,
    /// Direction of the navigation that requested the pending load (+1 / -1),
    /// so an unplayable file is skipped the same way. `None` for direct opens.
    nav: Option<isize>,
    /// The pending load is the automatic advance after a track ended.
    auto_advance: bool,
    /// Output device problem reported by the engine.
    device_error: Option<String>,
    /// Fractional mouse-wheel notches not yet applied to the volume.
    scroll_acc: f32,
    ended: bool,
    wave: Option<Arc<Waveform>>,
    wave_gain: Option<f32>,
    /// Animated bar heights for the waveform: (peak, rms) per drawn bar.
    wave_anim: Vec<(f32, f32)>,
    wave_anim_settling: bool,
    /// Music analysis that drives the background.
    motion: Motion,
    background_motion: BackgroundMotion,
    /// Created on the second frame, so compiling the shader (~35 ms) doesn't
    /// delay the window; `None` without a usable GL context.
    aurora: Option<Arc<Aurora>>,
    aurora_tried: bool,
    frames: u32,
    /// Background state: animation clock, intro progress (0..1), colors.
    clock: f32,
    intro: f32,
    stops: [[f32; 3]; STOPS],
    seek_hold: Option<(f64, Instant)>,
    config: Config,
    settings_open: bool,
    association_task: Option<Receiver<Result<(), String>>>,
    association_status: Option<Result<(), String>>,
    track_reveal: f32,
    /// Settings changed and not yet written to disk.
    config_dirty: Option<Instant>,
    last_frame: Instant,
}

impl App {
    fn new(
        player: Player,
        ipc_rx: Receiver<Option<PathBuf>>,
        wake: Arc<dyn Fn() + Send + Sync>,
        config: Config,
    ) -> Self {
        App {
            player,
            ipc_rx,
            wake,
            info: None,
            error: None,
            queue: Vec::new(),
            index: 0,
            skips: 0,
            nav: None,
            auto_advance: false,
            device_error: None,
            scroll_acc: 0.0,
            ended: false,
            wave: None,
            wave_gain: None,
            wave_anim: Vec::new(),
            wave_anim_settling: false,
            motion: Motion::new(),
            background_motion: BackgroundMotion::default(),
            aurora: None,
            aurora_tried: false,
            frames: 0,
            clock: 0.0,
            intro: 0.0,
            stops: AURORA_IDLE,
            seek_hold: None,
            config,
            settings_open: false,
            association_task: None,
            association_status: None,
            track_reveal: 1.0,
            config_dirty: None,
            last_frame: Instant::now(),
        }
    }

    fn open(&mut self, path: PathBuf) {
        self.skips = 0;
        self.nav = None;
        self.auto_advance = false;
        self.player.load(path);
    }

    fn play_index(&mut self, i: usize) {
        if let Some(p) = self.queue.get(i).cloned() {
            self.index = i;
            self.player.load(p);
        }
    }

    fn next(&mut self) {
        if self.index + 1 < self.queue.len() {
            self.nav = Some(1);
            self.auto_advance = false;
            self.play_index(self.index + 1);
        }
    }

    fn prev(&mut self) {
        // Like most players: restart the track unless we are at its very start.
        if self.position() > 3.0 || self.index == 0 {
            self.seek(0.0);
        } else {
            self.nav = Some(-1);
            self.auto_advance = false;
            self.play_index(self.index - 1);
        }
    }

    fn seek(&mut self, t: f64) {
        let t = match self.duration() {
            Some(d) => t.clamp(0.0, (d - 0.05).max(0.0)),
            None => t.max(0.0),
        };
        self.player.seek(t);
        self.seek_hold = Some((t, Instant::now()));
        if self.ended {
            self.ended = false;
            self.player.set_paused(false);
        }
    }

    fn toggle_pause(&mut self) {
        if self.info.is_none() {
            return;
        }
        if self.ended {
            self.seek(0.0);
            return;
        }
        self.player.set_paused(!self.player.paused());
    }

    /// Pause and rewind to the start.
    fn stop(&mut self) {
        if self.info.is_none() {
            return;
        }
        self.player.set_paused(true);
        self.player.seek(0.0);
        self.seek_hold = Some((0.0, Instant::now()));
        self.ended = false;
    }

    fn set_volume(&mut self, pos: f32) {
        self.config.volume = pos.clamp(0.0, 1.0);
        self.config.muted = false;
        self.player
            .set_volume(volume_gain(self.config.volume, false));
        self.config_dirty = Some(Instant::now());
    }

    /// Steps the remembered level (unmuting if needed).
    fn change_volume(&mut self, delta: f32) {
        self.set_volume(self.config.volume + delta);
    }

    fn toggle_mute(&mut self) {
        self.config.muted = !self.config.muted;
        if !self.config.muted && self.config.volume < 0.02 {
            self.config.volume = 0.5;
        }
        self.player
            .set_volume(volume_gain(self.config.volume, self.config.muted));
        self.config_dirty = Some(Instant::now());
    }

    fn save_config_if_due(&mut self, force: bool) {
        if let Some(t) = self.config_dirty
            && (force || t.elapsed() > Duration::from_millis(500))
        {
            self.config.save();
            self.config_dirty = None;
        }
    }

    fn duration(&self) -> Option<f64> {
        let measured = self
            .wave
            .as_ref()
            .map(|w| f64::from_bits(w.measured_secs.load(std::sync::atomic::Ordering::Relaxed)))
            .filter(|&d| d > 0.0);
        self.info.as_ref().and_then(|i| i.duration).or(measured)
    }

    fn position(&self) -> f64 {
        // Show the requested position until the engine has applied the seek.
        if let Some((t, at)) = self.seek_hold {
            let age = at.elapsed();
            if age < Duration::from_millis(150)
                || (self.player.busy() && age < Duration::from_secs(2))
            {
                return t;
            }
        }
        if self.ended {
            return self.duration().unwrap_or(0.0);
        }
        self.player.position()
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        while let Some(ev) = self.player.poll_event() {
            match ev {
                Event::Loaded(info) => {
                    self.track_reveal = 0.0;
                    self.error = None;
                    self.ended = false;
                    self.skips = 0;
                    self.nav = None;
                    self.auto_advance = false;
                    self.seek_hold = None;
                    if let Some(w) = self.wave.take() {
                        w.cancel();
                    }
                    self.wave = Some(waveform::spawn(&info, self.wake.clone()));
                    self.wave_gain = None;
                    self.wave_anim.iter_mut().for_each(|v| *v = (0.0, 0.0));
                    self.sync_queue(&info.path);
                    let title = match &info.artist {
                        Some(a) => format!("{} — {}", info.title, a),
                        None => info.title.clone(),
                    };
                    ctx.send_viewport_cmd(ViewportCommand::Title(format!(
                        "{title} — {}",
                        crate::APP_TITLE
                    )));
                    self.info = Some(info);
                }
                Event::Failed(path, e) => {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    // While navigating, silently skip unplayable files in the
                    // same direction.
                    if let Some(d) = self.nav
                        && self.skips < self.queue.len()
                    {
                        let i = self.index as isize + d;
                        if i >= 0 && (i as usize) < self.queue.len() {
                            self.skips += 1;
                            self.play_index(i as usize);
                            continue;
                        }
                    }
                    self.error = Some(format!("{name}: {e}"));
                    match self.info.as_ref().map(|i| i.path.clone()) {
                        // Keep pointing at what is actually loaded.
                        Some(cur) => self.sync_queue(&cur),
                        None => self.sync_queue(&path),
                    }
                    if self.auto_advance {
                        // The previous track already finished.
                        self.ended = true;
                        self.player.set_paused(true);
                    }
                    self.nav = None;
                    self.auto_advance = false;
                }
                Event::Device(err) => self.device_error = err,
            }
        }
        while let Ok(msg) = self.ipc_rx.try_recv() {
            if let Some(p) = msg {
                self.open(p);
            }
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
        if self.player.take_finished() {
            if self.index + 1 < self.queue.len() {
                self.next();
                self.auto_advance = true;
            } else {
                self.ended = true;
                self.player.set_paused(true);
            }
        }
    }

    /// Rebuilds the folder queue if `path` is outside it, and points `index` at it.
    fn sync_queue(&mut self, path: &Path) {
        if let Some(i) = self.queue.iter().position(|p| p == path) {
            self.index = i;
            return;
        }
        let mut q: Vec<PathBuf> = path
            .parent()
            .and_then(|d| std::fs::read_dir(d).ok())
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| is_audio_file(p))
                    .collect()
            })
            .unwrap_or_default();
        q.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
        if !q.iter().any(|p| p == path) {
            q = vec![path.to_path_buf()];
        }
        self.index = q.iter().position(|p| p == path).unwrap_or(0);
        self.queue = q;
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let (dropped, keys, scroll) = ctx.input(|i| {
            // Shortcuts are plain keys; leave Ctrl/Alt/Cmd combinations alone.
            let plain = !(i.modifiers.ctrl || i.modifiers.alt || i.modifiers.command);
            // Toggles fire once per press, not on key auto-repeat.
            let once = |k: Key| {
                plain
                    && i.events.iter().any(|e| {
                        matches!(e, egui::Event::Key { key, pressed: true, repeat: false, .. } if *key == k)
                    })
            };
            let held = |k: Key| plain && i.key_pressed(k);
            let dropped: Vec<PathBuf> = i
                .raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect();
            let keys = [
                once(Key::Space),
                held(Key::ArrowLeft),
                held(Key::ArrowRight),
                held(Key::ArrowUp),
                held(Key::ArrowDown),
                held(Key::N) || held(Key::PageDown),
                held(Key::P) || held(Key::PageUp),
                once(Key::S),
                once(Key::M),
                i.modifiers.shift,
            ];
            // Wheel notches: lines as-is, pixels (touchpads, hi-res wheels)
            // at egui's own 40 px per line; horizontal scrolling is ignored.
            let scroll: f32 = i
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::MouseWheel { unit, delta, .. } if delta.y != 0.0 => {
                        Some(match unit {
                            egui::MouseWheelUnit::Line => delta.y,
                            egui::MouseWheelUnit::Point => delta.y / 40.0,
                            egui::MouseWheelUnit::Page => delta.y * 3.0,
                        })
                    }
                    _ => None,
                })
                .sum();
            (dropped, keys, scroll)
        });
        if let Some(p) = dropped.into_iter().next() {
            self.open(p);
        }
        if self.settings_open {
            return;
        }
        let [space, left, right, up, down, next, prev, stop, mute, shift] = keys;
        let step = if shift { 30.0 } else { 5.0 };
        if self.info.is_none() {
            // Nothing to control (and no visible feedback) yet.
            return;
        }
        if space {
            self.toggle_pause();
        }
        if left {
            self.seek(self.position() - step);
        }
        if right {
            self.seek(self.position() + step);
        }
        if up {
            self.change_volume(0.05);
        }
        if down {
            self.change_volume(-0.05);
        }
        if next {
            self.next();
        }
        if prev {
            self.prev();
        }
        if stop {
            self.stop();
        }
        if mute {
            self.toggle_mute();
        }
        self.scroll_acc += scroll;
        while self.scroll_acc.abs() >= 1.0 {
            let notch = self.scroll_acc.signum();
            self.change_volume(0.05 * notch);
            self.scroll_acc -= notch;
        }
    }
}

impl eframe::App for App {
    /// Runs before every frame and also while the window is minimized, so
    /// auto-advance and files opened from the file manager keep working.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_events(ctx);
        if let Some(result) = self
            .association_task
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.association_task = None;
            self.association_status = Some(result);
        }
        self.save_config_if_due(false);
        if self.config_dirty.is_some() {
            ctx.request_repaint_after_secs(0.6);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;
        self.track_reveal = (self.track_reveal + dt / 0.28).min(1.0);

        self.handle_input(&ctx);

        self.frames = self.frames.saturating_add(1);
        if self.config.animated_background && !self.aurora_tried && self.frames >= 2 {
            self.aurora_tried = true;
            self.aurora = frame.gl().and_then(|gl| Aurora::new(gl).ok());
        }

        let full = ui.max_rect();
        let painter = ui.painter().clone();
        let paused = self.player.paused();
        let playing = self.info.is_some() && !paused;
        let (accent, light) = self.accent();

        if self.config.animated_background {
            self.motion.update(&self.player, dt, playing);
            let bass_driven = 0.85 * self.motion.bass_energy + 0.15 * self.motion.energy;
            let tempo = if self.config.tempo_adaptive_speed {
                self.motion.tempo
            } else {
                0.0
            };
            self.background_motion
                .update(dt, bass_driven, self.motion.pulse, tempo);
        }

        let inner = full.shrink2(vec2(14.0, 12.0));
        let header = Rect::from_min_size(inner.min, vec2(inner.width() - 36.0, 44.0));
        let settings_center = pos2(inner.right() - 14.0, header.top() + 14.0);
        let controls = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 40.0), inner.max);
        let wave_h = (inner.height() * 0.20).clamp(44.0, 72.0);
        let wave_rect = Rect::from_min_max(
            pos2(inner.left(), controls.top() - 14.0 - wave_h),
            pos2(inner.right(), controls.top() - 14.0),
        );
        let middle = Rect::from_min_max(
            pos2(inner.left(), header.bottom() + 8.0),
            pos2(inner.right(), wave_rect.top() - 12.0),
        );

        // The background starts at the top edge, below the native title bar.
        let clear_bottom = ((wave_rect.top() - 18.0 - full.top()) / full.height()).min(0.56);
        let background_moving = self.draw_background(&painter, full, dt, clear_bottom);
        self.draw_settings(ui, settings_center);

        if self.info.is_none() {
            self.draw_empty(&painter, inner);
            if background_moving {
                ctx.request_repaint_after_secs(1.0 / 60.0 + ctx.input(|i| i.predicted_dt));
            }
            return;
        }

        // The open middle shows the background; clicking it toggles playback.
        if middle.height() > 8.0
            && ui
                .interact(middle, ui.id().with("middle"), Sense::CLICK)
                .clicked()
        {
            self.toggle_pause();
        }

        let mut title_painter = painter.clone();
        title_painter.set_opacity(self.track_reveal);
        self.draw_header(
            &title_painter,
            header.translate(vec2(0.0, (1.0 - self.track_reveal) * 7.0)),
            ctx.input(|i| i.time) as f32,
            playing,
        );
        self.draw_waveform(ui, &painter, wave_rect, (accent, light), dt);
        let dragging = self.draw_controls(ui, &painter, controls, accent, light);

        if let Some(e) = self.error.as_ref().or(self.device_error.as_ref()) {
            let r = Rect::from_center_size(middle.center(), vec2(middle.width(), 26.0));
            draw_glass_pill(
                &painter,
                r,
                8.0,
                Color32::from_rgba_unmultiplied(110, 26, 30, 175),
                Color32::from_rgba_unmultiplied(255, 130, 120, 48),
            );
            painter.text(
                r.center(),
                Align2::CENTER_CENTER,
                e,
                FontId::proportional(12.0),
                TEXT,
            );
        }

        let animating = self.wave.as_ref().is_some_and(|w| w.is_running())
            || background_moving
            || self.wave_anim_settling
            || self.track_reveal < 1.0
            || dragging;
        // Cap at ~60 fps (high refresh monitors would otherwise burn CPU);
        // when nothing moves, don't repaint at all. egui subtracts the
        // predicted frame time from the delay, so add it back.
        if playing || animating {
            let predicted = ctx.input(|i| i.predicted_dt);
            ctx.request_repaint_after_secs(1.0 / 60.0 + predicted);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_config_if_due(true);
    }
}

impl App {
    fn draw_settings(&mut self, ui: &mut egui::Ui, center: Pos2) {
        if self.settings_open && ui.input(|i| i.key_pressed(Key::Escape)) {
            self.settings_open = false;
        }
        let response = button(
            ui,
            ui.painter(),
            "settings",
            center,
            14.0,
            true,
            None,
            Icon::Settings,
        )
        .on_hover_text("Settings");
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Settings"));
        if response.clicked() {
            self.settings_open = !self.settings_open;
        }
        let ctx = ui.ctx().clone();
        let reveal =
            ctx.animate_bool_with_time(ui.id().with("settings-panel"), self.settings_open, 0.16);
        if reveal < 0.01 {
            return;
        }
        let full = ui.max_rect();
        let width = 304.0f32.min(full.width() - 28.0);
        let origin = pos2(
            full.right() - width - 14.0,
            center.y + 20.0 - (1.0 - reveal) * 6.0,
        );
        let (accent, light) = self.accent();
        let lossless = self.info.as_ref().is_some_and(|i| i.lossless);
        let (glass_fill, glass_border, glass_sheen) = if lossless {
            (
                Color32::from_rgba_unmultiplied(24, 21, 18, 195),
                Color32::from_rgba_unmultiplied(255, 222, 140, 42),
                Color32::from_rgba_unmultiplied(255, 235, 180, 18),
            )
        } else {
            (
                Color32::from_rgba_unmultiplied(15, 20, 32, 195),
                Color32::from_rgba_unmultiplied(190, 215, 255, 42),
                Color32::from_rgba_unmultiplied(210, 230, 255, 18),
            )
        };
        let mut panel_rect = Rect::NOTHING;
        egui::Area::new(egui::Id::new("fixed-settings"))
            .order(egui::Order::Foreground)
            .fixed_pos(origin)
            .movable(false)
            .show(&ctx, |ui| {
                ui.set_opacity(reveal);
                let frame_resp = egui::Frame::new()
                    .fill(glass_fill)
                    .stroke(Stroke::new(1.0, glass_border))
                    .corner_radius(16)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 10],
                        blur: 28,
                        spread: 0,
                        color: Color32::from_black_alpha(120),
                    })
                    .inner_margin(16)
                    .show(ui, |ui| {
                        ui.set_width(width - 32.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Settings")
                                    .size(15.5)
                                    .color(TEXT)
                                    .strong(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let (close_r, close_resp) =
                                        ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
                                    let close_hover = ui.ctx().animate_bool_with_time(
                                        close_resp.id.with("hover"),
                                        close_resp.hovered(),
                                        0.14,
                                    );
                                    if close_hover > 0.01 {
                                        ui.painter().circle_filled(
                                            close_r.center(),
                                            11.0,
                                            Color32::from_white_alpha((26.0 * close_hover) as u8),
                                        );
                                    }
                                    let c = close_r.center();
                                    let d = 3.8;
                                    let col = if close_resp.hovered() { TEXT } else { DIM };
                                    ui.painter().line_segment(
                                        [c + vec2(-d, -d), c + vec2(d, d)],
                                        Stroke::new(1.3, col),
                                    );
                                    ui.painter().line_segment(
                                        [c + vec2(-d, d), c + vec2(d, -d)],
                                        Stroke::new(1.3, col),
                                    );
                                    if close_resp.hovered() {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    if close_resp.on_hover_text("Close settings").clicked() {
                                        self.settings_open = false;
                                    }
                                },
                            );
                        });
                        ui.add_space(10.0);
                        if setting_toggle(
                            ui,
                            "Animated background",
                            &mut self.config.animated_background,
                            accent,
                            light,
                        ) {
                            self.config_dirty = Some(Instant::now());
                            self.motion = Motion::new();
                            self.background_motion = BackgroundMotion::default();
                        }
                        if self.config.animated_background {
                            ui.add_space(6.0);
                            if setting_toggle(
                                ui,
                                "Adapt speed to tempo",
                                &mut self.config.tempo_adaptive_speed,
                                accent,
                                light,
                            ) {
                                self.config_dirty = Some(Instant::now());
                            }
                        }
                        #[cfg(windows)]
                        {
                            ui.add_space(10.0);
                            let sep_y = ui.cursor().top();
                            ui.painter().line_segment(
                                [
                                    pos2(ui.max_rect().left(), sep_y),
                                    pos2(ui.max_rect().right(), sep_y),
                                ],
                                Stroke::new(1.0, Color32::from_white_alpha(16)),
                            );
                            ui.add_space(10.0);
                            if glass_action_button(
                                ui,
                                "Set default player",
                                self.association_task.is_none(),
                                accent,
                                light,
                            ) {
                                let (tx, rx) = mpsc::channel();
                                let ctx = ui.ctx().clone();
                                self.association_task = Some(rx);
                                self.association_status = None;
                                std::thread::spawn(move || {
                                    let _ = tx.send(crate::platform::register());
                                    ctx.request_repaint();
                                });
                            }
                            if self.association_task.is_some() {
                                ui.add_space(4.0);
                                ui.spinner();
                            }
                            ui.add_space(4.0);
                            match &self.association_status {
                                Some(Err(error)) => {
                                    ui.colored_label(Color32::from_rgb(250, 145, 135), error);
                                }
                                _ => {
                                    ui.label(
                                        egui::RichText::new(
                                            "Confirm the choice in Windows Settings.",
                                        )
                                        .size(11.5)
                                        .color(DIM),
                                    );
                                }
                            }
                        }
                    });
                panel_rect = frame_resp.response.rect;
                // Subtle liquid-glass specular sheen along the top edge.
                let sheen = Rect::from_min_max(
                    pos2(panel_rect.left() + 14.0, panel_rect.top() + 1.0),
                    pos2(panel_rect.right() - 14.0, panel_rect.top() + 2.0),
                );
                ui.painter().rect_filled(sheen, 1.0, glass_sheen);
            });
        // Close when clicking outside the panel and gear button.
        if self.settings_open
            && !response.clicked()
            && ctx.input(|i| {
                i.pointer.primary_clicked()
                    && i.pointer
                        .interact_pos()
                        .is_some_and(|p| !panel_rect.contains(p))
            })
        {
            self.settings_open = false;
        }
    }

    fn accent(&self) -> (Color32, Color32) {
        match &self.info {
            Some(i) if i.lossless => (GOLD, GOLD_LIGHT),
            _ => (BLUE, BLUE_LIGHT),
        }
    }

    fn draw_empty(&self, p: &egui::Painter, r: Rect) {
        let c = r.center();
        if let Some(e) = &self.error {
            p.text(
                c - vec2(0.0, 22.0),
                Align2::CENTER_CENTER,
                e,
                FontId::proportional(13.0),
                Color32::from_rgb(240, 120, 110),
            );
        }
        p.text(
            c,
            Align2::CENTER_CENTER,
            "Drop an audio file here",
            FontId::proportional(16.0),
            DIM,
        );
        p.text(
            c + vec2(0.0, 24.0),
            Align2::CENTER_CENTER,
            "or open one with Flacura  ·  FLAC  MP3  WAV  ALAC  AAC  OGG  AIFF",
            FontId::proportional(11.0),
            FAINT,
        );
    }

    fn draw_header(&self, p: &egui::Painter, r: Rect, time: f32, playing: bool) {
        let Some(info) = &self.info else { return };

        // Quality label (right aligned, no pill background).
        let label = info.quality_label();
        let badge_font = FontId::proportional(11.5);
        let fg = if info.lossless { GOLD_LIGHT } else { DIM };
        let g = p.layout_no_wrap(label, badge_font, fg);
        let hires_w = if info.hires() { 44.0 } else { 0.0 };
        let bw = g.size().x + hires_w;
        let badge = Rect::from_min_size(pos2(r.right() - bw, r.top() + 3.0), vec2(bw, 16.0));
        if info.hires() {
            let chip = Rect::from_min_size(pos2(badge.left(), badge.top() + 1.0), vec2(38.0, 14.0));
            let pulse = if playing {
                0.5 + 0.5 * (time * 2.2).sin()
            } else {
                0.6
            };
            for k in 1..=4 {
                let a = (pulse * 22.0 / k as f32) as u8;
                p.rect_filled(
                    chip.expand(k as f32 * 1.4),
                    4.0 + k as f32,
                    Color32::from_rgba_unmultiplied(236, 186, 76, a),
                );
            }
            p.rect_filled(chip, 4.0, GOLD);
            p.text(
                chip.center(),
                Align2::CENTER_CENTER,
                "HI-RES",
                FontId::proportional(9.5),
                GOLD_DARK,
            );
        }
        p.galley(
            pos2(r.right() - g.size().x, badge.center().y - g.size().y / 2.0),
            g,
            fg,
        );

        // Title and artist, truncated to the space left of the badge.
        let max_w = (badge.left() - r.left() - 12.0).max(40.0);
        let title_color = if info.lossless {
            Color32::from_rgb(250, 240, 220)
        } else {
            TEXT
        };
        let title = one_line(
            p,
            &info.title,
            FontId::proportional(20.0),
            title_color,
            max_w,
        );
        p.galley(r.left_top(), title, title_color);
        let sub = match (&info.artist, &info.album) {
            (Some(a), Some(al)) => format!("{a}  ·  {al}"),
            (Some(a), None) => a.clone(),
            (None, Some(al)) => al.clone(),
            (None, None) => String::new(),
        };
        // Queue position and non-stereo layouts, right aligned under the badge.
        let mut extra = Vec::new();
        if self.queue.len() > 1 {
            extra.push(format!("{} / {}", self.index + 1, self.queue.len()));
        }
        match info.channels {
            2 => {}
            1 => extra.push("mono".into()),
            6 => extra.push("5.1".into()),
            8 => extra.push("7.1".into()),
            c => extra.push(format!("{c} ch")),
        }
        let mut sub_w = max_w;
        if !extra.is_empty() {
            let g = p.layout_no_wrap(extra.join("  ·  "), FontId::proportional(11.0), DIM);
            sub_w = sub_w.min(r.width() - g.size().x - 16.0).max(40.0);
            p.galley(pos2(r.right() - g.size().x, r.top() + 25.0), g, DIM);
        }
        if !sub.is_empty() {
            let g = one_line(p, &sub, FontId::proportional(12.5), DIM, sub_w);
            p.galley(r.left_top() + vec2(0.0, 23.0), g, DIM);
        }
    }

    /// Paints the aurora behind everything. Returns true while it still
    /// changes (so the caller keeps repainting).
    fn draw_background(
        &mut self,
        p: &egui::Painter,
        full: Rect,
        dt: f32,
        clear_bottom: f32,
    ) -> bool {
        p.rect_filled(full, 0.0, Color32::from_rgb(9, 12, 20));
        if !self.config.animated_background {
            return false;
        }
        // Colors follow the track's quality, blending over ~0.5 s.
        let want = match &self.info {
            Some(i) if i.lossless => AURORA_GOLD,
            Some(_) => AURORA_BLUE,
            None => AURORA_IDLE,
        };
        let k = 1.0 - (-dt * 5.0).exp();
        let mut blending = false;
        for (cur, want) in self.stops.iter_mut().flatten().zip(want.iter().flatten()) {
            *cur += (want - *cur) * k;
            if (want - *cur).abs() > 0.002 {
                blending = true;
            } else {
                *cur = *want;
            }
        }

        let Some(aurora) = &self.aurora else {
            // Keep frames coming until the shader has been set up.
            return !self.aurora_tried;
        };
        // Loud music: fast, tall, bright. Quiet music: slow, low, dim.
        let m = &self.background_motion;
        let e = m.energy;
        self.clock += dt * m.speed;
        self.intro = (self.intro + dt / 2.4).min(1.0);
        aurora.paint(
            p,
            full,
            aurora::Params {
                time: self.clock,
                stops: self.stops,
                amplitude: 0.6 + 1.4 * e,
                blend: 0.4,
                // The wavy edge comes down into view as the music swells.
                height: 0.10 + 0.14 * e + 0.035 * m.pulse,
                glow: 0.3 + 0.7 * m.pulse,
                brightness: 0.25 + 0.6 * e + 0.5 * m.pulse,
                opacity: 0.65,
                reveal: self.intro,
                clear_bottom,
            },
        );
        self.motion.is_moving()
            || m.energy > 0.005
            || m.pulse > 0.005
            || self.intro < 1.0
            || blending
    }

    fn draw_waveform(
        &mut self,
        ui: &egui::Ui,
        p: &egui::Painter,
        r: Rect,
        (accent, light): (Color32, Color32),
        dt: f32,
    ) {
        let resp = ui.interact(r, ui.id().with("waveform"), Sense::CLICK | Sense::DRAG);
        let duration = self.duration();
        let frac_at = |x: f32| ((x - r.left()) / r.width()).clamp(0.0, 1.0) as f64;

        if let (Some(d), Some(pos)) = (duration, resp.interact_pointer_pos())
            && (resp.clicked() || resp.drag_stopped() || resp.dragged())
        {
            if resp.dragged() && !resp.drag_stopped() {
                self.seek_hold = Some((frac_at(pos.x) * d, Instant::now()));
            } else {
                self.seek(frac_at(pos.x) * d);
            }
        }

        let progress = match duration {
            Some(d) if d > 0.0 => (self.position() / d).clamp(0.0, 1.0) as f32,
            _ => 0.0,
        };
        let play_x = r.left() + progress * r.width();
        let hover_x = resp.hover_pos().map(|p| p.x);

        let bar_w = 2.0;
        let gap = 1.0;
        let n = ((r.width() + gap) / (bar_w + gap)).floor().max(1.0) as usize;
        if self.wave_anim.len() != n {
            self.wave_anim = vec![(0.0, 0.0); n];
        }
        let k = 1.0 - (-dt * 14.0).exp();
        let mid = r.center().y;
        let half = r.height() / 2.0;
        let mut settling = false;
        // Fit recorded energy to the display, ignoring isolated peak spikes.
        // Cache once background decoding finishes instead of sorting per frame.
        let gain = self.wave_gain.unwrap_or_else(|| {
            self.wave.as_ref().map_or(1.0, |wave| {
                let mut rms: Vec<f32> = (0..BUCKETS)
                    .filter_map(|b| wave.get(b).map(|(_, rms)| rms))
                    .collect();
                let gain = waveform_gain(&mut rms);
                if !wave.is_running() {
                    self.wave_gain = Some(gain);
                }
                gain
            })
        });
        let mut mesh = Mesh::default();
        let unplayed = Color32::from_rgb(100, 113, 137);
        let hover_col = Color32::from_rgb(155, 174, 204);

        for i in 0..n {
            let b0 = i * BUCKETS / n;
            let b1 = ((i + 1) * BUCKETS / n).max(b0 + 1).min(BUCKETS);
            let target = self.wave.as_ref().and_then(|w| {
                let mut acc: Option<(f32, f32)> = None;
                let mut count = 0.0;
                for b in b0..b1 {
                    if let Some((pk, rms)) = w.get(b) {
                        let a = acc.get_or_insert((0.0, 0.0));
                        a.0 = a.0.max(pk);
                        a.1 += rms * rms;
                        count += 1.0;
                    }
                }
                acc.map(|(peak, sum)| (peak, (sum / count).sqrt()))
            });
            let cur = &mut self.wave_anim[i];
            if let Some((tp, tr)) = target {
                let (tp, tr) = waveform_levels(tp, tr, gain);
                cur.0 += (tp - cur.0) * k;
                cur.1 += (tr - cur.1) * k;
                if (tp - cur.0).abs() > 0.003 {
                    settling = true;
                }
            }
            let x = r.left() + i as f32 * (bar_w + gap);
            let ph = (cur.0 * half).max(1.0);
            let rh = (cur.1 * half).max(1.0).min(ph);
            let played = x + bar_w <= play_x;
            let hovered = hover_x.is_some_and(|hx| x <= hx);
            let (outer, core) = if played {
                (accent.gamma_multiply(0.55), light)
            } else if hovered {
                (hover_col.gamma_multiply(0.6), hover_col)
            } else {
                (unplayed.gamma_multiply(0.6), unplayed)
            };
            mesh.add_colored_rect(
                Rect::from_min_max(pos2(x, mid - ph), pos2(x + bar_w, mid + ph)),
                outer,
            );
            mesh.add_colored_rect(
                Rect::from_min_max(pos2(x, mid - rh), pos2(x + bar_w, mid + rh)),
                core,
            );
        }
        self.wave_anim_settling = settling;
        p.add(mesh);

        // Playhead.
        if duration.is_some() {
            p.line_segment(
                [pos2(play_x, r.top() - 3.0), pos2(play_x, r.bottom() + 3.0)],
                Stroke::new(1.5, light),
            );
            p.circle_filled(pos2(play_x, r.top() - 3.0), 2.5, light);
        }

        // Hover time tooltip.
        if let (Some(hx), Some(d)) = (hover_x, duration) {
            let t = frac_at(hx) * d;
            let s = fmt_time(t);
            let g = p.layout_no_wrap(s, FontId::proportional(11.0), TEXT);
            let w = g.size().x + 12.0;
            let x = (hx - w / 2.0).clamp(r.left(), r.right() - w);
            let tip = Rect::from_min_size(pos2(x, r.top() - 22.0), vec2(w, 18.0));
            draw_glass_pill(
                p,
                tip,
                6.0,
                Color32::from_rgba_unmultiplied(16, 20, 30, 195),
                Color32::from_white_alpha(32),
            );
            p.galley(
                pos2(tip.left() + 6.0, tip.center().y - g.size().y / 2.0),
                g,
                TEXT,
            );
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }

    /// Time, transport buttons and volume. Returns true while the volume is dragged.
    fn draw_controls(
        &mut self,
        ui: &egui::Ui,
        p: &egui::Painter,
        r: Rect,
        accent: Color32,
        light: Color32,
    ) -> bool {
        let cy = r.center().y;

        let (big, small) = (22.0, 15.0);
        // Left edge of the transport cluster (stop button).
        let stop_left = r.center().x - (big + 8.0 + small) - (2.0 * small + 4.0) - small;

        // Time; the duration is dropped when it would run into the buttons.
        let pos = self.position();
        let font = FontId::proportional(12.0);
        let g = p.layout_no_wrap(fmt_time(pos), font.clone(), TEXT);
        let tw = g.size().x;
        p.galley(pos2(r.left(), cy - g.size().y / 2.0), g, TEXT);
        if let Some(d) = self.duration() {
            let g = p.layout_no_wrap(format!(" / {}", fmt_time(d)), font, DIM);
            if r.left() + tw + g.size().x + 8.0 <= stop_left {
                p.galley(pos2(r.left() + tw, cy - g.size().y / 2.0), g, DIM);
            }
        }

        // Transport, centred on the play button.
        let paused = self.player.paused();
        let c = pos2(r.center().x, cy);
        let next_c = c + vec2(big + 8.0 + small, 0.0);
        let prev_c = c - vec2(big + 8.0 + small, 0.0);
        let stop_c = prev_c - vec2(2.0 * small + 4.0, 0.0);

        if button(ui, p, "stop", stop_c, small, true, None, Icon::Stop)
            .on_hover_text("Stop (S)")
            .clicked()
        {
            self.stop();
        }
        if button(ui, p, "prev", prev_c, small, true, None, Icon::Prev)
            .on_hover_text("Previous (P)")
            .clicked()
        {
            self.prev();
        }
        let play_icon = if paused || self.ended {
            Icon::Play
        } else {
            Icon::Pause
        };
        if button(
            ui,
            p,
            "play",
            c,
            big,
            true,
            Some((accent, light)),
            play_icon,
        )
        .on_hover_text("Play / pause (Space)")
        .clicked()
        {
            self.toggle_pause();
        }
        let has_next = self.index + 1 < self.queue.len();
        if button(ui, p, "next", next_c, small, has_next, None, Icon::Next)
            .on_hover_text("Next (N)")
            .clicked()
        {
            self.next();
        }

        // Volume: speaker (mute toggle) + slider.
        let slider = Rect::from_min_max(
            pos2(r.right() - 88.0, cy - 9.0),
            pos2(r.right() - 4.0, cy + 9.0),
        );
        let level = if self.config.muted {
            0.0
        } else {
            self.config.volume
        };
        let spk_c = pos2(slider.left() - 18.0, cy);
        let mute_tip = if self.config.muted {
            "Unmute (M)"
        } else {
            "Mute (M)"
        };
        if button(
            ui,
            p,
            "mute",
            spk_c,
            small,
            true,
            None,
            Icon::Speaker(level),
        )
        .on_hover_text(mute_tip)
        .clicked()
        {
            self.toggle_mute();
        }
        let resp = ui.interact(slider, ui.id().with("volume"), Sense::CLICK | Sense::DRAG);
        if (resp.clicked() || resp.dragged())
            && let Some(pp) = resp.interact_pointer_pos()
        {
            self.set_volume((pp.x - slider.left()) / slider.width());
        }
        let level = if self.config.muted {
            0.0
        } else {
            self.config.volume
        };
        let active = resp.hovered() || resp.dragged();
        let track = Rect::from_min_max(
            pos2(slider.left(), cy - 1.5),
            pos2(slider.right(), cy + 1.5),
        );
        p.rect_filled(track, 1.5, FAINT);
        let fill_x = track.left() + level * track.width();
        let fill = Rect::from_min_max(track.min, pos2(fill_x, track.bottom()));
        p.rect_filled(fill, 1.5, if self.config.muted { DIM } else { accent });
        p.circle_filled(
            pos2(fill_x, cy),
            if active { 6.0 } else { 4.5 },
            if active { light } else { TEXT },
        );
        if active {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            let label = format!("{:.0}%", level * 100.0);
            let g = p.layout_no_wrap(label, FontId::proportional(10.5), TEXT);
            let w = g.size().x + 12.0;
            let x = (fill_x - w / 2.0).clamp(slider.left() - 20.0, r.right() - w);
            let tip = Rect::from_min_size(pos2(x, slider.top() - 21.0), vec2(w, 17.0));
            draw_glass_pill(
                p,
                tip,
                6.0,
                Color32::from_rgba_unmultiplied(16, 20, 30, 195),
                Color32::from_white_alpha(32),
            );
            p.galley(
                pos2(tip.left() + 6.0, tip.center().y - g.size().y / 2.0),
                g,
                TEXT,
            );
        }
        resp.dragged()
    }
}

/// RMS carries most of the visible envelope. Peak-only scaling turns limited
/// masters into solid blocks; reserve headroom even for a full-scale signal.
fn waveform_levels(peak: f32, rms: f32, gain: f32) -> (f32, f32) {
    let peak = (peak * gain).clamp(0.0, 1.0);
    let rms = (rms * gain).clamp(0.0, peak);
    let outer = (0.10 * peak + 0.90 * rms).powf(0.75) * 0.84;
    let core = (rms.powf(0.75) * 0.68).min(outer);
    (outer, core)
}

fn draw_glass_pill(p: &egui::Painter, rect: Rect, radius: f32, fill: Color32, border: Color32) {
    p.rect(
        rect,
        radius,
        fill,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    let inset = (radius * 0.65).max(3.0);
    if rect.width() > inset * 2.0 {
        p.line_segment(
            [
                pos2(rect.left() + inset, rect.top() + 1.0),
                pos2(rect.right() - inset, rect.top() + 1.0),
            ],
            Stroke::new(1.0, Color32::from_white_alpha(20)),
        );
    }
}

fn setting_toggle(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut bool,
    accent: Color32,
    light: Color32,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).color(TEXT).size(13.5));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (rect, mut response) = ui.allocate_exact_size(vec2(38.0, 22.0), Sense::click());
            if response.clicked() {
                *value = !*value;
                response.mark_changed();
                changed = true;
            }
            response.widget_info(|| {
                egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, *value, label)
            });
            let t = ui.ctx().animate_bool_with_time(response.id, *value, 0.14);
            let hover = ui.ctx().animate_bool_with_time(
                response.id.with("hover"),
                response.hovered(),
                0.14,
            );
            ui.painter().rect_filled(
                rect,
                11.0,
                Color32::from_white_alpha((18.0 + 10.0 * hover) as u8),
            );
            ui.painter().rect_filled(
                rect,
                11.0,
                Color32::from_rgba_unmultiplied(
                    accent.r(),
                    accent.g(),
                    accent.b(),
                    (t * 165.0) as u8,
                ),
            );
            let border = Color32::from_rgba_unmultiplied(
                light.r(),
                light.g(),
                light.b(),
                (28.0 + 48.0 * t + 20.0 * hover) as u8,
            );
            ui.painter()
                .rect_stroke(rect, 11.0, Stroke::new(1.0, border), StrokeKind::Inside);
            let knob_x = rect.left() + 11.0 + t * 16.0;
            let knob_c = pos2(knob_x, rect.center().y);
            ui.painter()
                .circle_filled(knob_c + vec2(0.0, 1.0), 7.5, Color32::from_black_alpha(60));
            ui.painter().circle_filled(knob_c, 7.5, TEXT);
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        });
    });
    changed
}

#[cfg(windows)]
fn glass_action_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    accent: Color32,
    light: Color32,
) -> bool {
    let desired = vec2(ui.available_width(), 32.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(desired, sense);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    let hovered = enabled && response.hovered();
    let pressed = enabled && response.is_pointer_button_down_on();
    let hover = ui
        .ctx()
        .animate_bool_with_time(response.id.with("hover"), hovered, 0.14);
    let press = ui
        .ctx()
        .animate_bool_with_time(response.id.with("press"), pressed, 0.08);
    let fill = Color32::from_rgba_unmultiplied(
        accent.r(),
        accent.g(),
        accent.b(),
        (18.0 + 26.0 * hover + 14.0 * press) as u8,
    );
    let border = Color32::from_rgba_unmultiplied(
        light.r(),
        light.g(),
        light.b(),
        (36.0 + 48.0 * hover) as u8,
    );
    draw_glass_pill(ui.painter(), rect, 9.0, fill, border);
    let fg = if !enabled {
        DIM
    } else if hovered {
        light
    } else {
        TEXT
    };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.0),
        fg,
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    enabled && response.clicked()
}

fn waveform_gain(rms: &mut [f32]) -> f32 {
    if rms.is_empty() {
        return 1.0;
    }
    rms.sort_unstable_by(f32::total_cmp);
    let reference = rms[(rms.len() - 1) * 95 / 100];
    if reference > 1e-4 {
        (0.75 / reference).clamp(0.25, 64.0)
    } else {
        1.0
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Settings,
    Play,
    Pause,
    Stop,
    Prev,
    Next,
    /// Volume level 0..=1 (0 = muted).
    Speaker(f32),
}

/// Round icon button. `filled` = (fill, hover fill) for the primary button.
#[allow(clippy::too_many_arguments)]
fn button(
    ui: &egui::Ui,
    p: &egui::Painter,
    id: &str,
    c: Pos2,
    rad: f32,
    enabled: bool,
    filled: Option<(Color32, Color32)>,
    icon: Icon,
) -> egui::Response {
    let rect = Rect::from_center_size(c, vec2(rad * 2.0, rad * 2.0));
    // Not focusable: keyboard shortcuts are global, Tab focus would double-fire.
    let sense = if enabled { Sense::CLICK } else { Sense::HOVER };
    let resp = ui.interact(rect, ui.id().with(id), sense);
    let hovered = enabled && resp.hovered();
    let pressed = enabled && resp.is_pointer_button_down_on();
    let hover = ui
        .ctx()
        .animate_bool_with_time(ui.id().with((id, "hover")), hovered, 0.16);
    let press = ui
        .ctx()
        .animate_bool_with_time(ui.id().with((id, "press")), pressed, 0.08);
    let animated_rad = rad * (1.0 + hover * 0.06 - press * 0.10);
    let fg = match filled {
        Some((fill, hover_fill)) => {
            for k in (1..=4).rev() {
                p.circle_filled(
                    c,
                    animated_rad + k as f32 * 2.5,
                    Color32::from_rgba_unmultiplied(fill.r(), fill.g(), fill.b(), 10),
                );
            }
            let base = if hovered { hover_fill } else { fill };
            p.circle_filled(
                c,
                animated_rad,
                Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 230),
            );
            p.circle_stroke(
                c,
                animated_rad - 0.5,
                Stroke::new(
                    1.0,
                    Color32::from_rgba_unmultiplied(
                        hover_fill.r(),
                        hover_fill.g(),
                        hover_fill.b(),
                        140,
                    ),
                ),
            );
            BG
        }
        None => {
            if hover > 0.01 || press > 0.01 {
                p.circle_filled(
                    c,
                    animated_rad,
                    Color32::from_white_alpha((22.0 * hover + 16.0 * press) as u8),
                );
                p.circle_stroke(
                    c,
                    animated_rad - 0.5,
                    Stroke::new(
                        1.0,
                        Color32::from_white_alpha((38.0 * hover + 18.0 * press) as u8),
                    ),
                );
            }
            if !enabled {
                FAINT
            } else if matches!(icon, Icon::Settings) && !hovered {
                DIM
            } else if hovered {
                Color32::WHITE
            } else {
                TEXT
            }
        }
    };
    if filled.is_some() {
        let pause = ui.ctx().animate_bool_with_time(
            ui.id().with((id, "pause")),
            matches!(icon, Icon::Pause),
            0.16,
        );
        let mut icon_painter = p.clone();
        icon_painter.set_opacity(1.0 - pause);
        draw_icon(&icon_painter, c, animated_rad * 0.42, Icon::Play, fg);
        icon_painter.set_opacity(pause);
        draw_icon(&icon_painter, c, animated_rad * 0.42, Icon::Pause, fg);
    } else {
        draw_icon(p, c, animated_rad * 0.42, icon, fg);
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

fn draw_icon(p: &egui::Painter, c: Pos2, s: f32, icon: Icon, col: Color32) {
    let tri = |pts: Vec<Pos2>| egui::Shape::convex_polygon(pts, col, Stroke::NONE);
    match icon {
        Icon::Settings => {
            let outline = (0..32)
                .map(|i| {
                    let angle = i as f32 * std::f32::consts::TAU / 32.0;
                    let radius = s * if matches!(i % 4, 1 | 2) { 1.4 } else { 1.08 };
                    c + vec2(angle.cos(), angle.sin()) * radius
                })
                .collect();
            p.add(egui::Shape::closed_line(outline, Stroke::new(1.2, col)));
            p.circle_stroke(c, s * 0.45, Stroke::new(1.2, col));
        }
        Icon::Play => {
            let o = c + vec2(s * 0.15, 0.0);
            p.add(tri(vec![
                o + vec2(-s * 0.75, -s),
                o + vec2(s, 0.0),
                o + vec2(-s * 0.75, s),
            ]));
        }
        Icon::Pause => {
            for dx in [-s * 0.45, s * 0.45] {
                let bar = Rect::from_center_size(c + vec2(dx, 0.0), vec2(s * 0.5, s * 1.9));
                p.rect_filled(bar, 1.0, col);
            }
        }
        Icon::Stop => {
            p.rect_filled(Rect::from_center_size(c, vec2(s * 1.5, s * 1.5)), 1.5, col);
        }
        Icon::Prev | Icon::Next => {
            let d = if matches!(icon, Icon::Next) {
                1.0
            } else {
                -1.0
            };
            p.add(tri(vec![
                c + vec2(-s * 0.7 * d, -s * 0.9),
                c + vec2(s * 0.6 * d, 0.0),
                c + vec2(-s * 0.7 * d, s * 0.9),
            ]));
            let bar = Rect::from_center_size(c + vec2(s * 0.8 * d, 0.0), vec2(s * 0.3, s * 1.8));
            p.rect_filled(bar, 0.5, col);
        }
        Icon::Speaker(level) => {
            let o = c - vec2(s * 0.35, 0.0);
            p.rect_filled(
                Rect::from_min_max(o + vec2(-s * 0.9, -s * 0.38), o + vec2(-s * 0.35, s * 0.38)),
                0.5,
                col,
            );
            p.add(tri(vec![
                o + vec2(-s * 0.4, -s * 0.38),
                o + vec2(s * 0.2, -s * 0.95),
                o + vec2(s * 0.2, s * 0.95),
                o + vec2(-s * 0.4, s * 0.38),
            ]));
            let stroke = Stroke::new(1.4, col);
            if level <= 0.0 {
                let x = o + vec2(s * 0.95, 0.0);
                let k = s * 0.42;
                p.line_segment([x + vec2(-k, -k), x + vec2(k, k)], stroke);
                p.line_segment([x + vec2(-k, k), x + vec2(k, -k)], stroke);
            } else {
                let arcs = if level > 0.5 { 2 } else { 1 };
                for a in 0..arcs {
                    let rad = s * (0.6 + 0.45 * a as f32);
                    let pts: Vec<Pos2> = (0..=8)
                        .map(|i| {
                            let t = -0.8 + 1.6 * i as f32 / 8.0;
                            o + vec2(s * 0.2, 0.0) + vec2(t.cos(), t.sin()) * rad
                        })
                        .collect();
                    p.add(egui::Shape::line(pts, stroke));
                }
            }
        }
    }
}

fn one_line(
    p: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    max_w: f32,
) -> Arc<egui::Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), TextFormat::simple(font, color));
    job.wrap = TextWrapping {
        max_width: max_w,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    p.layout_job(job)
}

fn fmt_time(t: f64) -> String {
    let s = t.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Case-insensitive comparison treating digit runs as numbers ("2" < "10").
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = a.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    a.next();
                }
                let mut nb = String::new();
                while let Some(c) = b.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    b.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_peaks_still_show_rms_dynamics_and_leave_headroom() {
        let quiet = waveform_levels(1.0, 0.10, 0.95);
        let loud = waveform_levels(1.0, 0.65, 0.95);
        assert!(loud.0 > quiet.0 * 2.0);
        let clipped = waveform_levels(1.0, 1.0, 1.0);
        assert!(clipped.0 < 0.85 && clipped.1 <= clipped.0);
        assert_eq!(waveform_levels(0.0, 0.0, 16.0), (0.0, 0.0));
    }

    #[test]
    fn quiet_masters_remain_visible_without_one_spike_flattening_the_track() {
        let mut levels = vec![0.025; 100];
        levels[99] = 0.95;
        let gain = waveform_gain(&mut levels);
        let visible = waveform_levels(0.05, 0.025, gain);
        assert!(visible.0 > 0.55 && visible.0 < 0.85);
        assert_eq!(waveform_gain(&mut [0.0; 10]), 1.0);
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["10 b.flac", "2 a.flac", "1 c.flac", "B.mp3", "a.mp3"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["1 c.flac", "2 a.flac", "10 b.flac", "a.mp3", "B.mp3"]);
    }

    #[test]
    fn time_format() {
        assert_eq!(fmt_time(0.0), "0:00");
        assert_eq!(fmt_time(83.9), "1:23");
        assert_eq!(fmt_time(3725.0), "1:02:05");
    }
}
