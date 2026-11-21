//! The pane contents: Stream (side panel tabs), Video Out and Block Info.

use iced::widget::{button, column, container, row, shader, text};
use iced::{Element, Fill};

use crate::panels::{self, Tab};
use crate::theme;
use crate::widgets::frame_view::{self, FrameView, Zoom};

use super::{frame_type_name, App, DiffMode, Message, Stage};

impl App {
    pub(super) fn side_pane(&self) -> Element<'_, Message> {
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
        };
        column![bar, body].into()
    }

    pub(super) fn video_pane(&self) -> Element<'_, Message> {
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
    pub(super) fn block_pane(&self) -> Element<'_, Message> {
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
