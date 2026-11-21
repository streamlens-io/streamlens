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
use iced::widget::{button, column, container, pane_grid, row, text, PaneGrid};
use iced::{Element, Fill, Subscription, Task, Theme};
use streamlens_core::{DecodedFrame, StreamIndex, SweepFrame};
use streamlens_types::FrameType;

use crate::panels::Tab;
use crate::widgets::barchart::{self, ChartOrder};
use crate::widgets::frame_view::{self, ChannelMode, ColorMatrix, Zoom};
use crate::widgets::overlays::{Glyph, OverlaySettings, Segment};
use crate::worker::{self, Command};

mod chrome;
mod panes;
mod update;

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
    /// Headers of the current frame (None while loading).
    headers: Option<(u32, Arc<Vec<streamlens_core::RecordedNal>>)>,
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
                "StreamLens — {}",
                p.file_name().unwrap_or_default().to_string_lossy()
            ),
            None => "StreamLens".into(),
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

pub(super) fn title_style_container(theme: &Theme) -> container::Style {
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

/// Debug/testing hook: STREAMLENS_TAB=stream|headers|stats
fn tab_from_env() -> Tab {
    match std::env::var("STREAMLENS_TAB").as_deref() {
        Ok("stream") => Tab::Stream,
        Ok("headers") => Tab::Headers,
        Ok("stats") => Tab::Stats,
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

pub(super) fn frame_type_name(t: FrameType) -> &'static str {
    match t {
        FrameType::I => "I",
        FrameType::P => "P",
        FrameType::B => "B",
    }
}
