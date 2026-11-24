//! The Elm update: messages, worker events and the state they change.

use std::sync::Arc;

use iced::widget::pane_grid;
use iced::Task;

use crate::panels::Tab;
use crate::widgets::barchart::{self, ChartOrder};
use crate::widgets::frame_view::{self, ChannelMode, ColorMatrix, Zoom};
use crate::widgets::overlays::{self};
use crate::widgets::overview;
use crate::worker::{self, Command};

use streamlens_core::Source;

use super::{dialogs, goto, App, DiffMode, Message, Overlay, Playback, Stage};

impl App {
    pub fn update(&mut self, message: Message) -> Task<Message> {
        // Set once the run loop is live: applied before the app finishes
        // launching, the icon is clobbered when the Dock tile appears.
        #[cfg(target_os = "macos")]
        {
            static DOCK_ICON: std::sync::Once = std::sync::Once::new();
            DOCK_ICON.call_once(crate::set_dock_icon);
        }
        match message {
            Message::Worker(event) => self.on_worker(event),
            Message::OpenDialog => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .add_filter(
                            "video",
                            &[
                                "mp4", "mov", "mkv", "webm", "ts", "264", "h264", "265", "h265",
                                "avi", "flv", "m3u8",
                            ],
                        )
                        .pick_file()
                        .await
                        .map(|f| f.path().to_path_buf())
                },
                Message::FileChosen,
            ),
            Message::FileChosen(Some(path)) => {
                self.open(Source::File(path), None);
                Task::none()
            }
            Message::ToggleUrlDialog => {
                self.url_open = !self.url_open;
                if self.url_open {
                    iced::widget::operation::focus(dialogs::URL_INPUT)
                } else {
                    Task::none()
                }
            }
            Message::UrlInput(url) => {
                self.url_input = url;
                Task::none()
            }
            Message::OpenUrl => {
                self.url_open = false;
                self.open(Source::parse(&self.url_input.clone()), None);
                Task::none()
            }
            Message::FileChosen(None) => Task::none(),
            Message::Chart(barchart::Interaction::Select(i)) => self.select_frame(i),
            Message::Chart(barchart::Interaction::Hover(h)) => {
                self.hovered_bar = h;
                Task::none()
            }
            Message::Chart(barchart::Interaction::View {
                first,
                bar_width,
                visible,
            }) => {
                self.chart_view = (first, bar_width, visible);
                Task::none()
            }
            Message::Overview(overview::Seek { fraction, press }) => {
                // A drag that opened the "Go to" dialog continues under it.
                let dialog = self.goto_open || self.url_open || self.settings_open;
                let Some(index) = self.index.clone().filter(|_| !dialog) else {
                    return Task::none();
                };
                let target = match goto::timeline(&index) {
                    Some((origin, len)) => {
                        let t = (fraction as f64 * len as f64) as i64;
                        match goto::resolve(&index, goto::Target::Time(t)) {
                            Ok(goto::Resolved::Frame(d)) => Some(d),
                            // A click outside the indexed part offers to
                            // start there; a drag stops at its edge.
                            Ok(goto::Resolved::Outside(at)) if press => {
                                self.goto_open = true;
                                self.goto_input = goto::format_duration(t);
                                self.goto_error = None;
                                self.goto_outside = Some(at);
                                return iced::widget::operation::focus(dialogs::GOTO_INPUT);
                            }
                            _ => Some(goto::frame_at(&index, origin + t)),
                        }
                    }
                    None => {
                        let last = index.frames.len() as u32 - 1;
                        let bar = ((fraction * self.overview_total()) as u32).min(last);
                        match self.order {
                            ChartOrder::Decode => Some(bar),
                            ChartOrder::Display => index.decode_for_display(bar),
                        }
                    }
                };
                match target {
                    Some(t) if t != self.current => self.select_frame(t),
                    _ => Task::none(),
                }
            }
            Message::ToggleGoto => {
                self.goto_open = !self.goto_open;
                self.goto_error = None;
                self.goto_outside = None;
                if self.goto_open {
                    iced::widget::operation::focus(dialogs::GOTO_INPUT)
                } else {
                    Task::none()
                }
            }
            Message::GotoInput(s) => {
                self.goto_input = s;
                self.goto_error = None;
                self.goto_outside = None;
                Task::none()
            }
            Message::Goto => {
                let Some(index) = &self.index else {
                    return Task::none();
                };
                match goto::parse(&self.goto_input).and_then(|t| goto::resolve(index, t)) {
                    Ok(goto::Resolved::Frame(decode_index)) => {
                        self.goto_open = false;
                        self.select_frame(decode_index)
                    }
                    Ok(goto::Resolved::Outside(at)) => {
                        self.goto_outside = Some(at);
                        Task::none()
                    }
                    Err(e) => {
                        self.goto_error = Some(e);
                        Task::none()
                    }
                }
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
            Message::ToggleSettings => {
                if self.settings_open {
                    self.settings.save();
                }
                self.settings_open = !self.settings_open;
                Task::none()
            }
            Message::SetFramesAhead(n) => {
                self.settings.frames_ahead = n;
                Task::none()
            }
            Message::SetFramesBehind(n) => {
                self.settings.frames_behind = n;
                Task::none()
            }
            Message::ApplyWindow => {
                if let Some(tx) = &self.worker {
                    let _ = tx.send(Command::SetWindow(self.settings.window()));
                }
                Task::none()
            }
            Message::StartAt(at) => {
                self.goto_open = false;
                self.goto_outside = None;
                let Some(source) = self.file.clone() else {
                    return Task::none();
                };
                // At or before the stream start, that is a plain open.
                let start = self
                    .index
                    .as_ref()
                    .and_then(|i| i.span_ns)
                    .is_some_and(|(start, _)| at > start)
                    .then_some(at);
                self.open(source, start);
                Task::none()
            }
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
                let _ = tx.send(Command::SetWindow(self.settings.window()));
                self.worker = Some(tx);
                if let Some(path) = self.file.clone() {
                    self.open(path, None);
                }
            }
            worker::Event::IndexProgress { id, .. } | worker::Event::Index(id, _)
                if id != self.open_id =>
            {
                // From a stream opened before the latest Open.
            }
            worker::Event::IndexProgress {
                frames, fraction, ..
            } => {
                self.indexing = Some((frames, fraction));
            }
            worker::Event::Index(_, index) => {
                if index.complete {
                    self.indexing = None;
                }
                let n = index.frames.len();
                if self.index.is_some() {
                    // The index grew: keep the position, extend the
                    // per-frame state.
                    self.sweep.resize(n, None);
                    self.psnr_series.resize(n, None);
                    self.bits_series.resize(n, None);
                    self.sweep_total = self.sweep_total.max(n as u32);
                    self.index = Some(index);
                    return Task::none();
                }
                // First snapshot: the file is open. Reset sweep state.
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

    fn open(&mut self, source: Source, start_ns: Option<i64>) {
        self.reset_stream_state();
        self.file = Some(source.clone());
        self.indexing = Some((0, None));
        self.open_id += 1;
        if let Some(tx) = &self.worker {
            let _ = tx.send(Command::Open {
                source,
                start_ns,
                id: self.open_id,
            });
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
        if let Some(frame) = &self.frame {
            overlays::build_segments(&frame.analysis, &self.overlays, &mut segments, &mut glyphs);
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
            Stage::Predicted => frame.analysis.stages.predicted.as_ref(),
            Stage::Unfiltered => frame.analysis.stages.unfiltered.as_ref(),
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
                .stages
                .residual
                .as_ref()
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
}
