//! The video frame view: a shader widget drawing the decoded picture with
//! zoom/pan and instanced analysis overlays.
//!
//! Zoom/pan state lives in the app (so toolbar buttons and shortcuts can
//! drive it); the widget publishes view changes from mouse interaction.

use std::sync::Arc;

use iced::mouse;
use iced::wgpu;
use iced::widget::shader::{self, Viewport};
use iced::{Event, Rectangle};
use streamlens_core::DecodedFrame;

use super::overlays::{Glyph, Segment};

/// Zoom limits: 12.5%–800%.
pub const MIN_ZOOM: f32 = 0.125;
pub const MAX_ZOOM: f32 = 8.0;

/// Zoom mode owned by the app.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Zoom {
    /// Fit the frame to the widget, centered.
    #[default]
    Fit,
    /// Fixed scale with an explicit pan center (image pixel at view center).
    Manual(f32),
}

/// Which plane(s) to display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelMode {
    #[default]
    Color,
    Y,
    U,
    V,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMatrix {
    Bt601,
    #[default]
    Bt709,
    Bt2020,
}

pub struct FrameView<'a> {
    pub frame: Option<&'a Arc<DecodedFrame>>,
    /// Replaces the decoded image (reconstruction stages, diff modes).
    pub override_image: Option<&'a Arc<streamlens_types::PlanarImage>>,
    /// When false, the widget neither publishes interactions nor reacts to
    /// the mouse (Block Presenter magnifier).
    pub interactive: bool,
    /// Distinct id per on-screen frame view. iced keeps one pipeline object
    /// per primitive type, so simultaneous views (video pane + block
    /// magnifier) must key their GPU resources by instance or the
    /// last-prepared view's transform would drive every draw.
    pub instance: u64,
    /// Monotonic id so the pipeline knows when to re-upload textures; must
    /// change when the displayed image (frame OR stage/diff) changes.
    pub frame_generation: u64,
    pub zoom: Zoom,
    /// Image pixel shown at the view center (Manual mode).
    pub center: (f32, f32),
    pub channel: ChannelMode,
    pub matrix: ColorMatrix,
    /// Overlay geometry, cached per frame by the app (zoom-independent;
    /// stroke width scales in the shader).
    pub segments: &'a Arc<Vec<Segment>>,
    /// Per-block value text sprites (same caching as segments).
    pub glyphs: &'a Arc<Vec<Glyph>>,
}

/// Widget-internal state: only the transient drag anchor.
#[derive(Debug, Default)]
pub struct ViewState {
    drag: Option<(f32, f32)>,
    /// Last effective zoom reported to the app.
    reported_zoom: f32,
}

/// Messages the frame view publishes.
#[derive(Debug, Clone)]
pub enum Interaction {
    /// Cursor moved over an image pixel (hover inspector).
    Hover(u16, u16),
    /// Click locked a block at this pixel.
    Select(u16, u16),
    /// Mouse interaction changed the view (switches app to Manual).
    SetView { zoom: f32, center: (f32, f32) },
    /// Current effective zoom (so toolbar buttons know where they start).
    EffectiveZoom(f32),
}

/// Effective zoom and pan center for the given frame/bounds.
pub fn effective_view(
    zoom: Zoom,
    center: (f32, f32),
    image: (f32, f32),
    bounds: (f32, f32),
) -> (f32, (f32, f32)) {
    let (iw, ih) = image;
    let (bw, bh) = bounds;
    match zoom {
        Zoom::Fit => {
            let z = (bw / iw).min(bh / ih).clamp(MIN_ZOOM, MAX_ZOOM);
            (z, (iw / 2.0, ih / 2.0))
        }
        Zoom::Manual(z) => (z, clamp_center(center, z, image, bounds)),
    }
}

/// Keeps the image on screen: axes smaller than the view stay centered,
/// larger ones cannot pan past their edges.
pub fn clamp_center(
    center: (f32, f32),
    zoom: f32,
    image: (f32, f32),
    bounds: (f32, f32),
) -> (f32, f32) {
    let clamp_axis = |c: f32, dim: f32, view: f32| {
        let half = view / (2.0 * zoom);
        if dim <= view / zoom {
            dim / 2.0
        } else {
            c.clamp(half, dim - half)
        }
    };
    (
        clamp_axis(center.0, image.0, bounds.0),
        clamp_axis(center.1, image.1, bounds.1),
    )
}

impl<Message> shader::Program<Message> for FrameView<'_>
where
    Message: From<Interaction> + 'static,
{
    type State = ViewState;
    type Primitive = FramePrimitive;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<shader::Action<Message>> {
        if !self.interactive {
            return None;
        }
        let frame = self.frame?;
        let image = (frame.image.width as f32, frame.image.height as f32);
        let view = (bounds.width, bounds.height);
        let (zoom, center) = effective_view(self.zoom, self.center, image, view);

        // Keep the app's idea of the effective zoom fresh (any event will
        // do; mouse traffic is plentiful).
        if (state.reported_zoom - zoom).abs() > 1e-4 {
            state.reported_zoom = zoom;
            return Some(shader::Action::publish(
                Interaction::EffectiveZoom(zoom).into(),
            ));
        }

        match event {
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let pos = cursor.position_in(bounds)?;
                let scroll = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / 40.0,
                };
                let factor = (1.0 + scroll * 0.15).clamp(0.5, 2.0);
                let new_zoom = (zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
                // Keep the image point under the cursor stationary.
                let (px, py) = widget_to_image(zoom, center, bounds, (pos.x, pos.y));
                let k = zoom / new_zoom;
                let new_center = clamp_center(
                    (px - (px - center.0) * k, py - (py - center.1) * k),
                    new_zoom,
                    image,
                    view,
                );
                Some(shader::Action::publish(
                    Interaction::SetView {
                        zoom: new_zoom,
                        center: new_center,
                    }
                    .into(),
                ))
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle)) => {
                let pos = cursor.position_in(bounds)?;
                state.drag = Some((pos.x, pos.y));
                Some(shader::Action::capture())
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Middle)) => {
                state.drag = None;
                None
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let pos = cursor.position_in(bounds)?;
                if let Some((lx, ly)) = state.drag {
                    let new_center = clamp_center(
                        (
                            center.0 - (pos.x - lx) / zoom,
                            center.1 - (pos.y - ly) / zoom,
                        ),
                        zoom,
                        image,
                        view,
                    );
                    state.drag = Some((pos.x, pos.y));
                    return Some(shader::Action::publish(
                        Interaction::SetView {
                            zoom,
                            center: new_center,
                        }
                        .into(),
                    ));
                }
                let (px, py) = widget_to_image(zoom, center, bounds, (pos.x, pos.y));
                if px >= 0.0 && py >= 0.0 && px < image.0 && py < image.1 {
                    return Some(shader::Action::publish(
                        Interaction::Hover(px as u16, py as u16).into(),
                    ));
                }
                None
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let pos = cursor.position_in(bounds)?;
                let (px, py) = widget_to_image(zoom, center, bounds, (pos.x, pos.y));
                if px >= 0.0 && py >= 0.0 && px < image.0 && py < image.1 {
                    return Some(shader::Action::publish(
                        Interaction::Select(px as u16, py as u16).into(),
                    ));
                }
                None
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        _cursor: mouse::Cursor,
        bounds: Rectangle,
    ) -> FramePrimitive {
        let mut zoom = 1.0;
        let mut center = (0.0, 0.0);
        if let Some(frame) = self.frame {
            let image = (frame.image.width as f32, frame.image.height as f32);
            (zoom, center) =
                effective_view(self.zoom, self.center, image, (bounds.width, bounds.height));
        }
        FramePrimitive {
            frame: self.frame.cloned(),
            override_image: self.override_image.cloned(),
            generation: self.frame_generation,
            instance: self.instance,
            zoom,
            center,
            bounds_size: (bounds.width, bounds.height),
            channel: self.channel,
            matrix: self.matrix,
            segments: self.segments.clone(),
            glyphs: self.glyphs.clone(),
        }
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.drag.is_some() {
            mouse::Interaction::Grabbing
        } else {
            mouse::Interaction::Crosshair
        }
    }
}

fn widget_to_image(zoom: f32, center: (f32, f32), bounds: Rectangle, p: (f32, f32)) -> (f32, f32) {
    (
        center.0 + (p.0 - bounds.width / 2.0) / zoom,
        center.1 + (p.1 - bounds.height / 2.0) / zoom,
    )
}

/// Everything needed to render one frame; produced per draw.
#[derive(Debug)]
pub struct FramePrimitive {
    frame: Option<Arc<DecodedFrame>>,
    override_image: Option<Arc<streamlens_types::PlanarImage>>,
    generation: u64,
    instance: u64,
    zoom: f32,
    center: (f32, f32),
    bounds_size: (f32, f32),
    channel: ChannelMode,
    matrix: ColorMatrix,
    segments: Arc<Vec<Segment>>,
    glyphs: Arc<Vec<Glyph>>,
}

impl shader::Primitive for FramePrimitive {
    type Pipeline = pipeline::FramePipeline;

    fn prepare(
        &self,
        pipeline: &mut Self::Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
        let Some(frame) = &self.frame else { return };
        let image: &streamlens_types::PlanarImage =
            self.override_image.as_deref().unwrap_or(&frame.image);

        let (shared, inst) = pipeline.instance_mut(device, self.instance);
        inst.upload_frame(device, queue, image, self.generation, &shared.sampler);

        // Image → widget-normalized transform.
        let (bw, bh) = self.bounds_size;
        let (iw, ih) = (image.width as f32, image.height as f32);
        let scale = (self.zoom / bw, self.zoom / bh);
        let offset = (
            0.5 - self.center.0 * self.zoom / bw,
            0.5 - self.center.1 * self.zoom / bh,
        );
        let sample_scale = if image.bit_depth > 8 {
            65535.0 / ((1 << image.bit_depth) - 1) as f32
        } else {
            1.0
        };
        inst.upload_uniforms(
            queue,
            [iw * scale.0, ih * scale.1, offset.0, offset.1],
            [
                match self.matrix {
                    ColorMatrix::Bt601 => 0.0,
                    ColorMatrix::Bt709 => 1.0,
                    ColorMatrix::Bt2020 => 2.0,
                },
                sample_scale,
                match self.channel {
                    ChannelMode::Color => 0.0,
                    ChannelMode::Y => 1.0,
                    ChannelMode::U => 2.0,
                    ChannelMode::V => 3.0,
                },
                0.0,
            ],
            [scale.0, scale.1, offset.0, offset.1],
            [(1.0 / self.zoom).clamp(0.05, 4.0), self.zoom, 0.0, 0.0],
        );
        inst.upload_segments(device, queue, &self.segments);
        inst.upload_glyphs(device, queue, &self.glyphs);
    }

    fn draw(&self, pipeline: &Self::Pipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        if self.frame.is_some() {
            if let Some((shared, inst)) = pipeline.instance(self.instance) {
                inst.draw(
                    shared,
                    render_pass,
                    self.segments.len() as u32,
                    self.glyphs.len() as u32,
                );
            }
        }
        true
    }
}

mod pipeline {
    use std::collections::HashMap;

    use super::*;
    use streamlens_types::PlanarImage;

    /// iced stores one pipeline object per primitive type, shared by every
    /// frame-view widget on screen. Pipelines/samplers/atlas are genuinely
    /// shareable; textures, uniforms and instance buffers are keyed per
    /// widget instance so simultaneous views don't overwrite each other.
    pub struct FramePipeline {
        pub(super) shared: Shared,
        instances: HashMap<u64, Instance>,
    }

    pub struct Shared {
        frame_pipeline: wgpu::RenderPipeline,
        overlay_pipeline: wgpu::RenderPipeline,
        glyph_pipeline: wgpu::RenderPipeline,
        pub(super) sampler: wgpu::Sampler,
        atlas_view: wgpu::TextureView,
        atlas_sampler: wgpu::Sampler,
    }

    /// Per-widget GPU resources.
    pub struct Instance {
        uniforms: wgpu::Buffer,
        overlay_uniforms: wgpu::Buffer,
        textures: Option<Textures>,
        bind_group: Option<wgpu::BindGroup>,
        overlay_bind_group: wgpu::BindGroup,
        glyph_bind_group: wgpu::BindGroup,
        segment_buffer: Option<wgpu::Buffer>,
        segment_capacity: usize,
        glyph_buffer: Option<wgpu::Buffer>,
        glyph_capacity: usize,
        uploaded_generation: u64,
    }

    struct Textures {
        planes: [wgpu::Texture; 3],
        dims: (u32, u32, u8),
    }

    impl FramePipeline {
        pub fn instance_mut(&mut self, device: &wgpu::Device, id: u64) -> (&Shared, &mut Instance) {
            let shared = &self.shared;
            let inst = self
                .instances
                .entry(id)
                .or_insert_with(|| Instance::new(device, shared));
            (shared, inst)
        }

        pub fn instance(&self, id: u64) -> Option<(&Shared, &Instance)> {
            self.instances.get(&id).map(|i| (&self.shared, i))
        }
    }

    impl shader::Pipeline for FramePipeline {
        fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
            let frame_shader = device.create_shader_module(wgpu::include_wgsl!("frame_view.wgsl"));
            let overlay_shader = device.create_shader_module(wgpu::include_wgsl!("overlay.wgsl"));

            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("frame sampler"),
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });

            let frame_layout = frame_bind_group_layout(device);
            let frame_pipeline_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("frame pipeline layout"),
                    bind_group_layouts: &[&frame_layout],
                    push_constant_ranges: &[],
                });
            let frame_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("frame pipeline"),
                layout: Some(&frame_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &frame_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &frame_shader,
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

            let overlay_layout = overlay_bind_group_layout(device);
            let overlay_pipeline_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("overlay pipeline layout"),
                    bind_group_layouts: &[&overlay_layout],
                    push_constant_ranges: &[],
                });
            let overlay_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("overlay pipeline"),
                layout: Some(&overlay_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &overlay_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Segment>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![
                            0 => Float32x4, // seg
                            1 => Float32,   // width
                            2 => Float32,   // stroke flag
                            3 => Float32x4, // color
                        ],
                    }],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &overlay_shader,
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

            // Digit atlas + glyph pipeline (per-block value text).
            let glyph_shader = device.create_shader_module(wgpu::include_wgsl!("glyphs.wgsl"));
            let atlas = build_digit_atlas(device, queue);
            let atlas_view = atlas.create_view(&Default::default());
            let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("glyph sampler"),
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            });
            let glyph_layout = glyph_bind_group_layout(device);
            let glyph_pipeline_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("glyph pipeline layout"),
                    bind_group_layouts: &[&glyph_layout],
                    push_constant_ranges: &[],
                });
            let glyph_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("glyph pipeline"),
                layout: Some(&glyph_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &glyph_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Glyph>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![
                            0 => Float32x2, // pos
                            1 => Float32x2, // size
                            2 => Float32,   // glyph
                            3 => Float32,   // _pad
                            4 => Float32x4, // color
                        ],
                    }],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &glyph_shader,
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
                shared: Shared {
                    frame_pipeline,
                    overlay_pipeline,
                    glyph_pipeline,
                    sampler,
                    atlas_view,
                    atlas_sampler,
                },
                instances: HashMap::new(),
            }
        }
    }

    impl Instance {
        fn new(device: &wgpu::Device, shared: &Shared) -> Self {
            let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("frame uniforms"),
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let overlay_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("overlay uniforms"),
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let overlay_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("overlay bind group"),
                layout: &overlay_bind_group_layout(device),
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: overlay_uniforms.as_entire_binding(),
                }],
            });
            let glyph_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("glyph bind group"),
                layout: &glyph_bind_group_layout(device),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: overlay_uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&shared.atlas_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&shared.atlas_sampler),
                    },
                ],
            });
            Self {
                uniforms,
                overlay_uniforms,
                textures: None,
                bind_group: None,
                overlay_bind_group,
                glyph_bind_group,
                segment_buffer: None,
                segment_capacity: 0,
                glyph_buffer: None,
                glyph_capacity: 0,
                uploaded_generation: u64::MAX,
            }
        }

        pub fn upload_frame(
            &mut self,
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            img: &PlanarImage,
            generation: u64,
            sampler: &wgpu::Sampler,
        ) {
            if self.uploaded_generation == generation {
                return;
            }
            let needs_alloc = self
                .textures
                .as_ref()
                .is_none_or(|t| t.dims != (img.width, img.height, img.bit_depth));
            if needs_alloc {
                let format = if img.bit_depth > 8 {
                    wgpu::TextureFormat::R16Unorm
                } else {
                    wgpu::TextureFormat::R8Unorm
                };
                let make = |w: u32, h: u32| {
                    device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("plane"),
                        size: wgpu::Extent3d {
                            width: w.max(1),
                            height: h.max(1),
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    })
                };
                let (cw, ch) = img.plane_dims(1);
                let textures = Textures {
                    planes: [make(img.width, img.height), make(cw, ch), make(cw, ch)],
                    dims: (img.width, img.height, img.bit_depth),
                };
                let views: Vec<wgpu::TextureView> = textures
                    .planes
                    .iter()
                    .map(|t| t.create_view(&Default::default()))
                    .collect();
                self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("frame bind group"),
                    layout: &frame_bind_group_layout(device),
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&views[0]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&views[1]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(&views[2]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::Sampler(sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: self.uniforms.as_entire_binding(),
                        },
                    ],
                }));
                self.textures = Some(textures);
            }

            let textures = self.textures.as_ref().expect("allocated above");
            for (i, tex) in textures.planes.iter().enumerate() {
                write_plane(queue, tex, img, i);
            }
            self.uploaded_generation = generation;
        }

        pub fn upload_uniforms(
            &self,
            queue: &wgpu::Queue,
            transform: [f32; 4],
            params: [f32; 4],
            overlay_transform: [f32; 4],
            overlay_params: [f32; 4],
        ) {
            let mut data = [0u8; 32];
            data[..16].copy_from_slice(bytemuck_cast(&transform));
            data[16..].copy_from_slice(bytemuck_cast(&params));
            queue.write_buffer(&self.uniforms, 0, &data);
            let mut odata = [0u8; 32];
            odata[..16].copy_from_slice(bytemuck_cast(&overlay_transform));
            odata[16..].copy_from_slice(bytemuck_cast(&overlay_params));
            queue.write_buffer(&self.overlay_uniforms, 0, &odata);
        }

        pub fn upload_glyphs(
            &mut self,
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            glyphs: &[Glyph],
        ) {
            if glyphs.is_empty() {
                return;
            }
            if glyphs.len() > self.glyph_capacity {
                self.glyph_capacity = glyphs.len().next_power_of_two();
                self.glyph_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("glyph instances"),
                    size: (self.glyph_capacity * std::mem::size_of::<Glyph>()) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
            let buffer = self.glyph_buffer.as_ref().expect("allocated above");
            let bytes = unsafe {
                std::slice::from_raw_parts(glyphs.as_ptr().cast(), std::mem::size_of_val(glyphs))
            };
            queue.write_buffer(buffer, 0, bytes);
        }

        pub fn upload_segments(
            &mut self,
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            segments: &[Segment],
        ) {
            if segments.is_empty() {
                return;
            }
            if segments.len() > self.segment_capacity {
                self.segment_capacity = segments.len().next_power_of_two();
                self.segment_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("overlay segments"),
                    size: (self.segment_capacity * std::mem::size_of::<Segment>()) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
            let buffer = self.segment_buffer.as_ref().expect("allocated above");
            queue.write_buffer(buffer, 0, segment_bytes(segments));
        }

        pub fn draw(
            &self,
            shared: &Shared,
            pass: &mut wgpu::RenderPass<'_>,
            segment_count: u32,
            glyph_count: u32,
        ) {
            let Some(bind_group) = &self.bind_group else {
                return;
            };
            pass.set_pipeline(&shared.frame_pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..4, 0..1);

            if segment_count > 0 {
                if let Some(segments) = &self.segment_buffer {
                    pass.set_pipeline(&shared.overlay_pipeline);
                    pass.set_bind_group(0, &self.overlay_bind_group, &[]);
                    pass.set_vertex_buffer(0, segments.slice(..));
                    pass.draw(0..4, 0..segment_count);
                }
            }
            if glyph_count > 0 {
                if let Some(glyphs) = &self.glyph_buffer {
                    pass.set_pipeline(&shared.glyph_pipeline);
                    pass.set_bind_group(0, &self.glyph_bind_group, &[]);
                    pass.set_vertex_buffer(0, glyphs.slice(..));
                    pass.draw(0..4, 0..glyph_count);
                }
            }
        }
    }

    fn overlay_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay bind group layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        })
    }

    fn glyph_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glyph bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        })
    }

    fn frame_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame bind group layout"),
            entries: &[
                texture(0),
                texture(1),
                texture(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        })
    }

    fn write_plane(queue: &wgpu::Queue, tex: &wgpu::Texture, img: &PlanarImage, plane: usize) {
        let Some(data) = img.planes.get(plane) else {
            return;
        };
        let (w, h) = img.plane_dims(plane);
        if w == 0 || h == 0 {
            return;
        }
        let bpp = img.bytes_per_sample();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * bpp),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    fn bytemuck_cast(v: &[f32; 4]) -> &[u8] {
        // f32 slices are always safely viewable as bytes.
        unsafe { std::slice::from_raw_parts(v.as_ptr().cast(), 16) }
    }

    fn segment_bytes(segments: &[Segment]) -> &[u8] {
        // Segment is #[repr(C)] with only f32 fields.
        unsafe {
            std::slice::from_raw_parts(segments.as_ptr().cast(), std::mem::size_of_val(segments))
        }
    }

    /// 5x7 bitmap font strip for '0'-'9' and '-' (row bits, MSB = left).
    const DIGIT_FONT: [[u8; 7]; 11] = [
        [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E], // 0
        [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E], // 1
        [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F], // 2
        [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E], // 3
        [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02], // 4
        [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E], // 5
        [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E], // 6
        [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08], // 7
        [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E], // 8
        [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C], // 9
        [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00], // -
    ];

    fn build_digit_atlas(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::Texture {
        let (gw, gh, n) = (5u32, 7u32, DIGIT_FONT.len() as u32);
        let mut pixels = vec![0u8; (gw * n * gh) as usize];
        for (g, rows) in DIGIT_FONT.iter().enumerate() {
            for (y, row) in rows.iter().enumerate() {
                for x in 0..gw {
                    if row >> (gw - 1 - x) & 1 != 0 {
                        pixels[y * (gw * n) as usize + g * gw as usize + x as usize] = 0xFF;
                    }
                }
            }
        }
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("digit atlas"),
            size: wgpu::Extent3d {
                width: gw * n,
                height: gh,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(gw * n),
                rows_per_image: Some(gh),
            },
            wgpu::Extent3d {
                width: gw * n,
                height: gh,
                depth_or_array_layers: 1,
            },
        );
        tex
    }
}
