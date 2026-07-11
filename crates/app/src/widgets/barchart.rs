//! The BarChart: one bar per frame (height = size, color = type), the main
//! navigation surface. Instanced rendering — 100k+ frames is one draw call.
//!
//! Besides the bars it renders the selection cursor, keyframe ticks and an
//! optional stepped QP graph.

use std::sync::Arc;

use iced::mouse;
use iced::wgpu;
use iced::widget::shader::{self, Viewport};
use iced::{Event, Rectangle};
use streamlens_core::StreamIndex;

use crate::theme;

/// Bar order on the x axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChartOrder {
    Decode,
    #[default]
    Display,
}

#[derive(Debug, Clone)]
pub enum Interaction {
    /// A bar was clicked (decode-order index).
    Select(u32),
    /// A bar is hovered (tooltip / status bar).
    Hover(Option<u32>),
    /// Scroll/zoom changed: (first visible bar, bar width px) — drives the
    /// time ruler under the chart.
    View { first: f32, bar_width: f32 },
}

pub struct BarChart<'a> {
    pub index: Option<&'a Arc<StreamIndex>>,
    pub selected: Option<u32>,
    pub order: ChartOrder,
    pub qp_graph: bool,
    /// PSNR-Y values indexed by decode index, drawn in orange when Some.
    pub psnr_graph: Option<&'a [Option<f32>]>,
    /// Per-frame (intra, inter, skip) bit sums indexed by decode index.
    /// When set, bars are drawn as stacked segments by prediction type
    /// (frames without sweep data yet fall back to the type color).
    pub bits_split: Option<&'a [BitsEntry]>,
}

/// (intra, inter, skip) bit sums of one frame, `None` until swept.
pub type BitsEntry = Option<(u64, u64, u64)>;

#[derive(Debug)]
pub struct ChartState {
    /// First visible bar (fractional, in bar units).
    pub scroll: f32,
    /// Bar width in pixels.
    pub bar_width: f32,
    /// Selection the view last auto-followed.
    followed: Option<u32>,
}

impl Default for ChartState {
    fn default() -> Self {
        Self {
            scroll: 0.0,
            bar_width: 4.0,
            followed: None,
        }
    }
}

impl<Message> shader::Program<Message> for BarChart<'_>
where
    Message: From<Interaction> + 'static,
{
    type State = ChartState;
    type Primitive = ChartPrimitive;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<shader::Action<Message>> {
        let index = self.index?;
        let n = index.frames.len() as f32;

        // Keep the selected frame in view while stepping/playing.
        if self.selected != state.followed {
            state.followed = self.selected;
            if let Some(sel) = self.selected {
                let bar = self.decode_index_to_bar(index, sel) as f32;
                let visible = bounds.width / state.bar_width;
                if bar < state.scroll + 1.0 || bar > state.scroll + visible - 2.0 {
                    state.scroll = (bar - visible / 2.0).clamp(0.0, (n - visible).max(0.0));
                    return Some(shader::Action::publish(
                        Interaction::View {
                            first: state.scroll,
                            bar_width: state.bar_width,
                        }
                        .into(),
                    ));
                }
            }
        }

        match event {
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let pos = cursor.position_in(bounds)?;
                let (dx, dy) = match delta {
                    mouse::ScrollDelta::Lines { x, y } => (*x * 30.0, *y),
                    mouse::ScrollDelta::Pixels { x, y } => (*x, *y / 40.0),
                };
                if dy != 0.0 {
                    // Zoom bar width around the cursor.
                    let at = state.scroll + pos.x / state.bar_width;
                    let factor = (1.0 + dy * 0.2).clamp(0.5, 2.0);
                    state.bar_width = (state.bar_width * factor).clamp(0.5, 64.0);
                    state.scroll = at - pos.x / state.bar_width;
                } else if dx != 0.0 {
                    state.scroll += dx / state.bar_width;
                }
                let visible = bounds.width / state.bar_width;
                state.scroll = state.scroll.clamp(0.0, (n - visible).max(0.0));
                Some(shader::Action::publish(
                    Interaction::View {
                        first: state.scroll,
                        bar_width: state.bar_width,
                    }
                    .into(),
                ))
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let pos = cursor.position_in(bounds)?;
                let bar = (state.scroll + pos.x / state.bar_width) as usize;
                let decode_index = self.bar_to_decode_index(index, bar)?;
                Some(shader::Action::publish(
                    Interaction::Select(decode_index).into(),
                ))
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let hovered = cursor.position_in(bounds).and_then(|pos| {
                    let bar = (state.scroll + pos.x / state.bar_width) as usize;
                    self.bar_to_decode_index(index, bar)
                });
                Some(shader::Action::publish(Interaction::Hover(hovered).into()))
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Self::State,
        _cursor: mouse::Cursor,
        bounds: Rectangle,
    ) -> ChartPrimitive {
        let mut bars = Vec::new();
        let mut max_size = 1u32;
        if let Some(index) = self.index {
            let first = state.scroll.max(0.0) as usize;
            let count = (bounds.width / state.bar_width).ceil() as usize + 1;
            let n = index.frames.len();
            let x_of = |bar: usize| (bar as f32 - state.scroll) * state.bar_width / bounds.width;
            let bar_w = ((state.bar_width - gap(state.bar_width)) / bounds.width).max(0.001);

            // Vertical scale from the visible window's max frame size.
            for bar in first..(first + count).min(n) {
                if let Some(i) = self.bar_to_decode_index(index, bar) {
                    max_size = max_size.max(index.frames[i as usize].size_bytes);
                }
            }

            for bar in first..(first + count).min(n) {
                let Some(i) = self.bar_to_decode_index(index, bar) else {
                    continue;
                };
                let f = &index.frames[i as usize];
                let x = x_of(bar);
                let h = f.size_bytes as f32 / max_size as f32 * 0.94;
                let split = self
                    .bits_split
                    .as_ref()
                    .and_then(|b| b.get(i as usize))
                    .copied()
                    .flatten();
                match split {
                    Some((intra, inter, skip)) if intra + inter + skip > 0 => {
                        // Same silhouette as the size bar, colored by where
                        // the bits went: skip bottom, inter, intra on top.
                        let total = (intra + inter + skip) as f32;
                        let mut y = 1.0;
                        for (part, color) in [
                            (skip, [0.45, 0.45, 0.45, 1.0]),
                            (inter, [0.30, 0.50, 0.95, 1.0]),
                            (intra, [0.90, 0.20, 0.20, 1.0]),
                        ] {
                            let ph = h * part as f32 / total;
                            if ph <= 0.0 {
                                continue;
                            }
                            y -= ph;
                            bars.push(Bar {
                                pos: [x, y],
                                size: [bar_w, ph],
                                color,
                            });
                        }
                    }
                    _ => bars.push(Bar {
                        pos: [x, 1.0 - h],
                        size: [bar_w, h],
                        color: theme::frame_type_rgba(f.frame_type),
                    }),
                }
                // Keyframe tick along the top edge.
                if f.is_keyframe {
                    bars.push(Bar {
                        pos: [x, 0.0],
                        size: [bar_w, 0.03],
                        color: [1.0, 0.55, 0.1, 1.0],
                    });
                }
            }

            // Stepped QP graph. Scale covers 0..51 but extends to the actual
            // stream range (negative SliceQPY at high bit depths, >51 never
            // clipped away).
            if self.qp_graph {
                let lo = (index.qp_range.0 as i16).min(0);
                let hi = (index.qp_range.1 as i16).max(51);
                let range = (hi - lo).max(1) as f32;
                for bar in first..(first + count).min(n) {
                    let Some(i) = self.bar_to_decode_index(index, bar) else {
                        continue;
                    };
                    let f = &index.frames[i as usize];
                    let y = 1.0 - ((f.slice_qp as i16 - lo) as f32 / range) * 0.94;
                    bars.push(Bar {
                        pos: [x_of(bar), y],
                        size: [state.bar_width / bounds.width, 0.012],
                        color: [0.35, 0.75, 1.0, 0.95],
                    });
                }
            }

            // PSNR graph: orange line, 20–60 dB mapped over the full height.
            if let Some(psnr) = &self.psnr_graph {
                for bar in first..(first + count).min(n) {
                    let Some(i) = self.bar_to_decode_index(index, bar) else {
                        continue;
                    };
                    let Some(Some(p)) = psnr.get(i as usize) else {
                        continue;
                    };
                    let t = ((p - 20.0) / 40.0).clamp(0.0, 1.0);
                    let y = 1.0 - t * 0.94;
                    bars.push(Bar {
                        pos: [x_of(bar), y],
                        size: [state.bar_width / bounds.width, 0.012],
                        color: [1.0, 0.55, 0.1, 0.95],
                    });
                }
            }

            // Playhead: accent line over the selected bar, with a cap on
            // top so it reads at a glance.
            if let Some(sel) = self.selected {
                const ACCENT: [f32; 4] = [0.30, 0.82, 0.88, 1.0];
                let bar = self.decode_index_to_bar(index, sel);
                let line_w = (2.0 / bounds.width).max(bar_w * 0.25);
                bars.push(Bar {
                    pos: [x_of(bar), 0.0],
                    size: [line_w, 1.0],
                    color: ACCENT,
                });
                let cap_w = 7.0 / bounds.width;
                bars.push(Bar {
                    pos: [x_of(bar) - (cap_w - line_w) / 2.0, 0.0],
                    size: [cap_w, 0.05],
                    color: ACCENT,
                });
            }
        }
        ChartPrimitive {
            bars: Arc::new(bars),
        }
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        mouse::Interaction::Pointer
    }
}

impl BarChart<'_> {
    fn bar_to_decode_index(&self, index: &StreamIndex, bar: usize) -> Option<u32> {
        if bar >= index.frames.len() {
            return None;
        }
        match self.order {
            ChartOrder::Decode => Some(bar as u32),
            ChartOrder::Display => index.decode_for_display(bar as u32),
        }
    }

    fn decode_index_to_bar(&self, index: &StreamIndex, decode_index: u32) -> usize {
        match self.order {
            ChartOrder::Decode => decode_index as usize,
            ChartOrder::Display => index.frames[decode_index as usize].display_index as usize,
        }
    }
}

fn gap(bar_width: f32) -> f32 {
    if bar_width >= 3.0 {
        1.0
    } else {
        0.0
    }
}

/// One bar instance in widget-normalized coordinates (0..1).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bar {
    pos: [f32; 2],
    size: [f32; 2],
    color: [f32; 4],
}

#[derive(Debug)]
pub struct ChartPrimitive {
    bars: Arc<Vec<Bar>>,
}

impl shader::Primitive for ChartPrimitive {
    type Pipeline = pipeline::ChartPipeline;

    fn prepare(
        &self,
        pipeline: &mut Self::Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
        pipeline.upload(device, queue, &self.bars);
    }

    fn draw(&self, pipeline: &Self::Pipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        pipeline.draw(render_pass, self.bars.len() as u32);
        true
    }
}

mod pipeline {
    use super::Bar;
    use iced::wgpu;
    use iced::widget::shader;

    pub struct ChartPipeline {
        pipeline: wgpu::RenderPipeline,
        bars: Option<wgpu::Buffer>,
        capacity: usize,
    }

    impl shader::Pipeline for ChartPipeline {
        fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
            let shader_module = device.create_shader_module(wgpu::include_wgsl!("barchart.wgsl"));
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("barchart layout"),
                bind_group_layouts: &[],
                push_constant_ranges: &[],
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("barchart pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader_module,
                    entry_point: Some("vs_main"),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Bar>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![
                            0 => Float32x2, // pos
                            1 => Float32x2, // size
                            2 => Float32x4, // color
                        ],
                    }],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader_module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: Default::default(),
                multiview: None,
                cache: None,
            });
            Self {
                pipeline,
                bars: None,
                capacity: 0,
            }
        }
    }

    impl ChartPipeline {
        pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, bars: &[Bar]) {
            if bars.is_empty() {
                return;
            }
            if bars.len() > self.capacity {
                self.capacity = bars.len().next_power_of_two();
                self.bars = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("bars"),
                    size: (self.capacity * std::mem::size_of::<Bar>()) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
            let buffer = self.bars.as_ref().expect("allocated above");
            let bytes = unsafe {
                std::slice::from_raw_parts(bars.as_ptr().cast(), std::mem::size_of_val(bars))
            };
            queue.write_buffer(buffer, 0, bytes);
        }

        pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, count: u32) {
            if count == 0 {
                return;
            }
            let Some(bars) = &self.bars else { return };
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, bars.slice(..));
            pass.draw(0..4, 0..count);
        }
    }
}
