//! Application state and the Elm loop.
//!
//! Layout: toolbar, BarChart strip with a time ruler,
//! then draggable panes — stream panel | video | block panel — and a status
//! bar at the bottom.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use iced::keyboard;
use iced::widget::{button, column, container, pane_grid, row, shader, text, PaneGrid};
use iced::{Element, Fill, Subscription, Task, Theme};
use streamlens_core::{DecodedFrame, StreamIndex, SweepFrame};
use streamlens_types::FrameType;

use crate::panels::{self, Tab};
use crate::theme;
use crate::widgets::barchart::{self, BarChart, ChartOrder};
use crate::widgets::frame_view::{self, ChannelMode, ColorMatrix, FrameView, Zoom};
use crate::widgets::overlays::{self, Glyph, OverlaySettings, Segment};
use crate::worker::{self, Command};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Playback {
    #[default]
    Stopped,
    Playing,
    Fast,
}

/// Which reconstruction stage the video pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stage {
    #[default]
    Decoded,
    Predicted,
    Unfiltered,
    Residual,
}

impl Stage {
    pub const ALL: [Stage; 4] = [
        Stage::Decoded,
        Stage::Predicted,
        Stage::Unfiltered,
        Stage::Residual,
    ];

    fn label(&self) -> &'static str {
        match self {
            Stage::Decoded => "Decoded",
            Stage::Predicted => "Predicted",
            Stage::Unfiltered => "Unfiltered",
            Stage::Residual => "Residual",
        }
    }
}

/// Pixel-difference rendering between the selected stage and a reference
/// (the memorized frame when set, otherwise the decoded output).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffMode {
    #[default]
    Off,
    /// Signed difference around mid-gray.
    Subtraction,
    /// Absolute difference as a heat ramp.
    Temperature,
}

impl DiffMode {
    pub const ALL: [DiffMode; 3] = [DiffMode::Off, DiffMode::Subtraction, DiffMode::Temperature];
}

impl std::fmt::Display for DiffMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DiffMode::Off => "Diff: off",
            DiffMode::Subtraction => "Diff: subtract",
            DiffMode::Temperature => "Diff: heat",
        })
    }
}

pub struct App {
    worker: Option<Sender<Command>>,
    index: Option<Arc<StreamIndex>>,
    indexing: Option<(u32, Option<f32>)>,
    file: Option<PathBuf>,
    /// Window width in logical pixels; the toolbar reflows below a
    /// breakpoint instead of clipping.
    window_width: f32,

    current: u32,
    frame: Option<Arc<DecodedFrame>>,
    frame_generation: u64,
    playback: Playback,
    hovered_bar: Option<u32>,
    hovered_pixel: Option<(u16, u16)>,
    selected_pixel: Option<(u16, u16)>,

    order: ChartOrder,
    qp_graph: bool,
    /// Draw chart bars as stacked intra/inter/skip bit shares (sweep data).
    bits_split: bool,
    chart_view: (f32, f32),
    zoom: Zoom,
    view_center: (f32, f32),
    effective_zoom: f32,
    channel: ChannelMode,
    matrix: ColorMatrix,
    stage: Stage,
    stages_on: bool,
    diff: DiffMode,
    /// Converted image for non-decoded stages / diff modes (None = show the
    /// decoded frame directly).
    stage_image: Option<Arc<streamlens_types::PlanarImage>>,
    /// Memorized frame for cross-frame comparison.
    memory: Option<Arc<streamlens_types::PlanarImage>>,
    overlays: OverlaySettings,
    /// Overlay geometry for the current frame, rebuilt on frame or overlay
    /// changes (not per redraw).
    segments: Arc<Vec<Segment>>,
    glyphs: Arc<Vec<Glyph>>,

    tab: Tab,
    /// HRD buffer model, computed once per opened stream.
    hrd: Option<HrdView>,
    /// Headers of the current frame (None while loading).
    headers: Option<(u32, Arc<Vec<streamlens_syntax::analyzer::RecordedNal>>)>,
    header_expanded: HashSet<Vec<usize>>,
    header_filter: String,

    panes: pane_grid::State<Pane>,
    error: Option<String>,

    // Sweep state
    /// Per-frame sweep results indexed by decode index (None = not yet done).
    sweep: Vec<Option<SweepFrame>>,
    sweep_done: u32,
    sweep_total: u32,
    /// Chart series maintained incrementally from sweep batches; the chart
    /// borrows these per redraw (rebuilding n-length vectors per redraw is
    /// O(n) per mouse move on six-figure streams).
    psnr_series: Vec<Option<f32>>,
    bits_series: Vec<Option<(u64, u64, u64)>>,
    /// Reference YUV path (for PSNR).
    ref_path: Option<PathBuf>,
    /// Show PSNR graph overlay in BarChart.
    show_psnr: bool,
}

/// Prepared HRD buffer model for the Buffer panel.
struct HrdView {
    fullness: Arc<Vec<f64>>,
    cpb_size: f64,
    /// Which model parameters were assumed rather than declared.
    note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Stream,
    Video,
    Block,
}

impl Pane {
    fn title(&self) -> &'static str {
        match self {
            Pane::Stream => "Stream",
            Pane::Video => "Video Out",
            Pane::Block => "Block Info",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Worker(worker::Event),
    OpenDialog,
    FileChosen(Option<PathBuf>),
    Chart(barchart::Interaction),
    Video(frame_view::Interaction),
    SelectFrame(u32),
    /// Step in display order (what the eye expects).
    Step(i32),
    /// Step in decode (stream) order.
    StepDecode(i32),
    GoStart,
    GoEnd,
    PlayPause,
    FastPlay,
    WindowResized(f32),
    ToggleOrder,
    ToggleQpGraph(bool),
    ToggleBitsSplit(bool),
    ZoomIn,
    ZoomOut,
    ZoomFit,
    ZoomActual,
    SetStage(Stage),
    SetDiff(DiffMode),
    MemorizeFrame,
    CycleChannel,
    CycleMatrix,
    ToggleOverlay(Overlay, bool),
    PaneResized(pane_grid::ResizeEvent),
    PaneDragged(pane_grid::DragEvent),
    SelectTab(Tab),
    ToggleHeaderNode(Vec<usize>),
    HeaderFilter(String),
    DismissError,
    TogglePsnr(bool),
    OpenRefDialog,
    ReferenceChosen(Option<PathBuf>),
}

#[derive(Debug, Clone, Copy)]
pub enum Overlay {
    Partitions,
    MotionVectors,
    Types,
    QpHeatmap,
    Sizes,
    SliceBoundaries,
    Values,
}

impl From<barchart::Interaction> for Message {
    fn from(i: barchart::Interaction) -> Self {
        Message::Chart(i)
    }
}

impl From<frame_view::Interaction> for Message {
    fn from(i: frame_view::Interaction) -> Self {
        Message::Video(i)
    }
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let (mut panes, stream) = pane_grid::State::new(Pane::Stream);
        let video = panes
            .split(pane_grid::Axis::Vertical, stream, Pane::Video)
            .map(|(pane, split)| {
                panes.resize(split, 0.24);
                pane
            });
        if let Some(video) = video {
            if let Some((_, split)) = panes.split(pane_grid::Axis::Vertical, video, Pane::Block) {
                panes.resize(split, 0.70);
            }
        }

        let app = Self {
            worker: None,
            index: None,
            indexing: None,
            file: std::env::args().nth(1).map(PathBuf::from),
            // Matches main.rs's initial window size (resize events only
            // arrive on change); the STREAMLENS_WINDOW hook overrides both.
            window_width: std::env::var("STREAMLENS_WINDOW")
                .ok()
                .and_then(|v| v.split_once('x')?.0.parse().ok())
                .unwrap_or(1440.0),
            current: 0,
            frame: None,
            frame_generation: 0,
            playback: Playback::Stopped,
            hovered_bar: None,
            hovered_pixel: None,
            selected_pixel: None,
            order: ChartOrder::Display,
            qp_graph: true,
            // Debug/testing hook: STREAMLENS_BITS_SPLIT=1
            bits_split: std::env::var("STREAMLENS_BITS_SPLIT").as_deref() == Ok("1"),
            chart_view: (0.0, 4.0),
            zoom: Zoom::Fit,
            view_center: (0.0, 0.0),
            effective_zoom: 1.0,
            channel: ChannelMode::Color,
            matrix: ColorMatrix::Bt709,
            stage: stage_from_env(),
            stages_on: false,
            diff: DiffMode::Off,
            stage_image: None,
            memory: None,
            overlays: overlays_from_env(),
            segments: Arc::new(Vec::new()),
            glyphs: Arc::new(Vec::new()),
            tab: tab_from_env(),
            hrd: None,
            headers: None,
            header_expanded: HashSet::new(),
            header_filter: String::new(),
            panes,
            error: None,
            sweep: Vec::new(),
            sweep_done: 0,
            sweep_total: 0,
            psnr_series: Vec::new(),
            bits_series: Vec::new(),
            ref_path: None,
            show_psnr: false,
        };
        (app, Task::none())
    }

    pub fn title(&self) -> String {
        match &self.file {
            Some(p) => format!(
                "streamlens — {}",
                p.file_name().unwrap_or_default().to_string_lossy()
            ),
            None => "streamlens".into(),
        }
    }

    pub fn theme(&self) -> Theme {
        Theme::TokyoNight
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let worker = Subscription::run(worker::run).map(Message::Worker);
        let keys = keyboard::listen().filter_map(|event| {
            use keyboard::key::Named;
            use keyboard::Key;
            let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
                return None;
            };
            match key.as_ref() {
                Key::Named(Named::ArrowRight) if modifiers.shift() => Some(Message::StepDecode(1)),
                Key::Named(Named::ArrowLeft) if modifiers.shift() => Some(Message::StepDecode(-1)),
                Key::Named(Named::ArrowRight) => Some(Message::Step(1)),
                Key::Named(Named::ArrowLeft) => Some(Message::Step(-1)),
                Key::Named(Named::Home) => Some(Message::GoStart),
                Key::Named(Named::End) => Some(Message::GoEnd),
                Key::Named(Named::Space) => Some(Message::PlayPause),
                Key::Character("o") if modifiers.command() => Some(Message::OpenDialog),
                Key::Character("+") | Key::Character("=") => Some(Message::ZoomIn),
                Key::Character("-") => Some(Message::ZoomOut),
                Key::Character("0") => Some(Message::ZoomFit),
                Key::Character("1") => Some(Message::ZoomActual),
                _ => None,
            }
        });
        let resizes =
            iced::window::resize_events().map(|(_, size)| Message::WindowResized(size.width));
        Subscription::batch([worker, keys, resizes])
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Worker(event) => self.on_worker(event),
            Message::OpenDialog => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .add_filter(
                            "video",
                            &[
                                "mp4", "mov", "mkv", "webm", "ts", "264", "h264", "265", "h265",
                                "avi", "flv",
                            ],
                        )
                        .pick_file()
                        .await
                        .map(|f| f.path().to_path_buf())
                },
                Message::FileChosen,
            ),
            Message::FileChosen(Some(path)) => {
                self.open(path);
                Task::none()
            }
            Message::FileChosen(None) => Task::none(),
            Message::Chart(barchart::Interaction::Select(i)) => self.select_frame(i),
            Message::Chart(barchart::Interaction::Hover(h)) => {
                self.hovered_bar = h;
                Task::none()
            }
            Message::Chart(barchart::Interaction::View { first, bar_width }) => {
                self.chart_view = (first, bar_width);
                Task::none()
            }
            Message::Video(frame_view::Interaction::Hover(x, y)) => {
                self.hovered_pixel = Some((x, y));
                Task::none()
            }
            Message::Video(frame_view::Interaction::Select(x, y)) => {
                self.selected_pixel = Some((x, y));
                Task::none()
            }
            Message::Video(frame_view::Interaction::SetView { zoom, center }) => {
                self.zoom = Zoom::Manual(zoom);
                self.view_center = center;
                self.effective_zoom = zoom;
                Task::none()
            }
            Message::Video(frame_view::Interaction::EffectiveZoom(z)) => {
                self.effective_zoom = z;
                Task::none()
            }
            Message::SelectFrame(i) => self.select_frame(i),
            Message::Step(d) => self.step_display(d),
            Message::StepDecode(d) => {
                let target = self.current.saturating_add_signed(d);
                self.select_frame(target)
            }
            Message::GoStart => self.step_display_to(0),
            Message::GoEnd => {
                let last = self.index.as_ref().map(|i| i.frames.len() as u32 - 1);
                match last {
                    Some(_) => self.step_display_to(u32::MAX),
                    None => Task::none(),
                }
            }
            Message::PlayPause => {
                match self.playback {
                    Playback::Stopped => self.start_play(false),
                    _ => self.stop_play(),
                }
                Task::none()
            }
            Message::FastPlay => {
                match self.playback {
                    Playback::Fast => self.stop_play(),
                    _ => self.start_play(true),
                }
                Task::none()
            }
            Message::WindowResized(width) => {
                self.window_width = width;
                Task::none()
            }
            Message::ToggleOrder => {
                self.order = match self.order {
                    ChartOrder::Decode => ChartOrder::Display,
                    ChartOrder::Display => ChartOrder::Decode,
                };
                Task::none()
            }
            Message::ToggleQpGraph(v) => {
                self.qp_graph = v;
                Task::none()
            }
            Message::ToggleBitsSplit(v) => {
                self.bits_split = v;
                Task::none()
            }
            Message::ZoomIn => {
                self.set_zoom(self.effective_zoom * 1.4);
                Task::none()
            }
            Message::ZoomOut => {
                self.set_zoom(self.effective_zoom / 1.4);
                Task::none()
            }
            Message::ZoomFit => {
                self.zoom = Zoom::Fit;
                Task::none()
            }
            Message::ZoomActual => {
                self.set_zoom(1.0);
                Task::none()
            }
            Message::SetStage(stage) => {
                self.stage = stage;
                // Stage planes are only captured on demand; toggle capture
                // in the worker and re-decode the current frame.
                let want = stage != Stage::Decoded;
                if want != self.stages_on {
                    self.stages_on = want;
                    if let Some(tx) = &self.worker {
                        let _ = tx.send(Command::SetStages {
                            on: want,
                            refresh: self.current,
                        });
                    }
                }
                self.rebuild_stage_image();
                Task::none()
            }
            Message::SetDiff(diff) => {
                self.diff = diff;
                self.rebuild_stage_image();
                Task::none()
            }
            Message::MemorizeFrame => {
                self.memory = self.current_stage_planar().map(|img| Arc::new(img.clone()));
                self.rebuild_stage_image();
                // The displayed diff image changed without a frame/stage/
                // diff change; bump the generation or the GPU keeps the
                // old texture.
                self.frame_generation += 1;
                Task::none()
            }
            Message::CycleChannel => {
                self.channel = match self.channel {
                    ChannelMode::Color => ChannelMode::Y,
                    ChannelMode::Y => ChannelMode::U,
                    ChannelMode::U => ChannelMode::V,
                    ChannelMode::V => ChannelMode::Color,
                };
                Task::none()
            }
            Message::CycleMatrix => {
                self.matrix = match self.matrix {
                    ColorMatrix::Bt601 => ColorMatrix::Bt709,
                    ColorMatrix::Bt709 => ColorMatrix::Bt2020,
                    ColorMatrix::Bt2020 => ColorMatrix::Bt601,
                };
                Task::none()
            }
            Message::ToggleOverlay(o, on) => {
                match o {
                    Overlay::Partitions => self.overlays.partitions = on,
                    Overlay::MotionVectors => self.overlays.motion_vectors = on,
                    Overlay::Types => self.overlays.types = on,
                    Overlay::QpHeatmap => self.overlays.qp_heatmap = on,
                    Overlay::Sizes => self.overlays.sizes = on,
                    Overlay::SliceBoundaries => self.overlays.slice_boundaries = on,
                    Overlay::Values => self.overlays.values = on,
                }
                self.rebuild_segments();
                Task::none()
            }
            Message::PaneResized(e) => {
                self.panes.resize(e.split, e.ratio);
                Task::none()
            }
            Message::PaneDragged(pane_grid::DragEvent::Dropped { pane, target }) => {
                self.panes.drop(pane, target);
                Task::none()
            }
            Message::PaneDragged(_) => Task::none(),
            Message::SelectTab(tab) => {
                self.tab = tab;
                self.request_panel_data();
                Task::none()
            }
            Message::ToggleHeaderNode(path) => {
                if !self.header_expanded.remove(&path) {
                    self.header_expanded.insert(path);
                }
                Task::none()
            }
            Message::HeaderFilter(f) => {
                self.header_filter = f;
                Task::none()
            }
            Message::DismissError => {
                self.error = None;
                Task::none()
            }
            Message::TogglePsnr(v) => {
                self.show_psnr = v;
                Task::none()
            }
            Message::OpenRefDialog => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .add_filter("raw YUV", &["yuv", "raw"])
                        .pick_file()
                        .await
                        .map(|f| f.path().to_path_buf())
                },
                Message::ReferenceChosen,
            ),
            Message::ReferenceChosen(path) => {
                self.ref_path = path.clone();
                if let Some(tx) = &self.worker {
                    let _ = tx.send(worker::Command::SetReference(path));
                }
                Task::none()
            }
        }
    }

    fn set_zoom(&mut self, zoom: f32) {
        let z = zoom.clamp(frame_view::MIN_ZOOM, frame_view::MAX_ZOOM);
        // Keep the current center; the widget clamps it against the bounds.
        if self.view_center == (0.0, 0.0) {
            if let Some(f) = &self.frame {
                self.view_center = (f.image.width as f32 / 2.0, f.image.height as f32 / 2.0);
            }
        }
        self.zoom = Zoom::Manual(z);
        self.effective_zoom = z;
    }

    fn start_play(&mut self, fast: bool) {
        let Some(index) = &self.index else { return };
        let from_display = index.frames[self.current as usize].display_index;
        // Restart from the beginning when already at the end.
        let from_display = if from_display >= index.frames.len() as u32 - 1 {
            0
        } else {
            from_display
        };
        if let Some(tx) = &self.worker {
            let _ = tx.send(Command::Play { from_display, fast });
            self.playback = if fast {
                Playback::Fast
            } else {
                Playback::Playing
            };
        }
    }

    fn stop_play(&mut self) {
        if let Some(tx) = &self.worker {
            let _ = tx.send(Command::Stop);
        }
        self.playback = Playback::Stopped;
    }

    /// Step by `d` in display order.
    fn step_display(&mut self, d: i32) -> Task<Message> {
        let Some(index) = &self.index else {
            return Task::none();
        };
        let cur = index.frames[self.current as usize].display_index;
        self.step_display_to(cur.saturating_add_signed(d))
    }

    /// Jump to a display-order position.
    fn step_display_to(&mut self, display_index: u32) -> Task<Message> {
        let Some(index) = &self.index else {
            return Task::none();
        };
        let display_index = display_index.min(index.frames.len() as u32 - 1);
        match index.decode_for_display(display_index) {
            Some(t) => self.select_frame(t),
            None => Task::none(),
        }
    }

    fn on_worker(&mut self, event: worker::Event) -> Task<Message> {
        match event {
            worker::Event::Ready(tx) => {
                self.worker = Some(tx);
                if let Some(path) = self.file.clone() {
                    self.open(path);
                }
            }
            worker::Event::IndexProgress { frames, fraction } => {
                self.indexing = Some((frames, fraction));
            }
            worker::Event::Opened(index) => {
                self.indexing = None;
                self.hrd = streamlens_core::hrd::build_model(&index).map(|m| {
                    let mut note = if m.declared {
                        "declared".to_string()
                    } else {
                        "estimated: measured bitrate, 1 s CPB".to_string()
                    };
                    if !m.initial_fullness_declared {
                        note.push_str(", initial fullness assumed 90%");
                    }
                    if !m.timing_declared {
                        note.push_str(", timing from frame rate");
                    }
                    HrdView {
                        fullness: Arc::new(streamlens_core::hrd::fullness_before_removal(&m)),
                        cpb_size: m.cpb_size as f64,
                        note,
                    }
                });
                // Reset sweep state for the new file.
                let n = index.frames.len();
                self.sweep = vec![None; n];
                self.sweep_done = 0;
                self.sweep_total = n as u32;
                self.index = Some(index);
                self.frame = None;
                self.zoom = Zoom::Fit;
                self.playback = Playback::Stopped;
                // Debug/testing hook: jump to a frame at startup.
                let start = std::env::var("STREAMLENS_START_FRAME")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                self.current = 0;
                if self.stage != Stage::Decoded && !self.stages_on {
                    self.stages_on = true;
                    if let Some(tx) = &self.worker {
                        let _ = tx.send(Command::SetStages {
                            on: true,
                            refresh: start,
                        });
                    }
                }
                let task = self.select_frame(start);
                // Debug/testing hook: STREAMLENS_ZOOM=4 opens zoomed in.
                if let Some(z) = std::env::var("STREAMLENS_ZOOM")
                    .ok()
                    .and_then(|v| v.parse::<f32>().ok())
                {
                    self.zoom = Zoom::Manual(z);
                    self.effective_zoom = z;
                }
                // Debug/testing hook: STREAMLENS_PLAY=1 starts playback.
                if std::env::var("STREAMLENS_PLAY").as_deref() == Ok("1") {
                    self.start_play(false);
                }
                return task;
            }
            worker::Event::Frame(frame) => {
                log::debug!(
                    "frame {} arrived (current {})",
                    frame.decode_index,
                    self.current
                );
                // A frame from a previously opened stream can arrive after
                // Open; its index means nothing against the new file.
                if self
                    .index
                    .as_ref()
                    .is_none_or(|i| frame.decode_index as usize >= i.frames.len())
                {
                    return Task::none();
                }
                // During playback the worker drives the position; otherwise
                // only the frame we asked for is accepted (bursts of stale
                // deliveries arrive while stepping fast).
                if self.playback != Playback::Stopped {
                    self.current = frame.decode_index;
                } else if frame.decode_index != self.current {
                    return Task::none();
                }
                self.frame = Some(frame);
                self.frame_generation += 1;
                self.rebuild_segments();
                self.rebuild_stage_image();
            }
            worker::Event::Headers(idx, h) => {
                if idx == self.current {
                    self.headers = Some((idx, h));
                }
            }
            worker::Event::PlayEnded => {
                self.playback = Playback::Stopped;
            }
            worker::Event::Sweep { done, total, batch } => {
                if total > self.sweep_total {
                    self.sweep_total = total;
                    self.sweep.resize(total as usize, None);
                }
                self.psnr_series.resize(self.sweep.len(), None);
                self.bits_series.resize(self.sweep.len(), None);
                for (decode_idx, sf) in batch {
                    let i = decode_idx as usize;
                    if i < self.sweep.len() {
                        self.psnr_series[i] = sf.psnr_y;
                        self.bits_series[i] = Some((sf.bits_intra, sf.bits_inter, sf.bits_skip));
                        self.sweep[i] = Some(sf);
                    }
                }
                self.sweep_done = done;
            }
            worker::Event::Error(e) => {
                self.indexing = None;
                self.error = Some(e);
            }
        }
        Task::none()
    }

    fn open(&mut self, path: PathBuf) {
        self.reset_stream_state();
        self.file = Some(path.clone());
        self.indexing = Some((0, None));
        if let Some(tx) = &self.worker {
            let _ = tx.send(Command::Open(path));
        }
    }

    /// Clears everything derived from the current stream. Stale positions
    /// index out of bounds against a shorter file, and stale images or
    /// overlay geometry belong to the old resolution.
    fn reset_stream_state(&mut self) {
        self.index = None;
        self.frame = None;
        self.error = None;
        self.playback = Playback::Stopped;
        self.current = 0;
        self.hovered_bar = None;
        self.hovered_pixel = None;
        self.selected_pixel = None;
        self.headers = None;
        self.header_expanded.clear();
        self.header_filter.clear();
        self.memory = None;
        self.stage_image = None;
        self.segments = Arc::new(Vec::new());
        self.glyphs = Arc::new(Vec::new());
        self.hrd = None;
        self.sweep.clear();
        self.sweep_done = 0;
        self.sweep_total = 0;
        self.psnr_series.clear();
        self.bits_series.clear();
    }

    fn select_frame(&mut self, decode_index: u32) -> Task<Message> {
        if self.playback != Playback::Stopped {
            self.stop_play();
        }
        let Some(index) = &self.index else {
            return Task::none();
        };
        let clamped = decode_index.min(index.frames.len() as u32 - 1);
        self.current = clamped;
        self.selected_pixel = None;
        self.headers = None;
        self.request_frame(clamped);
        self.request_panel_data();
        Task::none()
    }

    fn rebuild_segments(&mut self) {
        let mut segments = Vec::new();
        let mut glyphs = Vec::new();
        if let Some(analysis) = self.frame.as_ref().and_then(|f| f.analysis.as_deref()) {
            overlays::build_segments(analysis, &self.overlays, &mut segments, &mut glyphs);
        }
        self.segments = Arc::new(segments);
        self.glyphs = Arc::new(glyphs);
    }

    fn request_frame(&self, decode_index: u32) {
        if let Some(tx) = &self.worker {
            let _ = tx.send(Command::Frame(decode_index));
        }
    }

    /// The image the selected stage shows, before diff conversion.
    fn current_stage_planar(&self) -> Option<&streamlens_types::PlanarImage> {
        let frame = self.frame.as_ref()?;
        match self.stage {
            Stage::Decoded => Some(&frame.image),
            Stage::Predicted => frame.analysis.as_ref()?.stages.predicted.as_ref(),
            Stage::Unfiltered => frame.analysis.as_ref()?.stages.unfiltered.as_ref(),
            Stage::Residual => None, // converted in rebuild_stage_image
        }
    }

    /// Recomputes the displayed image for non-decoded stages / diff modes.
    fn rebuild_stage_image(&mut self) {
        self.stage_image = None;
        let Some(frame) = self.frame.as_ref() else {
            return;
        };

        let base: Option<streamlens_types::PlanarImage> = match self.stage {
            Stage::Decoded => None,
            Stage::Predicted | Stage::Unfiltered => self.current_stage_planar().cloned(),
            Stage::Residual => frame
                .analysis
                .as_ref()
                .and_then(|a| a.stages.residual.as_ref())
                .map(|r| crate::stages::residual_to_planar(r, frame.image.bit_depth)),
        };

        // Difference against the memory frame (when set) or the decoded
        // output.
        let result = if self.diff != DiffMode::Off {
            let a: &streamlens_types::PlanarImage = base.as_ref().unwrap_or(&frame.image);
            let b: &streamlens_types::PlanarImage = self.memory.as_deref().unwrap_or(&frame.image);
            match self.diff {
                DiffMode::Subtraction => crate::stages::subtract(a, b),
                DiffMode::Temperature => crate::stages::temperature(a, b),
                DiffMode::Off => unreachable!(),
            }
            .or(base)
        } else {
            base
        };

        self.stage_image = result.map(Arc::new);
    }

    /// Fetch data the visible side panel needs for the current frame.
    fn request_panel_data(&self) {
        let (Some(tx), Some(_)) = (&self.worker, &self.index) else {
            return;
        };
        match self.tab {
            Tab::Headers
                if self
                    .headers
                    .as_ref()
                    .is_none_or(|(i, _)| *i != self.current) =>
            {
                let _ = tx.send(Command::Headers(self.current));
            }
            _ => {}
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let grid = PaneGrid::new(&self.panes, |_, pane, _| {
            let body = match pane {
                Pane::Video => self.video_pane(),
                Pane::Stream => self.side_pane(),
                Pane::Block => self.block_pane(),
            };
            pane_grid::Content::new(body)
                .style(pane_body_style)
                .title_bar(
                    pane_grid::TitleBar::new(text(pane.title()).size(11))
                        .padding(4)
                        .style(title_style),
                )
        })
        .on_resize(8, Message::PaneResized)
        .on_drag(Message::PaneDragged)
        .style(panel_style)
        .width(Fill)
        .height(Fill)
        .spacing(4);

        let mut root = column![
            self.toolbar(),
            self.legend_row(),
            container(self.chart_strip()).height(150).padding([0, 4]),
            self.time_ruler(),
            grid,
            self.status_bar(),
        ]
        .spacing(3);

        if let Some(err) = &self.error {
            root = root.push(
                container(
                    row![
                        text(err).size(13),
                        button(text("×").size(13)).on_press(Message::DismissError)
                    ]
                    .spacing(8),
                )
                .padding(6)
                .style(container::danger),
            );
        }
        root.into()
    }

    fn toolbar(&self) -> Element<'_, Message> {
        let playing = self.playback == Playback::Playing;
        let fast = self.playback == Playback::Fast;

        // Glyphs chosen from blocks without emoji variants — U+23EE-style
        // media symbols render as color emoji on macOS regardless of FE0E.
        let transport = theme::group([
            theme::flat("│◀◀", Message::GoStart),
            theme::flat("◀", Message::Step(-1)),
            theme::segment(
                if playing { "▮▮" } else { "▶" },
                playing,
                Message::PlayPause,
            ),
            theme::flat("▶│", Message::Step(1)),
            theme::flat("▶▶│", Message::GoEnd),
            theme::segment("Fast", fast, Message::FastPlay),
        ]);

        let zoom = theme::group([
            theme::flat("−", Message::ZoomOut),
            button(
                text(match self.zoom {
                    Zoom::Fit => format!("Fit {:.0}%", self.effective_zoom * 100.0),
                    Zoom::Manual(_) => format!("{:.0}%", self.effective_zoom * 100.0),
                })
                .size(12)
                .font(iced::Font::MONOSPACE),
            )
            .padding([4, 6])
            .style(button::text)
            .on_press(Message::ZoomFit)
            .into(),
            theme::flat("+", Message::ZoomIn),
            theme::flat("1:1", Message::ZoomActual),
        ]);

        let order = theme::group([
            theme::segment(
                "Display",
                self.order == ChartOrder::Display,
                Message::ToggleOrder,
            ),
            theme::segment(
                "Decode",
                self.order == ChartOrder::Decode,
                Message::ToggleOrder,
            ),
        ]);

        let color = theme::group([
            theme::flat(channel_label(self.channel), Message::CycleChannel),
            theme::flat(matrix_label(self.matrix), Message::CycleMatrix),
        ]);

        let overlay =
            |label, on, o: Overlay| theme::chip(label, on, Message::ToggleOverlay(o, !on));
        let overlays = theme::group([
            text("OVERLAYS").size(10).color(theme::DIM).into(),
            overlay("Part", self.overlays.partitions, Overlay::Partitions),
            overlay("MV", self.overlays.motion_vectors, Overlay::MotionVectors),
            overlay("Types", self.overlays.types, Overlay::Types),
            overlay("QP", self.overlays.qp_heatmap, Overlay::QpHeatmap),
            overlay("Sizes", self.overlays.sizes, Overlay::Sizes),
            overlay(
                "Slices",
                self.overlays.slice_boundaries,
                Overlay::SliceBoundaries,
            ),
            // Value glyphs show bits when "Sizes" is on, QP otherwise.
            overlay(
                if self.overlays.sizes {
                    "Values: bits"
                } else {
                    "Values: QP"
                },
                self.overlays.values,
                Overlay::Values,
            ),
        ]);

        let mut chart_children: Vec<Element<'_, Message>> = vec![
            text("CHART").size(10).color(theme::DIM).into(),
            theme::chip("QP", self.qp_graph, Message::ToggleQpGraph(!self.qp_graph)),
            theme::chip(
                "Bits split",
                self.bits_split,
                Message::ToggleBitsSplit(!self.bits_split),
            ),
        ];
        if self.ref_path.is_some() {
            chart_children.push(theme::chip(
                "PSNR",
                self.show_psnr,
                Message::TogglePsnr(!self.show_psnr),
            ));
        }
        let chart_toggles = theme::group(chart_children);
        let reference = theme::group([theme::flat("Ref…", Message::OpenRefDialog)]);

        // Everything fits one row on a wide window (overlays right-aligned
        // via the spacer). Below the breakpoint the spacer would force
        // clipping, so the groups flow across wrapped rows instead.
        if self.window_width >= 1180.0 {
            row![
                theme::accent("Open", Message::OpenDialog),
                transport,
                zoom,
                order,
                color,
                iced::widget::space::horizontal(),
                overlays,
                chart_toggles,
                reference,
            ]
            .spacing(8)
            .padding(8)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            row![
                theme::accent("Open", Message::OpenDialog),
                transport,
                zoom,
                order,
                color,
                overlays,
                chart_toggles,
                reference,
            ]
            .spacing(8)
            .padding(8)
            .align_y(iced::Alignment::Center)
            .wrap()
            .into()
        }
    }

    fn chart_strip(&self) -> Element<'_, Message> {
        let psnr_graph =
            (self.show_psnr && self.ref_path.is_some()).then_some(self.psnr_series.as_slice());
        let bits_split = self.bits_split.then_some(self.bits_series.as_slice());
        shader(BarChart {
            index: self.index.as_ref(),
            selected: Some(self.current),
            order: self.order,
            qp_graph: self.qp_graph,
            psnr_graph,
            bits_split,
        })
        .width(Fill)
        .height(Fill)
        .into()
    }

    /// Legend for the chart: frame-type colors left, position right.
    fn legend_row(&self) -> Element<'_, Message> {
        let square = |t: FrameType| {
            container(row![])
                .width(9)
                .height(9)
                .style(move |_: &Theme| container::Style {
                    background: Some(iced::Background::Color(theme::frame_type_color(t))),
                    border: iced::Border {
                        radius: 2.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
        };
        let mut r = row![].spacing(6).align_y(iced::Alignment::Center);
        for (t, name) in [
            (FrameType::I, "I-frame"),
            (FrameType::P, "P-frame"),
            (FrameType::B, "B-frame"),
        ] {
            r = r.push(square(t));
            r = r.push(text(name).size(11).color(theme::DIM));
        }
        r = r.push(text("· bar height = frame size").size(11).color(theme::DIM));
        r = r.push(iced::widget::space::horizontal());
        if let Some(index) = &self.index {
            let disp = index.frames[self.current as usize].display_index;
            r = r.push(
                text(format!("frame {disp} / {}", index.frames.len()))
                    .size(11)
                    .font(iced::Font::MONOSPACE)
                    .color(theme::DIM),
            );
        }
        container(r).padding([0, 8]).into()
    }

    /// Time labels aligned with the visible chart window. Labels are anchored
    /// to fixed bar-grid multiples, so they stay put while scrolling and line
    /// up with the bars beneath (chart and ruler share the same 4 px side
    /// padding).
    fn time_ruler(&self) -> Element<'_, Message> {
        let Some(index) = &self.index else {
            return row![].into();
        };
        let (first, bar_width) = self.chart_view;
        let mut labels = row![].spacing(0);
        // One label roughly every 140 px, snapped to a bar-grid multiple.
        let step = (140.0 / bar_width.max(0.5)).ceil() as usize;
        let n = index.frames.len();
        let mut bar = ((first.max(0.0) / step as f32).ceil() as usize) * step;
        // Lead spacer: exact pixel offset of the first anchored label.
        labels = labels.push(
            container(row![])
                .width((bar as f32 - first.max(0.0)) * bar_width)
                .clip(true),
        );
        let mut used_px = 0.0f32;
        while bar < n && used_px < 4000.0 {
            let Some(i) = (match self.order {
                ChartOrder::Decode => Some(bar as u32),
                ChartOrder::Display => index.decode_for_display(bar as u32),
            }) else {
                break;
            };
            let f = &index.frames[i as usize];
            let label = f.pts.map(format_time).unwrap_or_else(|| format!("#{bar}"));
            labels = labels.push(
                container(text(label).size(10))
                    .width(step as f32 * bar_width)
                    .clip(true),
            );
            used_px += step as f32 * bar_width;
            bar += step;
        }
        container(labels).padding([0, 4]).height(16).into()
    }

    fn side_pane(&self) -> Element<'_, Message> {
        let bar = panels::tab_bar(self.tab);
        let body: Element<'_, Message> = match (&self.index, self.tab) {
            (None, _) => container(text("no stream").size(13)).padding(10).into(),
            (Some(_), Tab::Info) => self.info_pane(),
            (Some(index), Tab::Stream) => panels::stream_view(index, self.current),
            (Some(_), Tab::Headers) => panels::headers_view(
                self.headers
                    .as_ref()
                    .filter(|(i, _)| *i == self.current)
                    .map(|(_, h)| h),
                &self.header_expanded,
                &self.header_filter,
            ),
            (Some(index), Tab::Stats) => panels::stats_view(
                index,
                self.frame.as_ref().and_then(|f| f.analysis.as_deref()),
            ),
            (Some(_), Tab::Buffer) => match &self.hrd {
                Some(hrd) => iced::widget::canvas(panels::BufferChart {
                    fullness: hrd.fullness.clone(),
                    cpb_size: hrd.cpb_size,
                    params_note: hrd.note.clone(),
                    current: self.current as usize,
                })
                .width(Fill)
                .height(Fill)
                .into(),
                None => container(text("no timing information for HRD model").size(12))
                    .padding(10)
                    .into(),
            },
            (Some(index), Tab::Dpb) => panels::dpb_view(index, self.current),
        };
        column![bar, body].into()
    }

    fn video_pane(&self) -> Element<'_, Message> {
        // Stage planes are captured lazily: clicking a stage tab enables
        // capture and re-decodes. 8-bit only (decoder patch limitation).
        let obtainable = self
            .frame
            .as_ref()
            .is_some_and(|f| f.analysis.is_some() && f.image.bit_depth == 8);
        let available = |stage: Stage| stage == Stage::Decoded || obtainable;
        let mut stage_tabs = row![].spacing(2).padding(2);
        for stage in Stage::ALL {
            if available(stage) {
                stage_tabs = stage_tabs.push(theme::chip(
                    stage.label(),
                    stage == self.stage,
                    Message::SetStage(stage),
                ));
            } else {
                // Unavailable (no analysis / >8-bit): dimmed, no action.
                stage_tabs = stage_tabs.push(
                    button(text(stage.label()).size(12))
                        .padding([3, 8])
                        .style(button::text),
                );
            }
        }
        stage_tabs = stage_tabs.push(
            iced::widget::pick_list(DiffMode::ALL, Some(self.diff), Message::SetDiff)
                .text_size(11)
                .padding([1, 4]),
        );
        stage_tabs = stage_tabs.push(theme::chip(
            "Mem",
            self.memory.is_some(),
            Message::MemorizeFrame,
        ));

        let view = shader(FrameView {
            frame: self.frame.as_ref(),
            override_image: self.stage_image.as_ref(),
            frame_generation: self.frame_generation * 16 + self.stage as u64 * 4 + self.diff as u64,
            instance: 0,
            zoom: self.zoom,
            center: self.view_center,
            channel: self.channel,
            matrix: self.matrix,
            segments: &self.segments,
            glyphs: &self.glyphs,
            interactive: true,
        })
        .width(Fill)
        .height(Fill);

        let caption: Element<'_, Message> = match self.hovered_pixel.or(self.selected_pixel) {
            Some((x, y)) => {
                let block = match self.block_coords(x, y) {
                    Some((unit, bx, by)) => format!(" · {unit} ({bx}, {by})"),
                    None => String::new(),
                };
                container(
                    text(format!("pixel ({x}, {y}){block}"))
                        .size(11)
                        .font(iced::Font::MONOSPACE)
                        .color(theme::DIM),
                )
                .padding([2, 8])
                .into()
            }
            None => container(text("").size(11)).padding([2, 8]).into(),
        };
        column![stage_tabs.wrap(), view, caption].into()
    }

    /// Block Info pane: magnified view of the block under the cursor, with
    /// everything known about it below (follows hover).
    fn block_pane(&self) -> Element<'_, Message> {
        let pixel = self.hovered_pixel.or(self.selected_pixel);
        let analysis = self.frame.as_ref().and_then(|f| f.analysis.as_deref());
        column![
            self.block_magnifier(pixel),
            panels::block_view(analysis, pixel)
        ]
        .into()
    }

    /// The magnifier on top of the Block Info pane. Zooms to the coding unit
    /// under the cursor; without analysis it just magnifies around the
    /// cursor and claims no block geometry.
    fn block_magnifier(&self, pixel: Option<(u16, u16)>) -> Element<'_, Message> {
        let (Some((x, y)), Some(_)) = (pixel, self.frame.as_ref()) else {
            return column![].height(0).into();
        };
        let rect = self
            .frame
            .as_ref()
            .and_then(|f| f.analysis.as_ref())
            .and_then(|a| a.cu_at(x, y))
            .map(|cu| cu.rect);
        const PANE: f32 = 200.0;
        let (caption, zoom, center) = match rect {
            Some(r) => (
                format!("block {}×{} @ ({}, {})", r.w, r.h, r.x, r.y),
                ((PANE - 30.0) / r.w.max(r.h) as f32).clamp(2.0, frame_view::MAX_ZOOM * 4.0),
                (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0),
            ),
            None => (format!("({x}, {y})"), 8.0, (x as f32, y as f32)),
        };
        column![
            text(caption).size(11),
            shader(FrameView {
                frame: self.frame.as_ref(),
                override_image: self.stage_image.as_ref(),
                frame_generation: self.frame_generation * 16
                    + self.stage as u64 * 4
                    + self.diff as u64,
                instance: 1,
                zoom: Zoom::Manual(zoom),
                center,
                channel: self.channel,
                matrix: self.matrix,
                segments: &self.segments,
                glyphs: &self.glyphs,
                interactive: false,
            })
            .width(Fill)
            .height(iced::Length::Fixed(PANE))
        ]
        .spacing(2)
        .padding(4)
        .into()
    }

    /// Block coordinates for a pixel: (unit name, col, row). H.264 MBs are
    /// always 16×16; H.265 CTU size comes from the frame analysis — without
    /// it we show no block coordinate rather than guess.
    fn block_coords(&self, x: u16, y: u16) -> Option<(&'static str, u32, u32)> {
        if let Some(a) = self.frame.as_ref().and_then(|f| f.analysis.as_deref()) {
            let unit = if a.superblock_size == 16 { "MB" } else { "CTU" };
            let s = a.superblock_size.max(1);
            return Some((unit, x as u32 / s, y as u32 / s));
        }
        match self.index.as_ref().map(|i| i.codec) {
            Some(streamlens_types::Codec::H264) => Some(("MB", x as u32 / 16, y as u32 / 16)),
            _ => None,
        }
    }

    fn status_bar(&self) -> Element<'_, Message> {
        let left: String = if let Some((frames, fraction)) = self.indexing {
            match fraction {
                Some(fr) => format!("indexing… {frames} frames ({:.0}%)", fr * 100.0),
                None => format!("indexing… {frames} frames"),
            }
        } else if let Some(index) = &self.index {
            let sweep_suffix = if self.sweep_total > 0 && self.sweep_done < self.sweep_total {
                format!(
                    "  sweep {:.0}%",
                    self.sweep_done as f32 / self.sweep_total as f32 * 100.0
                )
            } else {
                String::new()
            };
            let ref_suffix = if let Some(ref_path) = &self.ref_path {
                format!(
                    "  ref: {}",
                    ref_path.file_name().unwrap_or_default().to_string_lossy()
                )
            } else {
                String::new()
            };
            format!(
                "{:?} · {}×{} {} · {} frames{}{}",
                index.codec,
                index.info.width,
                index.info.height,
                index.info.profile,
                index.frames.len(),
                sweep_suffix,
                ref_suffix,
            )
        } else {
            "open a stream (Cmd+O)".into()
        };

        let middle: String = match (self.hovered_bar, &self.index) {
            (Some(h), Some(index)) if (h as usize) < index.frames.len() => {
                let f = &index.frames[h as usize];
                format!(
                    "bar: #{} {:?} — {} B — QP {} — POC {}",
                    h, f.frame_type, f.size_bytes, f.slice_qp, f.poc
                )
            }
            _ => String::new(),
        };

        let right: String = if let Some(index) = &self.index {
            let f = &index.frames[self.current as usize];
            let psnr_str = self
                .sweep
                .get(self.current as usize)
                .and_then(|s| s.as_ref())
                .and_then(|sf| sf.psnr_y)
                .map(|p| format!("  PSNR-Y {p:.2} dB"))
                .unwrap_or_default();
            format!(
                "Dec {}  Disp {}  Type {}{}  Size {} B  Offset {:#x}  POC {}  QP {}{}",
                f.decode_index,
                f.display_index,
                frame_type_name(f.frame_type),
                if f.is_idr { " (IDR)" } else { "" },
                f.size_bytes,
                f.byte_offset,
                f.poc,
                f.slice_qp,
                psnr_str,
            )
        } else {
            String::new()
        };

        container(
            row![
                text(left).size(11).font(iced::Font::MONOSPACE),
                iced::widget::space::horizontal(),
                text(middle).size(11).font(iced::Font::MONOSPACE),
                iced::widget::space::horizontal(),
                text(right).size(11).font(iced::Font::MONOSPACE),
            ]
            .spacing(10),
        )
        .padding([3, 8])
        .style(title_style_container)
        .into()
    }

    fn info_pane(&self) -> Element<'_, Message> {
        let Some(index) = &self.index else {
            return container(text("no stream").size(13)).padding(10).into();
        };
        let info = &index.info;
        let f = &index.frames[self.current as usize];

        let col = column![
            theme::section("Stream"),
            theme::kv("Codec", format!("{:?}", info.codec)),
            theme::kv(
                "Profile",
                if info.profile.is_empty() {
                    "—".into()
                } else {
                    format!("{} @ L{}", info.profile, info.level)
                }
            ),
            theme::kv("Resolution", format!("{}×{}", info.width, info.height)),
            theme::kv(
                "Bit depth",
                if info.bit_depth == 0 {
                    "—".into()
                } else {
                    format!("{} ({})", info.bit_depth, info.chroma_format)
                }
            ),
            theme::kv(
                "Frame rate",
                info.frame_rate
                    .map(|(n, d)| format!("{:.3} fps", n as f64 / d as f64))
                    .unwrap_or_else(|| "—".into()),
            ),
            theme::kv("Frames", index.frames.len().to_string()),
            text("").size(6),
            row![
                theme::section("Frame"),
                theme::badge(
                    frame_type_name(f.frame_type).into(),
                    theme::frame_type_color(f.frame_type)
                )
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            theme::kv("Decode #", f.decode_index.to_string()),
            theme::kv("Display #", f.display_index.to_string()),
            theme::kv("POC", f.poc.to_string()),
            theme::kv("Slice QP", f.slice_qp.to_string()),
            theme::kv("Slices", f.slice_count.to_string()),
            theme::kv("Size", format!("{} B", f.size_bytes)),
            theme::kv("Offset", format!("{:#x}", f.byte_offset)),
            theme::kv(
                "PTS",
                f.pts
                    .map(|t| format!("{:.3} s", t as f64 / 1e9))
                    .unwrap_or_else(|| "—".into()),
            ),
            theme::kv("L0 refs", format_refs(&f.ref_frames[0])),
            theme::kv("L1 refs", format_refs(&f.ref_frames[1])),
        ]
        .spacing(4);

        container(iced::widget::scrollable(col.padding(12)))
            .width(Fill)
            .height(Fill)
            .into()
    }
}

fn panel_style(theme: &Theme) -> pane_grid::Style {
    let palette = theme.extended_palette();
    pane_grid::Style {
        // Hovered-region highlight while dragging panes.
        hovered_region: pane_grid::Highlight {
            background: iced::Background::Color(iced::Color {
                a: 0.3,
                ..palette.primary.base.color
            }),
            border: iced::Border {
                color: palette.primary.strong.color,
                width: 2.0,
                radius: 0.0.into(),
            },
        },
        picked_split: pane_grid::Line {
            color: palette.primary.strong.color,
            width: 3.0,
        },
        hovered_split: pane_grid::Line {
            color: palette.primary.base.color,
            width: 2.0,
        },
    }
}

fn title_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(iced::Background::Color(palette.background.weak.color)),
        text_color: Some(palette.background.weak.text),
        border: iced::Border {
            color: palette.background.strong.color,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

fn title_style_container(theme: &Theme) -> container::Style {
    title_style(theme)
}

fn pane_body_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        border: iced::Border {
            color: palette.background.strong.color,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

/// Debug/testing hook: STREAMLENS_STAGE=predicted|unfiltered|residual
fn stage_from_env() -> Stage {
    match std::env::var("STREAMLENS_STAGE").as_deref() {
        Ok("predicted") => Stage::Predicted,
        Ok("unfiltered") => Stage::Unfiltered,
        Ok("residual") => Stage::Residual,
        _ => Stage::Decoded,
    }
}

/// Debug/testing hook: STREAMLENS_TAB=stream|headers|stats|buffer|dpb
fn tab_from_env() -> Tab {
    match std::env::var("STREAMLENS_TAB").as_deref() {
        Ok("stream") => Tab::Stream,
        Ok("headers") => Tab::Headers,
        Ok("stats") => Tab::Stats,
        Ok("buffer") => Tab::Buffer,
        Ok("dpb") => Tab::Dpb,
        _ => Tab::Info,
    }
}

/// Debug/testing hook: STREAMLENS_OVERLAYS=partitions,mv,types,qp,slices
fn overlays_from_env() -> OverlaySettings {
    let mut o = OverlaySettings::default();
    if let Ok(v) = std::env::var("STREAMLENS_OVERLAYS") {
        for name in v.split(',') {
            match name.trim() {
                "partitions" => o.partitions = true,
                "mv" => o.motion_vectors = true,
                "types" => o.types = true,
                "qp" => o.qp_heatmap = true,
                "sizes" => o.sizes = true,
                "slices" => o.slice_boundaries = true,
                "values" => o.values = true,
                _ => {}
            }
        }
    }
    o
}

fn format_refs(refs: &[u32]) -> String {
    if refs.is_empty() {
        "—".into()
    } else {
        refs.iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn format_time(ns: i64) -> String {
    let total_ms = ns / 1_000_000;
    let ms = total_ms % 1000;
    let s = (total_ms / 1000) % 60;
    let m = (total_ms / 60_000) % 60;
    let h = total_ms / 3_600_000;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

fn frame_type_name(t: FrameType) -> &'static str {
    match t {
        FrameType::I => "I",
        FrameType::P => "P",
        FrameType::B => "B",
    }
}

fn channel_label(c: ChannelMode) -> &'static str {
    match c {
        ChannelMode::Color => "YUV",
        ChannelMode::Y => "Y",
        ChannelMode::U => "U",
        ChannelMode::V => "V",
    }
}

fn matrix_label(m: ColorMatrix) -> &'static str {
    match m {
        ColorMatrix::Bt601 => "BT.601",
        ColorMatrix::Bt709 => "BT.709",
        ColorMatrix::Bt2020 => "BT.2020",
    }
}
