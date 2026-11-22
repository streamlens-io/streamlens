//! Window chrome around the panes: toolbar, chart strip with legend and
//! time ruler, status bar.

use iced::widget::{button, canvas, container, row, shader, text};
use iced::{Element, Fill, Theme};
use streamlens_types::FrameType;

use crate::theme;
use crate::widgets::barchart::{BarChart, ChartOrder};
use crate::widgets::frame_view::{ChannelMode, ColorMatrix, Zoom};
use crate::widgets::overview::Overview;

use super::{frame_type_name, goto, title_style_container, App, Message, Overlay, Playback};

impl App {
    pub(super) fn toolbar(&self) -> Element<'_, Message> {
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
            theme::flat("Go to…", Message::ToggleGoto),
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
        let reference = theme::group([
            theme::flat("Ref…", Message::OpenRefDialog),
            theme::flat("Settings", Message::ToggleSettings),
        ]);

        // Everything fits one row on a wide window (overlays right-aligned
        // via the spacer). Below the breakpoint the spacer would force
        // clipping, so the groups flow across wrapped rows instead.
        if self.window_width >= 1340.0 {
            row![
                theme::accent("Open", Message::OpenDialog),
                theme::flat("Open URL…", Message::ToggleUrlDialog),
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
                theme::flat("Open URL…", Message::ToggleUrlDialog),
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

    pub(super) fn chart_strip(&self) -> Element<'_, Message> {
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
    pub(super) fn legend_row(&self) -> Element<'_, Message> {
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
            // Frames count from where indexing started.
            let from = index
                .start_ns
                .and(goto::origin(index))
                .zip(index.frames[0].pts)
                .map(|(origin, pts)| format!(" from {}", goto::format_duration(pts - origin)))
                .unwrap_or_default();
            r = r.push(
                text(format!("frame {disp} / {}{from}", index.frames.len()))
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
    pub(super) fn time_ruler(&self) -> Element<'_, Message> {
        let Some(index) = &self.index else {
            return row![].into();
        };
        let (first, bar_width, _) = self.chart_view;
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

    /// The whole stream under the chart; see [`Overview`].
    pub(super) fn overview_strip(&self) -> Element<'_, Message> {
        let Some(index) = &self.index else {
            return row![].into();
        };
        let n = index.frames.len() as u32;
        let bar_to_decode = |bar: f32| {
            let bar = (bar.max(0.0) as u32).min(n - 1);
            match self.order {
                ChartOrder::Decode => bar,
                ChartOrder::Display => index.decode_for_display(bar).unwrap_or(0),
            }
        };
        let (first, _, visible) = self.chart_view;
        let overview = match goto::timeline(index) {
            // Seconds from the stream start: the indexed part may start
            // mid-stream.
            Some((origin, len)) => {
                let t = |d: u32| {
                    index.frames[d as usize]
                        .pts
                        .map_or(0.0, |p| (p - origin) as f32 / 1e9)
                };
                let display = |i: u32| index.decode_for_display(i).unwrap_or(0);
                let from = t(bar_to_decode(first));
                Overview {
                    total: len as f32 / 1e9,
                    indexed: (t(display(0)), t(display(n - 1))),
                    window: (from, t(bar_to_decode(first + visible)) - from),
                    current: t(self.current),
                }
            }
            None => Overview {
                total: self.overview_total(),
                indexed: (0.0, n as f32),
                window: (first, visible),
                current: match self.order {
                    ChartOrder::Decode => self.current,
                    ChartOrder::Display => index.frames[self.current as usize].display_index,
                } as f32,
            },
        };
        container(canvas(overview).width(Fill).height(10))
            .padding([0, 4])
            .into()
    }

    /// Frames in the whole stream: the index length once complete, else
    /// estimated from the indexing progress.
    pub(super) fn overview_total(&self) -> f32 {
        let n = self.index.as_ref().map_or(0, |i| i.frames.len()) as f32;
        match self.indexing {
            Some((frames, Some(fraction))) if fraction > 0.0 => (frames as f32 / fraction).max(n),
            _ => n,
        }
    }

    pub(super) fn status_bar(&self) -> Element<'_, Message> {
        // ASCII dots: the monospace font has no "…", and a missing glyph
        // blanks the whole text.
        let left: String = if let Some((frames, fraction)) = self.indexing {
            match fraction {
                Some(fr) => format!("indexing... {frames} frames ({:.0}%)", fr * 100.0),
                None => format!("indexing... {frames} frames"),
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
                f.file_offset,
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
}

fn format_time(ns: i64) -> String {
    let total_ms = ns / 1_000_000;
    let ms = total_ms % 1000;
    let s = (total_ms / 1000) % 60;
    let m = (total_ms / 60_000) % 60;
    let h = total_ms / 3_600_000;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
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
