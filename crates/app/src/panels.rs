//! Side-panel views: Stream Viewer (NAL tree), Headers (syntax field tree),
//! HEX viewer, Statistics, Buffer (HRD), DPB, Pixels and Block Info.

use std::collections::HashSet;
use std::sync::Arc;

use iced::widget::{button, column, container, row, scrollable, text, Column};
use iced::{Element, Fill};
use streamlens_core::StreamIndex;
use streamlens_syntax::analyzer::RecordedNal;
use streamlens_syntax::FieldNode;
use streamlens_types::{FrameAnalysis, FrameType, PredMode};

use crate::app::Message;
use crate::theme;

/// Which side panel is visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Info,
    Pixels,
    Stream,
    Headers,
    Hex,
    Stats,
    Buffer,
    Dpb,
}

impl Tab {
    pub const ALL: [Tab; 8] = [
        Tab::Info,
        Tab::Pixels,
        Tab::Stream,
        Tab::Headers,
        Tab::Hex,
        Tab::Stats,
        Tab::Buffer,
        Tab::Dpb,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Tab::Info => "Info",
            Tab::Pixels => "Pixels",
            Tab::Stream => "Stream",
            Tab::Headers => "Headers",
            Tab::Hex => "Hex",
            Tab::Stats => "Stats",
            Tab::Buffer => "Buffer",
            Tab::Dpb => "DPB",
        }
    }
}

pub fn tab_bar(active: Tab) -> Element<'static, Message> {
    let mut bar = row![].spacing(1);
    for tab in Tab::ALL {
        let mut b = button(text(tab.label()).size(11)).padding([2, 5]);
        if tab != active {
            b = b.style(button::secondary);
        }
        bar = bar.push(b.on_press(Message::SelectTab(tab)));
    }
    container(bar).padding(3).into()
}

/// Stream Viewer: AU/NAL inventory around the current frame, synced to it.
pub fn stream_view<'a>(index: &'a StreamIndex, current: u32) -> Element<'a, Message> {
    const WINDOW: u32 = 50;
    let from = current.saturating_sub(WINDOW);
    let to = (current + WINDOW).min(index.frames.len() as u32 - 1);

    let mut col = column![].spacing(1);
    if from > 0 {
        col = col.push(nav_row(format!("… {from} earlier frames"), 0));
    }
    for i in from..=to {
        let f = &index.frames[i as usize];
        let au = &index.analysis[i as usize];
        let marker = if i == current { "▶" } else { " " };
        let head = format!(
            "{marker} AU {i} — {:?} POC {} — {} B @ {:#x}",
            f.frame_type, f.poc, f.size_bytes, f.byte_offset
        );
        col = col.push(nav_row_colored(
            head,
            i,
            theme::frame_type_color(f.frame_type),
        ));
        if i == current {
            for nal in &au.nals {
                col = col.push(
                    text(format!(
                        "      {} — {} B @ {:#x}",
                        nal.name,
                        nal.size,
                        f.byte_offset + nal.offset as u64
                    ))
                    .size(11),
                );
            }
        }
    }
    if to + 1 < index.frames.len() as u32 {
        col = col.push(nav_row(
            format!("… {} later frames", index.frames.len() as u32 - to - 1),
            index.frames.len() as u32 - 1,
        ));
    }
    scrollable(col.padding(8)).width(Fill).height(Fill).into()
}

fn nav_row(label: String, target: u32) -> Element<'static, Message> {
    button(text(label).size(11))
        .style(button::text)
        .padding(1)
        .on_press(Message::SelectFrame(target))
        .into()
}

fn nav_row_colored(label: String, target: u32, color: iced::Color) -> Element<'static, Message> {
    button(text(label).size(11).color(color))
        .style(button::text)
        .padding(1)
        .on_press(Message::SelectFrame(target))
        .into()
}

/// Headers panel: recorded field trees of the current AU's NALs.
pub fn headers_view<'a>(
    headers: Option<&'a Arc<Vec<RecordedNal>>>,
    expanded: &'a HashSet<Vec<usize>>,
    filter: &'a str,
) -> Element<'a, Message> {
    let Some(headers) = headers else {
        return container(text("loading headers…").size(12))
            .padding(10)
            .into();
    };
    let mut col = column![iced::widget::text_input("filter fields…", filter)
        .on_input(Message::HeaderFilter)
        .size(12)
        .padding(4)]
    .spacing(1);

    for (n, nal) in headers.iter().enumerate() {
        let path = vec![n];
        let open = expanded.contains(&path) || !filter.is_empty();
        col = col.push(
            button(
                text(format!(
                    "{} {} ({} B)",
                    if open { "▼" } else { "▶" },
                    nal.name,
                    nal.size
                ))
                .size(12),
            )
            .style(button::text)
            .padding(1)
            .on_press(Message::ToggleHeaderNode(path.clone())),
        );
        if let Some(err) = &nal.error {
            col = col.push(text(format!("    ⚠ {err}")).size(11));
        }
        if open {
            for (i, node) in nal.tree.roots.iter().enumerate() {
                let mut p = path.clone();
                p.push(i);
                col = push_field_node(col, node, p, 1, expanded, filter);
            }
        }
    }
    scrollable(col.padding(8)).width(Fill).height(Fill).into()
}

fn push_field_node<'a>(
    mut col: Column<'a, Message>,
    node: &'a FieldNode,
    path: Vec<usize>,
    depth: usize,
    expanded: &'a HashSet<Vec<usize>>,
    filter: &'a str,
) -> Column<'a, Message> {
    let matches = filter.is_empty() || node.name.contains(filter);
    let indent = "    ".repeat(depth);
    if node.children.is_empty() {
        if matches {
            let label = format!(
                "{indent}{} = {}   ({}b @ bit {})",
                node.name, node.value, node.bit_width, node.bit_offset
            );
            col = col.push(text(label).size(11));
        }
    } else {
        let open = expanded.contains(&path) || !filter.is_empty();
        col = col.push(
            button(
                text(format!(
                    "{indent}{} {}",
                    if open { "▼" } else { "▶" },
                    node.name
                ))
                .size(11),
            )
            .style(button::text)
            .padding(0)
            .on_press(Message::ToggleHeaderNode(path.clone())),
        );
        if open {
            for (i, child) in node.children.iter().enumerate() {
                let mut p = path.clone();
                p.push(i);
                col = push_field_node(col, child, p, depth + 1, expanded, filter);
            }
        }
    }
    col
}

/// Block Info panel: everything about the block under the locked (or
/// hovered) pixel.
pub fn block_view<'a>(
    analysis: Option<&'a FrameAnalysis>,
    pixel: Option<(u16, u16)>,
) -> Element<'a, Message> {
    let Some(a) = analysis else {
        return container(text("no per-block analysis for this frame").size(12))
            .padding(10)
            .into();
    };
    let Some((x, y)) = pixel else {
        return container(text("hover a block in the video view").size(12))
            .padding(10)
            .into();
    };
    let Some(sb) = a.superblock_at(x, y) else {
        return container(text("outside coded area").size(12))
            .padding(10)
            .into();
    };

    let unit = if a.superblock_size == 16 { "MB" } else { "CTU" };
    let mut col = column![text(format!(
        "{unit} @ ({}, {}) — slice {}, tile {}",
        sb.rect.x, sb.rect.y, sb.slice_index, sb.tile_index
    ))
    .size(13),]
    .spacing(3);

    let Some(cu) = a.cus_of(sb).iter().find(|cu| cu.rect.contains(x, y)) else {
        return scrollable(col.padding(10)).into();
    };

    col = col.push(text("").size(4));
    col = col.push(text("Coding unit").size(13));
    col = col.push(
        text(format!(
            "{}×{} @ ({}, {}), depth {}",
            cu.rect.w, cu.rect.h, cu.rect.x, cu.rect.y, cu.depth
        ))
        .size(12),
    );
    col = col.push(text(format!("Mode: {:?}   QP: {}", cu.pred, cu.qp)).size(12));
    if let Some(bits) = cu.bits {
        col = col.push(text(format!("Bits: {}", bits.total)).size(12));
    }

    for (i, pu) in a.pus_of(cu).iter().enumerate() {
        col = col.push(text("").size(4));
        col = col.push(
            text(format!(
                "PU {i}: {}×{} @ ({}, {})",
                pu.rect.w, pu.rect.h, pu.rect.x, pu.rect.y
            ))
            .size(12),
        );
        if let Some(intra) = &pu.intra {
            col = col.push(
                text(format!(
                    "  intra luma mode {} / chroma {}",
                    intra.luma_mode, intra.chroma_mode
                ))
                .size(12),
            );
        }
        if let Some(inter) = &pu.inter {
            for list in 0..2 {
                if let Some(mv) = inter.mv[list] {
                    let refinfo = match (inter.ref_poc[list], inter.ref_idx[list]) {
                        (Some(poc), _) => format!("→ POC {poc}"),
                        (None, Some(idx)) => format!("ref_idx {idx}"),
                        _ => String::new(),
                    };
                    col = col.push(
                        text(format!(
                            "  L{list}: ({:+.2}, {:+.2}) px {refinfo}",
                            mv.x as f32 / 4.0,
                            mv.y as f32 / 4.0
                        ))
                        .size(12),
                    );
                }
            }
            if let Some(merge) = inter.merge {
                col = col.push(text(format!("  merge idx {merge}")).size(12));
            }
        }
    }

    let tus = a.tus_of(cu);
    if !tus.is_empty() {
        let cbf = |v: Option<bool>| match v {
            Some(true) => "1",
            Some(false) => "0",
            None => "—", // decoder doesn't report chroma cbf
        };
        col = col.push(text("").size(4));
        col = col.push(text(format!("Transform units: {}", tus.len())).size(12));
        for tu in tus.iter().take(16) {
            col = col.push(
                text(format!(
                    "  {}×{} @ ({}, {}) depth {} cbf(y/cb/cr): {}/{}/{}",
                    tu.rect.w,
                    tu.rect.h,
                    tu.rect.x,
                    tu.rect.y,
                    tu.depth,
                    tu.cbf_luma as u8,
                    cbf(tu.cbf_cb),
                    cbf(tu.cbf_cr)
                ))
                .size(11),
            );
        }
    }

    scrollable(col.padding(10)).width(Fill).height(Fill).into()
}

/// HEX viewer: a pageable window into the whole elementary stream with the
/// current frame's AU highlighted. Navigate with the offset box (hex) and
/// the page buttons.
pub fn hex_view<'a>(
    window_offset: u64,
    bytes: Option<&'a Arc<Vec<u8>>>,
    au_range: (u64, u64),
    es_size: u64,
    addr_input: &'a str,
) -> Element<'a, Message> {
    let controls = row![
        button(text("◀").size(11))
            .padding([1, 6])
            .style(button::secondary)
            .on_press(Message::HexPage(-1)),
        button(text("▶").size(11))
            .padding([1, 6])
            .style(button::secondary)
            .on_press(Message::HexPage(1)),
        iced::widget::text_input("offset (hex)…", addr_input)
            .on_input(Message::HexInput)
            .on_submit(Message::HexGo)
            .size(11)
            .padding(3)
            .width(110),
        button(text("Go").size(11))
            .padding([1, 6])
            .style(button::secondary)
            .on_press(Message::HexGo),
        text(format!(
            "AU @ {:#x} · {} B · ES {} B",
            au_range.0,
            au_range.1 - au_range.0,
            es_size
        ))
        .size(11),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    let mut col = column![controls].spacing(1);

    let Some(bytes) = bytes else {
        col = col.push(text("loading bytes…").size(12));
        return container(col.padding(8)).into();
    };

    const PER_ROW: usize = 16;
    // Column header.
    let header: String = (0..PER_ROW)
        .map(|i| format!("{}{i:02X} ", if i == 8 { " " } else { "" }))
        .collect();
    col = col.push(
        text(format!("{:>8}  {header}", "offset"))
            .size(11)
            .font(iced::Font::MONOSPACE)
            .color(iced::Color::from_rgba(1.0, 1.0, 1.0, 0.45)),
    );

    let dim = iced::Color::from_rgba(1.0, 1.0, 1.0, 0.45);
    for (r, chunk) in bytes.chunks(PER_ROW).enumerate() {
        let addr = window_offset + (r * PER_ROW) as u64;
        let hex: String = chunk
            .iter()
            .enumerate()
            .map(|(i, b)| format!("{}{b:02X} ", if i == 8 { " " } else { "" }))
            .collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if (0x20..0x7F).contains(&b) {
                    b as char
                } else {
                    '·'
                }
            })
            .collect();
        let in_au = addr + PER_ROW as u64 > au_range.0 && addr < au_range.1;
        let mut t = text(format!("{addr:08X}  {hex:<49} {ascii}"))
            .size(11)
            .font(iced::Font::MONOSPACE);
        if !in_au {
            t = t.color(dim);
        }
        col = col.push(t);
    }

    scrollable(col.padding(8)).width(Fill).height(Fill).into()
}

/// Category label color for statistics sections.
const CATEGORY: iced::Color = iced::Color::from_rgb(0.42, 0.66, 1.0);

fn category(name: &str) -> Element<'static, Message> {
    text(name.to_string()).size(12).color(CATEGORY).into()
}

/// One statistics row: indented name, value, optional proportional bar.
fn stat_row(
    indent: usize,
    name: &str,
    value: String,
    bar: Option<(f32, iced::Color)>,
) -> Element<'static, Message> {
    const BAR_MAX: f32 = 110.0;
    let mut r = row![
        text(format!("{}{name}", "    ".repeat(indent)))
            .size(12)
            .width(140),
        text(value).size(12).width(130),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);
    if let Some((frac, color)) = bar {
        let w = (BAR_MAX * frac.clamp(0.0, 1.0)).max(1.0);
        r = r.push(
            container(row![])
                .width(w)
                .height(9)
                .style(move |_: &iced::Theme| container::Style {
                    background: Some(iced::Background::Color(color)),
                    ..Default::default()
                }),
        );
    }
    r.into()
}

/// Statistics: stream-level distributions plus current-frame block stats,
/// presented as name / value / proportional bar rows.
pub fn stats_view<'a>(
    index: &'a StreamIndex,
    analysis: Option<&'a FrameAnalysis>,
) -> Element<'a, Message> {
    let mut col = column![text("Stream").size(14)].spacing(2);

    // Whole-stream aggregates come precomputed from the index pass —
    // rescanning every frame record per redraw doesn't scale to
    // six-figure streams.
    let counts = index.stats.type_counts;
    let sizes = index.stats.type_bytes;
    let (qp_min, qp_max) = index.qp_range;
    let total: u32 = counts.iter().sum::<u32>().max(1);
    let types = [FrameType::I, FrameType::P, FrameType::B];

    col = col.push(category("frames"));
    col = col.push(stat_row(1, "total", total.to_string(), None));
    for (k, name) in ["I", "P", "B"].iter().enumerate() {
        if counts[k] > 0 {
            let frac = counts[k] as f32 / total as f32;
            col = col.push(stat_row(
                1,
                name,
                format!("{} ({:.1}%)", counts[k], frac * 100.0),
                Some((frac, theme::frame_type_color(types[k]))),
            ));
        }
    }

    col = col.push(category("avg size"));
    let avgs: Vec<u64> = (0..3)
        .map(|k| {
            if counts[k] > 0 {
                sizes[k] / counts[k] as u64
            } else {
                0
            }
        })
        .collect();
    let max_avg = avgs.iter().copied().max().unwrap_or(1).max(1);
    for (k, name) in ["I", "P", "B"].iter().enumerate() {
        if counts[k] > 0 {
            col = col.push(stat_row(
                1,
                name,
                format!("{} B", avgs[k]),
                Some((
                    avgs[k] as f32 / max_avg as f32,
                    theme::frame_type_color(types[k]),
                )),
            ));
        }
    }

    col = col.push(category("stream"));
    if let Some((min, avg, max)) = index.stats.keyframe_distance {
        col = col.push(stat_row(
            1,
            "key distance",
            format!("{min} / {avg:.1} / {max}"),
            None,
        ));
    }
    let bitrate = index
        .info
        .duration_ns
        .filter(|&d| d > 0)
        .map(|d| (index.es_size * 8) as f64 / (d as f64 / 1e9));
    if let Some(bps) = bitrate {
        col = col.push(stat_row(
            1,
            "avg bitrate",
            format!("{:.0} kbit/s (measured)", bps / 1000.0),
            None,
        ));
    }
    col = col.push(stat_row(1, "slice QP", format!("{qp_min}..{qp_max}"), None));

    if let Some(a) = analysis {
        col = col.push(text("").size(6));
        col = col.push(text("Picture (per-block)").size(14));
        let mut pred_counts: [(PredMode, u32); 5] = [
            (PredMode::Intra, 0),
            (PredMode::Inter, 0),
            (PredMode::Skip, 0),
            (PredMode::Direct, 0),
            (PredMode::Pcm, 0),
        ];
        let mut pred_area = [0u64; 5];
        let (mut bqp_min, mut bqp_max) = (i8::MAX, i8::MIN);
        let mut mv_count = 0u64;
        let mut zero_mvs = 0u64;
        let (mut mv_max_x, mut mv_max_y) = (0u32, 0u32);
        let (mut mv_sum_x, mut mv_sum_y) = (0u64, 0u64);
        for cu in &a.cus {
            bqp_min = bqp_min.min(cu.qp);
            bqp_max = bqp_max.max(cu.qp);
            let k = pred_counts
                .iter()
                .position(|(m, _)| *m == cu.pred)
                .unwrap_or(0);
            pred_counts[k].1 += 1;
            pred_area[k] += cu.rect.w as u64 * cu.rect.h as u64;
            for pu in a.pus_of(cu) {
                if let Some(inter) = &pu.inter {
                    for mv in inter.mv.iter().flatten() {
                        mv_count += 1;
                        if mv.x == 0 && mv.y == 0 {
                            zero_mvs += 1;
                        }
                        mv_max_x = mv_max_x.max(mv.x.unsigned_abs() as u32);
                        mv_max_y = mv_max_y.max(mv.y.unsigned_abs() as u32);
                        mv_sum_x += mv.x.unsigned_abs() as u64;
                        mv_sum_y += mv.y.unsigned_abs() as u64;
                    }
                }
            }
        }
        let total_area: u64 = pred_area.iter().sum::<u64>().max(1);

        col = col.push(category("prediction (by area)"));
        col = col.push(stat_row(1, "coding units", a.cus.len().to_string(), None));
        for (i, (mode, n)) in pred_counts.iter().enumerate() {
            if *n > 0 {
                let frac = pred_area[i] as f32 / total_area as f32;
                col = col.push(stat_row(
                    1,
                    &format!("{mode:?}"),
                    format!("{n} ({:.1}%)", frac * 100.0),
                    Some((frac, pred_mode_color(*mode))),
                ));
            }
        }
        col = col.push(stat_row(
            1,
            "block QP",
            format!("{bqp_min}..{bqp_max}"),
            None,
        ));

        col = col.push(category("mv"));
        col = col.push(stat_row(1, "count", mv_count.to_string(), None));
        if mv_count > 0 {
            col = col.push(stat_row(
                1,
                "max |x| / |y|",
                format!(
                    "{:.2} / {:.2} px",
                    mv_max_x as f32 / 4.0,
                    mv_max_y as f32 / 4.0
                ),
                None,
            ));
            col = col.push(stat_row(
                1,
                "avg |x| / |y|",
                format!(
                    "{:.2} / {:.2} px",
                    mv_sum_x as f32 / mv_count as f32 / 4.0,
                    mv_sum_y as f32 / mv_count as f32 / 4.0
                ),
                None,
            ));
            col = col.push(stat_row(1, "zero vectors", zero_mvs.to_string(), None));
        }
    } else {
        col = col.push(text("").size(6));
        col = col.push(text("per-block stats need the analysis decoders").size(11));
    }

    scrollable(col.padding(10)).width(Fill).height(Fill).into()
}

/// Colors matching the Types overlay palette (overlays::type_color).
fn pred_mode_color(mode: PredMode) -> iced::Color {
    match mode {
        PredMode::Intra => iced::Color::from_rgb(0.90, 0.20, 0.20),
        PredMode::Inter => iced::Color::from_rgb(0.30, 0.50, 0.95),
        PredMode::Skip => iced::Color::from_rgb(0.45, 0.45, 0.45),
        PredMode::Direct => iced::Color::from_rgb(0.25, 0.85, 0.85),
        PredMode::Pcm => iced::Color::from_rgb(0.95, 0.85, 0.20),
    }
}

/// Buffer Analyzer: CPB fullness over the timeline, current frame marked.
pub struct BufferChart {
    pub fullness: Arc<Vec<f64>>,
    pub cpb_size: f64,
    /// Where the model parameters came from (declared vs assumed), shown
    /// verbatim so estimated inputs are never presented as stream data.
    pub params_note: String,
    pub current: usize,
}

impl iced::widget::canvas::Program<Message> for BufferChart {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<iced::widget::canvas::Geometry> {
        use iced::widget::canvas::{Frame, Path, Stroke, Text};
        use iced::{Color, Point};

        let mut frame = Frame::new(renderer, bounds.size());
        let n = self.fullness.len();
        if n == 0 {
            return vec![frame.into_geometry()];
        }
        let (w, h) = (bounds.width, bounds.height - 18.0);
        let max = self.cpb_size.max(1.0);
        let x_of = |i: usize| i as f32 / (n.max(2) - 1) as f32 * w;
        let y_of = |v: f64| h - (v / max).clamp(-0.2, 1.0) as f32 * (h - 4.0);

        // CPB ceiling.
        frame.stroke(
            &Path::line(Point::new(0.0, y_of(max)), Point::new(w, y_of(max))),
            Stroke::default()
                .with_color(Color::from_rgba(1.0, 1.0, 1.0, 0.4))
                .with_width(1.0),
        );
        // Fullness as a min/max envelope per pixel column — a polyline with
        // one point per frame doesn't scale to six-figure streams.
        let cols = w.ceil().max(1.0) as usize;
        let path = Path::new(|b| {
            let mut started = false;
            for c in 0..cols {
                let lo_i = c * n / cols;
                let hi_i = ((c + 1) * n / cols).max(lo_i + 1).min(n);
                let (mut lo, mut hi) = (f64::MAX, f64::MIN);
                for &v in &self.fullness[lo_i..hi_i] {
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
                let x = c as f32;
                if !started {
                    b.move_to(Point::new(x, y_of(hi)));
                    started = true;
                } else {
                    b.line_to(Point::new(x, y_of(hi)));
                }
                if lo < hi {
                    b.line_to(Point::new(x, y_of(lo)));
                }
            }
        });
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(Color::from_rgb(0.35, 0.75, 1.0))
                .with_width(1.5),
        );
        // Underflow markers (one per column at most).
        let mut last_col = usize::MAX;
        for (i, &v) in self.fullness.iter().enumerate() {
            if v < 0.0 {
                let col = i * cols / n;
                if col != last_col {
                    frame.fill(
                        &Path::circle(Point::new(x_of(i), y_of(0.0)), 3.0),
                        Color::from_rgb(1.0, 0.25, 0.25),
                    );
                    last_col = col;
                }
            }
        }
        // Current frame cursor.
        let cx = x_of(self.current.min(n - 1));
        frame.stroke(
            &Path::line(Point::new(cx, 0.0), Point::new(cx, h)),
            Stroke::default()
                .with_color(Color::from_rgba(1.0, 1.0, 1.0, 0.7))
                .with_width(1.0),
        );
        frame.fill_text(Text {
            content: format!(
                "CPB {:.0} kbit ({}) — fullness at cursor {:.0} kbit",
                max / 1000.0,
                self.params_note,
                self.fullness[self.current.min(n - 1)] / 1000.0
            ),
            position: Point::new(4.0, h + 2.0),
            size: iced::Pixels(11.0),
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.8),
            ..Text::default()
        });
        vec![frame.into_geometry()]
    }
}

/// DPB panel: reference pictures held after decoding the current frame.
pub fn dpb_view(index: &StreamIndex, current: u32) -> Element<'_, Message> {
    let au = &index.analysis[current as usize];
    let mut col = column![text(format!(
        "DPB after frame {current} — {} pictures",
        au.dpb.entries.len()
    ))
    .size(13)]
    .spacing(2);
    for e in &au.dpb.entries {
        let f = &index.frames[e.decode_index as usize];
        col = col.push(nav_row_colored(
            format!(
                "POC {} — decode #{} — {:?} — {:?}",
                e.poc, e.decode_index, f.frame_type, e.marking
            ),
            e.decode_index,
            theme::frame_type_color(f.frame_type),
        ));
    }
    if au.dpb.entries.is_empty() {
        col = col.push(text("empty").size(12));
    }
    scrollable(col.padding(10)).width(Fill).height(Fill).into()
}

/// Pixels panel: numeric sample values of the displayed image around the
/// selected pixel, per plane, dec or hex. `source` names which image is
/// sampled (stage/diff selection), so converted values aren't mistaken for
/// decoded output.
pub fn pixels_view<'a>(
    image: Option<&'a streamlens_types::PlanarImage>,
    source: String,
    pixel: Option<(u16, u16)>,
    plane: usize,
    hex: bool,
) -> Element<'a, Message> {
    let Some(img) = image else {
        return container(text("no frame").size(12)).padding(10).into();
    };
    let Some((px, py)) = pixel else {
        return container(text("hover a pixel in the video view").size(12))
            .padding(10)
            .into();
    };
    let plane = plane.min(img.planes.len() - 1);
    let (pw, ph) = img.plane_dims(plane);
    // Map luma coords to the plane's resolution.
    let (sx, sy) = if plane == 0 {
        (px as u32, py as u32)
    } else {
        (
            px as u32 * pw / img.width.max(1),
            py as u32 * ph / img.height.max(1),
        )
    };

    const SPAN: u32 = 8;
    let x0 = sx.saturating_sub(SPAN);
    let y0 = sy.saturating_sub(SPAN);

    let mut col = column![row![
        text(format!(
            "{source} — plane {} ({}) around ({sx}, {sy})",
            plane,
            ["Y", "Cb", "Cr"].get(plane).unwrap_or(&"?")
        ))
        .size(12),
        button(text(if hex { "hex" } else { "dec" }).size(11))
            .padding([1, 6])
            .style(button::secondary)
            .on_press(Message::TogglePixelsHex),
        button(text(["Y", "Cb", "Cr"][plane]).size(11))
            .padding([1, 6])
            .style(button::secondary)
            .on_press(Message::CyclePixelsPlane),
    ]
    .spacing(6)]
    .spacing(1);

    let bps = img.bytes_per_sample() as usize;
    let sample = |x: u32, y: u32| -> Option<u16> {
        if x >= pw || y >= ph {
            return None;
        }
        let idx = (y * pw + x) as usize * bps;
        let data = &img.planes[plane];
        if bps == 2 {
            Some(u16::from_le_bytes([*data.get(idx)?, *data.get(idx + 1)?]))
        } else {
            data.get(idx).map(|&v| v as u16)
        }
    };

    // Header row with x coordinates.
    let mut header = String::from("      ");
    for x in x0..(x0 + 2 * SPAN + 1).min(pw) {
        header.push_str(&if hex {
            format!("{x:>5x}")
        } else {
            format!("{x:>5}")
        });
    }
    col = col.push(text(header).size(10).font(iced::Font::MONOSPACE));

    for y in y0..(y0 + 2 * SPAN + 1).min(ph) {
        let mut line = if hex {
            format!("{y:>5x} ")
        } else {
            format!("{y:>5} ")
        };
        for x in x0..(x0 + 2 * SPAN + 1).min(pw) {
            match sample(x, y) {
                Some(v) if hex => line.push_str(&format!("{v:>5x}")),
                Some(v) => line.push_str(&format!("{v:>5}")),
                None => line.push_str("    ."),
            }
        }
        let is_center = y == sy;
        let mut t = text(line).size(10).font(iced::Font::MONOSPACE);
        if is_center {
            t = t.color(iced::Color::from_rgb(1.0, 0.95, 0.2));
        }
        col = col.push(t);
    }

    scrollable(col.padding(8)).width(Fill).height(Fill).into()
}
