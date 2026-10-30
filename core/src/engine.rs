//! GPU engine: owns the device/surface, the culled scene buffers and the
//! camera. The platform shell feeds it input and asks for frames.

use crate::scene::{Label, Scene};
use crate::spatial::{reorder, Index};
use crate::text::{Text, ATLAS, LINE_H};
use bytemuck::{Pod, Zeroable};
use std::time::Instant;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CamU {
    center_hi: [f32; 2],
    center_lo: [f32; 2],
    viewport: [f32; 2],
    scale: f32,
    /// Coverage multiplier for decimated LOD levels.
    boost: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RectGpu {
    xywh: [f32; 4],
    fill: u32,
    stroke: u32,
    shape: u32,
    sw: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LineGpu {
    pts: [f32; 4],
    color: u32,
    width: f32,
    kind: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GlyphGpu {
    xywh: [f32; 4],
    uv: [f32; 4],
    color: u32,
    /// 1 = the atlas holds a distance field for this glyph.
    sdf: u32,
    _pad: [u32; 2],
}

#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub cx: f64,
    pub cy: f64,
    /// Pixels per world unit.
    pub s: f64,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Stats {
    pub frame_ms: f32,
    pub text_ms: f32,
    pub rects: u32,
    pub lines: u32,
    pub glyphs: u32,
    pub draws: u32,
}

pub const MIN_SCALE: f64 = 1e-5;
pub const MAX_SCALE: f64 = 64.0;
const BG: wgpu::Color = wgpu::Color { r: 0.969, g: 0.965, b: 0.953, a: 1.0 };

fn split(v: f64) -> (f32, f32) {
    let hi = v as f32;
    (hi, (v - hi as f64) as f32)
}

fn rect_gpu(r: &crate::scene::Rect) -> RectGpu {
    RectGpu {
        xywh: [r.x, r.y, r.w, r.h],
        fill: r.fill,
        stroke: r.stroke,
        shape: r.shape as u32 | (r.radius.clamp(0.0, 16_000_000.0) as u32) << 8,
        sw: r.stroke_w,
    }
}

/// One instance per segment plus one per arrowhead.
fn push_line(out: &mut Vec<LineGpu>, l: &crate::scene::Line) {
    out.push(LineGpu { pts: [l.x0, l.y0, l.x1, l.y1], color: l.color, width: l.width, kind: 0, _pad: 0 });
    if l.head {
        let (dx, dy) = (l.x1 - l.x0, l.y1 - l.y0);
        let len = (dx * dx + dy * dy).sqrt().max(1e-6);
        let hl = (len * 0.4).min(10.0 + 4.0 * l.width);
        let back = [l.x1 - dx / len * hl, l.y1 - dy / len * hl];
        out.push(LineGpu { pts: [back[0], back[1], l.x1, l.y1], color: l.color, width: l.width, kind: 1, _pad: 0 });
    }
}

/// A small per-frame vertex buffer that grows as needed.
#[derive(Default)]
struct DynBuf {
    buf: Option<wgpu::Buffer>,
    cap: usize,
    count: u32,
}

impl DynBuf {
    fn write<T: Pod>(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, data: &[T]) {
        let bytes = std::mem::size_of_val(data);
        if bytes > self.cap {
            self.cap = (bytes * 2).max(4096);
            self.buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("overlay"),
                size: self.cap as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        if let (Some(b), false) = (&self.buf, data.is_empty()) {
            queue.write_buffer(b, 0, bytemuck::cast_slice(data));
        }
        self.count = data.len() as u32;
    }
}

fn intersects(a: &[f32; 4], b: &[f32; 4]) -> bool {
    a[0] <= b[2] && a[2] >= b[0] && a[1] <= b[3] && a[3] >= b[1]
}

pub struct Engine {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: Option<wgpu::Surface<'static>>,
    config: wgpu::SurfaceConfiguration,
    rect_pipe: wgpu::RenderPipeline,
    line_pipe: wgpu::RenderPipeline,
    glyph_pipe: wgpu::RenderPipeline,
    cam_buf: wgpu::Buffer,
    cam_bg: wgpu::BindGroup,
    /// Identity camera for the screen-space UI layer.
    ui_cam_buf: wgpu::Buffer,
    ui_cam_bg: wgpu::BindGroup,
    atlas_tex: wgpu::Texture,
    wrap_cache: std::cell::RefCell<std::collections::HashMap<(String, u32), usize>>,
    atlas_bg: wgpu::BindGroup,
    /// Level 0 = every shape; level k = every 8^k-th, for far zoom.
    rect_lods: Vec<(Index, wgpu::Buffer)>,
    rect_median: f32,
    line_buf: Option<wgpu::Buffer>,
    glyph_buf: Option<wgpu::Buffer>,
    glyph_cap: usize,
    glyph_count: u32,

    scene: Scene,
    label_bb: Vec<[f32; 4]>,
    /// (longest line in class, index, first instance)
    line_classes: Vec<(f32, Index, u32)>,
    label_idx: Index,
    text: Text,
    prepared: Option<([f32; 4], f64)>,
    ranges: Vec<(u32, u32)>,
    glyphs: Vec<GlyphGpu>,
    /// Drawn on top of the scene: selection, things being dragged or typed.
    ov_rects: DynBuf,
    ov_lines: DynBuf,
    ov_glyphs: DynBuf,
    /// Screen-space UI on top of everything, in window pixels.
    ui_rects: DynBuf,
    ui_lines: DynBuf,
    ui_glyphs: DynBuf,
    /// Canvas background (window clear colour).
    pub bg: wgpu::Color,

    pub cam: Camera,
    pub stats: Stats,
    pub adapter_info: String,
}

impl Engine {
    pub async fn new(
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        font: Vec<u8>,
        high_perf: bool,
    ) -> Engine {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: if high_perf {
                    wgpu::PowerPreference::HighPerformance
                } else {
                    wgpu::PowerPreference::LowPower
                },
                force_fallback_adapter: false,
                apply_limit_buckets: false,
                compatible_surface: Some(&surface),
            })
            .await
            .expect("no GPU adapter");
        let ai = adapter.get_info();
        let adapter_info = format!("{} ({:?})", ai.name, ai.backend);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: None,
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .expect("no device");

        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .expect("surface unsupported");
        // Colours are sRGB values blended in gamma space, like browsers and
        // Excalidraw; an sRGB target would encode them a second time.
        config.format = config.format.remove_srgb_suffix();
        config.view_formats = vec![];
        config.present_mode = wgpu::PresentMode::AutoVsync;
        config.desired_maximum_frame_latency = std::env::var("HS_LATENCY").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shaders"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders.wgsl").into()),
        });

        let cam_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cam"),
            size: std::mem::size_of::<CamU>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cam_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
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
        });
        let cam_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &cam_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: cam_buf.as_entire_binding() }],
        });
        let ui_cam_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ui cam"),
            size: std::mem::size_of::<CamU>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ui_cam_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &cam_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: ui_cam_buf.as_entire_binding() }],
        });

        let atlas_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas"),
            size: wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let atlas_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let atlas_view = atlas_tex.create_view(&Default::default());
        let atlas_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &atlas_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&atlas_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });

        let layout1 = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&cam_bgl)],
            immediate_size: 0,
        });
        let layout2 = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&cam_bgl), Some(&atlas_bgl)],
            immediate_size: 0,
        });

        let blend = wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING;
        let target = [Some(wgpu::ColorTargetState {
            format: config.format,
            blend: Some(blend),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let make = |layout: &wgpu::PipelineLayout, vs: &str, fs: &str, stride: u64, attrs: &[wgpu::VertexAttribute]| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(vs),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: stride,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: attrs,
                    })],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &target,
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let rect_pipe = make(
            &layout1,
            "vs_rect",
            "fs_rect",
            32,
            &wgpu::vertex_attr_array![0 => Float32x4, 1 => Uint32, 2 => Uint32, 3 => Uint32, 4 => Float32],
        );
        let line_pipe = make(
            &layout1,
            "vs_line",
            "fs_line",
            32,
            &wgpu::vertex_attr_array![0 => Float32x4, 1 => Uint32, 2 => Float32, 3 => Uint32],
        );
        let glyph_pipe = make(
            &layout2,
            "vs_glyph",
            "fs_glyph",
            48,
            &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Uint32, 3 => Uint32],
        );

        Engine {
            device,
            queue,
            surface: Some(surface),
            config,
            rect_pipe,
            line_pipe,
            glyph_pipe,
            cam_buf,
            cam_bg,
            ui_cam_buf,
            ui_cam_bg,
            atlas_tex,
            wrap_cache: Default::default(),
            atlas_bg,
            rect_lods: Vec::new(),
            rect_median: 1.0,
            line_buf: None,
            glyph_buf: None,
            glyph_cap: 0,
            glyph_count: 0,
            scene: Scene::default(),
            label_bb: Vec::new(),
            line_classes: Vec::new(),
            label_idx: Index::build(&[], &[]).1,
            text: Text::new(font),
            prepared: None,
            ranges: Vec::new(),
            glyphs: Vec::new(),
            ov_rects: DynBuf::default(),
            ov_lines: DynBuf::default(),
            ov_glyphs: DynBuf::default(),
            ui_rects: DynBuf::default(),
            ui_lines: DynBuf::default(),
            ui_glyphs: DynBuf::default(),
            bg: BG,
            cam: Camera { cx: 0.0, cy: 0.0, s: 1.0 },
            stats: Stats::default(),
            adapter_info,
        }
    }

    pub fn set_scene(&mut self, mut scene: Scene) {
        use wgpu::util::DeviceExt;
        // Shapes.
        let bb: Vec<[f32; 4]> = scene.rects.iter().map(|r| [r.x, r.y, r.x + r.w, r.y + r.h]).collect();
        let sz: Vec<f32> = scene.rects.iter().map(|r| r.w.max(r.h)).collect();
        let (perm, idx) = Index::build(&bb, &sz);
        reorder(&mut scene.rects, &perm);
        let sz: Vec<f32> = perm.iter().map(|&i| sz[i as usize]).collect();
        let rects: Vec<RectGpu> = scene.rects.iter().map(rect_gpu).collect();
        let mut med: Vec<f32> = sz.clone();
        med.sort_by(|a, b| a.partial_cmp(b).unwrap());
        self.rect_median = med.get(med.len() / 2).copied().unwrap_or(1.0);
        self.rect_lods.clear();
        let upload = |v: &[RectGpu]| {
            self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("rects"),
                contents: bytemuck::cast_slice(v),
                usage: wgpu::BufferUsages::VERTEX,
            })
        };
        if !rects.is_empty() {
            let mut lods = vec![(idx, upload(&rects))];
            let mut cur = rects;
            while cur.len() > 20_000 {
                // Every 8th of a Morton-sorted list is a spatially even sample.
                let next: Vec<RectGpu> = cur.iter().step_by(8).copied().collect();
                let bb: Vec<[f32; 4]> = next.iter().map(|r| [r.xywh[0], r.xywh[1], r.xywh[0] + r.xywh[2], r.xywh[1] + r.xywh[3]]).collect();
                let sz: Vec<f32> = next.iter().map(|r| r.xywh[2].max(r.xywh[3])).collect();
                let (_, i) = Index::build(&bb, &sz);
                // Already in Morton order, so the permutation is the identity.
                lods.push((i, upload(&next)));
                cur = next;
            }
            self.rect_lods = lods;
        }

        // Lines: one instance per segment plus one per arrowhead.
        let mut lines: Vec<LineGpu> = Vec::with_capacity(scene.lines.len() * 2);
        for l in &scene.lines {
            push_line(&mut lines, l);
        }
        // Group by length class (powers of two) so far zoom can skip every
        // class that is too short to see without touching its instances.
        let len_of = |l: &LineGpu| (l.pts[2] - l.pts[0]).hypot(l.pts[3] - l.pts[1]).max(l.width);
        let mut classes: Vec<Vec<LineGpu>> = vec![Vec::new(); 40];
        for l in lines {
            let c = (len_of(&l).max(1e-3).log2().floor() as i32 + 12).clamp(0, 39) as usize;
            classes[c].push(l);
        }
        let mut all: Vec<LineGpu> = Vec::new();
        self.line_classes.clear();
        for (c, mut v) in classes.into_iter().enumerate() {
            if v.is_empty() {
                continue;
            }
            let bb: Vec<[f32; 4]> = v
                .iter()
                .map(|l| {
                    let p = l.pts;
                    let pad = if l.kind == 1 { ((p[2] - p[0]).hypot(p[3] - p[1])) * 0.5 } else { l.width };
                    [p[0].min(p[2]) - pad, p[1].min(p[3]) - pad, p[0].max(p[2]) + pad, p[1].max(p[3]) + pad]
                })
                .collect();
            let sz: Vec<f32> = v.iter().map(len_of).collect();
            let (perm, idx) = Index::build(&bb, &sz);
            reorder(&mut v, &perm);
            let max_len = 2f32.powi(c as i32 - 12 + 1);
            self.line_classes.push((max_len, idx, all.len() as u32));
            all.extend(v);
        }
        self.line_buf = (!all.is_empty()).then(|| {
            self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("lines"),
                contents: bytemuck::cast_slice(&all),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });

        // Labels.
        let bb: Vec<[f32; 4]> = scene
            .labels
            .iter()
            .map(|t| [t.x, t.y, t.x + t.w.max(t.size), t.y + t.size * LINE_H * t.line_count() as f32])
            .collect();
        let sz: Vec<f32> = scene.labels.iter().map(|t| t.size).collect();
        let (perm, idx) = Index::build(&bb, &sz);
        reorder(&mut scene.labels, &perm);
        self.label_bb = perm.iter().map(|&i| bb[i as usize]).collect();
        self.label_idx = idx;

        self.scene = scene;
        self.prepared = None;
        self.text.clear_layouts();
    }

    /// Replaces the overlay. Sizes are in world units, so call it again
    /// after zooming if it holds screen-sized things like outlines.
    pub fn set_overlay(&mut self, ov: &Scene) {
        let rects: Vec<RectGpu> = ov.rects.iter().map(rect_gpu).collect();
        let mut lines = Vec::new();
        for l in &ov.lines {
            push_line(&mut lines, l);
        }
        let mut glyphs = Vec::new();
        for label in &ov.labels {
            self.label_glyphs(label, &mut glyphs);
        }
        self.upload_atlas();
        self.ov_rects.write(&self.device, &self.queue, &rects);
        self.ov_lines.write(&self.device, &self.queue, &lines);
        self.ov_glyphs.write(&self.device, &self.queue, &glyphs);
    }

    /// Replaces the screen-space UI layer (coordinates in window pixels).
    pub fn set_ui(&mut self, ui: &Scene) {
        let rects: Vec<RectGpu> = ui.rects.iter().map(rect_gpu).collect();
        let mut lines = Vec::new();
        for l in &ui.lines {
            push_line(&mut lines, l);
        }
        let mut glyphs = Vec::new();
        for label in &ui.labels {
            self.glyphs_at_scale(label, 1.0, &mut glyphs);
        }
        self.upload_atlas();
        self.ui_rects.write(&self.device, &self.queue, &rects);
        self.ui_lines.write(&self.device, &self.queue, &lines);
        self.ui_glyphs.write(&self.device, &self.queue, &glyphs);
    }

    fn label_glyphs(&mut self, label: &Label, out: &mut Vec<GlyphGpu>) {
        self.glyphs_at_scale(label, self.cam.s as f32, out);
    }

    fn glyphs_at_scale(&mut self, label: &Label, scale: f32, out: &mut Vec<GlyphGpu>) {
        let px = label.size * scale;
        if px < 1.0 {
            return;
        }
        let bucket = Text::bucket_for(px);
        let ratio = label.size / crate::text::BUCKETS[bucket as usize];
        let baseline = self.text.baseline();
        for (g, gx, line) in self.text.make_layout(label).glyphs {
            if let Ok(Some(ag)) = self.text.glyph(g, bucket) {
                out.push(GlyphGpu {
                    xywh: [
                        label.x + gx * label.size + ag.min[0] * ratio,
                        label.y + (line as f32 * LINE_H + baseline) * label.size + ag.min[1] * ratio,
                        ag.size[0] * ratio,
                        ag.size[1] * ratio,
                    ],
                    uv: ag.uv,
                    color: label.color,
                    sdf: ag.sdf as u32,
                    _pad: [0; 2],
                });
            }
        }
    }

    /// Size of `text` at font `size` in world units, laid out like a label.
    pub fn measure(&self, text: &str, size: f32) -> (f32, f32) {
        let (w, n) = self.text.measure(text);
        (w * size, n as f32 * LINE_H * size)
    }

    /// Where a caret after the last character of `label` goes (top of the line).
    pub fn caret(&self, label: &Label) -> (f32, f32) {
        let mut l = label.clone();
        l.text.push('|');
        match self.text.make_layout(&l).glyphs.last() {
            Some(&(_, gx, line)) => (l.x + gx * l.size, l.y + line as f32 * LINE_H * l.size),
            None => (l.x, l.y),
        }
    }

    /// Lines `text` takes when word-wrapped to `width` at font `size`. Only
    /// the ratio width/size matters, so results are remembered by text and
    /// ratio: every scene rebuild asks again for every label.
    pub fn wrapped_lines(&self, text: &str, size: f32, width: f32) -> usize {
        let key = (text.to_string(), (width / size).to_bits());
        if let Some(&n) = self.wrap_cache.borrow().get(&key) {
            return n;
        }
        let label = Label { x: 0.0, y: 0.0, w: width, size, color: 0, align: crate::scene::Align::Left, wrap: true, text: text.to_string() };
        let paras = text.split('\n').count();
        let n = self.text.make_layout(&label).glyphs.last().map_or(paras, |g| g.2 as usize + 1).max(paras);
        let mut c = self.wrap_cache.borrow_mut();
        if c.len() > 50_000 {
            c.clear();
        }
        c.insert(key, n);
        n
    }

    pub fn screen_to_world(&self, x: f64, y: f64) -> (f64, f64) {
        let (hw, hh) = (self.config.width as f64 * 0.5, self.config.height as f64 * 0.5);
        (self.cam.cx + (x - hw) / self.cam.s, self.cam.cy + (y - hh) / self.cam.s)
    }

    fn upload_atlas(&mut self) {
        if let Some((y0, y1)) = self.text.dirty.take() {
            let start = (y0 * ATLAS) as usize;
            let end = (y1.min(ATLAS) * ATLAS) as usize;
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: y0, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &self.text.pixels[start..end],
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(ATLAS), rows_per_image: None },
                wgpu::Extent3d { width: ATLAS, height: y1.min(ATLAS) - y0, depth_or_array_layers: 1 },
            );
        }
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        if let Some(sf) = &self.surface {
            sf.configure(&self.device, &self.config);
        }
        self.prepared = None;
    }

    /// Android: the window goes away when the app is backgrounded.
    pub fn detach_surface(&mut self) {
        self.surface = None;
    }

    pub fn attach_surface(&mut self, surface: wgpu::Surface<'static>, w: u32, h: u32) {
        self.surface = Some(surface);
        self.resize(w, h);
    }

    pub fn set_vsync(&mut self, on: bool) {
        self.config.present_mode = if on { wgpu::PresentMode::AutoVsync } else { wgpu::PresentMode::AutoNoVsync };
        if let Some(sf) = &self.surface {
            sf.configure(&self.device, &self.config);
        }
    }

    pub fn fit(&mut self) {
        let b = self.scene.bounds();
        self.fit_box(b, 0.92);
    }

    /// Shows world box `b` filling `fill` of the window.
    pub fn fit_box(&mut self, b: [f32; 4], fill: f64) {
        let (w, h) = (self.config.width as f64, self.config.height as f64);
        let bw = (b[2] - b[0]).max(1.0) as f64;
        let bh = (b[3] - b[1]).max(1.0) as f64;
        self.cam.s = ((w / bw).min(h / bh) * fill).clamp(MIN_SCALE, MAX_SCALE);
        self.cam.cx = (b[0] as f64 + b[2] as f64) * 0.5;
        self.cam.cy = (b[1] as f64 + b[3] as f64) * 0.5;
    }

    pub fn pan(&mut self, dx: f64, dy: f64) {
        self.cam.cx -= dx / self.cam.s;
        self.cam.cy -= dy / self.cam.s;
    }

    /// Zoom by `f` keeping the world point under screen (x, y) fixed.
    pub fn zoom_at(&mut self, f: f64, x: f64, y: f64) {
        let (hw, hh) = (self.config.width as f64 * 0.5, self.config.height as f64 * 0.5);
        let wx = self.cam.cx + (x - hw) / self.cam.s;
        let wy = self.cam.cy + (y - hh) / self.cam.s;
        self.cam.s = (self.cam.s * f).clamp(MIN_SCALE, MAX_SCALE);
        self.cam.cx = wx - (x - hw) / self.cam.s;
        self.cam.cy = wy - (y - hh) / self.cam.s;
    }

    pub fn view_rect(&self) -> [f32; 4] {
        let hw = self.config.width as f64 * 0.5 / self.cam.s;
        let hh = self.config.height as f64 * 0.5 / self.cam.s;
        [
            (self.cam.cx - hw) as f32,
            (self.cam.cy - hh) as f32,
            (self.cam.cx + hw) as f32,
            (self.cam.cy + hh) as f32,
        ]
    }

    fn text_needs_rebuild(&self, view: &[f32; 4]) -> bool {
        match self.prepared {
            None => true,
            Some((r, s)) => {
                let ratio = self.cam.s / s;
                !(view[0] >= r[0] && view[1] >= r[1] && view[2] <= r[2] && view[3] <= r[3]) || !(0.84..=1.19).contains(&ratio)
            }
        }
    }

    fn rebuild_text(&mut self, view: &[f32; 4]) {
        let t0 = Instant::now();
        let (vw, vh) = (view[2] - view[0], view[3] - view[1]);
        let region = [view[0] - vw * 0.5, view[1] - vh * 0.5, view[2] + vw * 0.5, view[3] + vh * 0.5];
        let s = self.cam.s as f32;
        let mut ranges = std::mem::take(&mut self.ranges);
        self.label_idx.query(&region, 0.25 / s, &mut ranges);
        let baseline = self.text.baseline();
        let mut scratch = Vec::new();
        for attempt in 0..2 {
            self.glyphs.clear();
            let mut full = false;
            'outer: for &(a, b) in &ranges {
                for i in a..b {
                    if !intersects(&self.label_bb[i as usize], &region) {
                        continue;
                    }
                    let label = &self.scene.labels[i as usize];
                    let px = label.size * s;
                    if px < 0.25 {
                        continue;
                    }
                    let mut color = label.color;
                    if px < 2.0 {
                        // Too small to read: one soft bar per line.
                        if self.glyphs.len() > 400_000 {
                            continue;
                        }
                        let a = ((color >> 24) as f32 * 0.35) as u32;
                        color = color & 0x00ff_ffff | a << 24;
                        for (j, line) in label.text.split('\n').enumerate() {
                            let n = line.chars().count() as f32;
                            if n == 0.0 {
                                continue;
                            }
                            let mut w = n * 0.5 * label.size;
                            if label.w > 0.0 {
                                w = w.min(label.w);
                            }
                            let off = match label.align {
                                crate::scene::Align::Left => 0.0,
                                crate::scene::Align::Center => (label.w - w) * 0.5,
                                crate::scene::Align::Right => label.w - w,
                            };
                            self.glyphs.push(GlyphGpu {
                                xywh: [label.x + off, label.y + (j as f32 * LINE_H + 0.4) * label.size, w, label.size * 0.5],
                                uv: self.text.white_uv,
                                color,
                                sdf: 0,
                                _pad: [0; 2],
                            });
                        }
                        continue;
                    }
                    let bucket = Text::bucket_for(px);
                    let ratio = label.size / crate::text::BUCKETS[bucket as usize];
                    let (lx, ly, size) = (label.x, label.y, label.size);
                    scratch.clear();
                    scratch.extend_from_slice(&self.text.layout(i, label).glyphs);
                    for &(g, gx, line) in &scratch {
                        match self.text.glyph(g, bucket) {
                            Err(()) => {
                                full = true;
                                break 'outer;
                            }
                            Ok(None) => {}
                            Ok(Some(ag)) => {
                                let ox = lx + gx * size + ag.min[0] * ratio;
                                let oy = ly + (line as f32 * LINE_H + baseline) * size + ag.min[1] * ratio;
                                self.glyphs.push(GlyphGpu {
                                    xywh: [ox, oy, ag.size[0] * ratio, ag.size[1] * ratio],
                                    uv: ag.uv,
                                    color,
                                    sdf: ag.sdf as u32,
                                    _pad: [0; 2],
                                });
                            }
                        }
                    }
                }
            }
            if full && attempt == 0 {
                self.text.clear_atlas();
                continue;
            }
            break;
        }
        self.ranges = ranges;

        if self.glyphs.len() > self.glyph_cap {
            self.glyph_cap = (self.glyphs.len() * 3 / 2).max(4096);
            self.glyph_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("glyphs"),
                size: (self.glyph_cap * std::mem::size_of::<GlyphGpu>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        if let (Some(buf), false) = (&self.glyph_buf, self.glyphs.is_empty()) {
            self.queue.write_buffer(buf, 0, bytemuck::cast_slice(&self.glyphs));
        }
        self.glyph_count = self.glyphs.len() as u32;
        self.upload_atlas();
        self.prepared = Some((region, self.cam.s));
        self.stats.text_ms = t0.elapsed().as_secs_f32() * 1000.0;
    }

    /// Renders one frame. Returns false if the surface was not ready.
    pub fn render(&mut self) -> bool {
        let t0 = Instant::now();
        let view = self.view_rect();
        if self.text_needs_rebuild(&view) {
            self.rebuild_text(&view);
        }
        self.write_camera();

        let Some(sf) = &self.surface else { return false };
        let frame = match sf.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                sf.configure(&self.device, &self.config);
                return false;
            }
            _ => return false,
        };
        let target = frame.texture.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.encode(&mut enc, &target);
        self.queue.submit([enc.finish()]);
        self.queue.present(frame);
        self.stats.frame_ms = t0.elapsed().as_secs_f32() * 1000.0;
        true
    }

    /// Renders the current view off-screen and returns tightly packed RGBA8.
    pub fn capture(&mut self) -> (u32, u32, Vec<u8>) {
        let view = self.view_rect();
        if self.text_needs_rebuild(&view) {
            self.rebuild_text(&view);
        }
        self.write_camera();
        let (w, h) = (self.config.width, self.config.height);
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("capture"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let row = (w * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.encode(&mut enc, &tex.create_view(&Default::default()));
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        self.wait_idle();
        let data = buf.slice(..).get_mapped_range().expect("map");
        let bgra = matches!(self.config.format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb);
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            let line = &data[(y * row) as usize..(y * row + w * 4) as usize];
            for px in line.chunks_exact(4) {
                if bgra {
                    out.extend_from_slice(&[px[2], px[1], px[0], 255]);
                } else {
                    out.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
            }
        }
        (w, h, out)
    }

    fn lod(&self) -> usize {
        // Far zoom: switch to a sparser sample once a typical shape is
        // well under a pixel (each sample then stands for 8^k shapes).
        let px = self.rect_median as f64 * self.cam.s;
        let mut lod = 0usize;
        let mut lim = 0.3;
        while lod + 1 < self.rect_lods.len() && px < lim {
            lod += 1;
            lim /= 2.83;
        }
        lod
    }

    fn write_camera(&self) {
        let (hx, lx) = split(self.cam.cx);
        let (hy, ly) = split(self.cam.cy);
        let cam = CamU {
            center_hi: [hx, hy],
            center_lo: [lx, ly],
            viewport: [self.config.width as f32, self.config.height as f32],
            scale: self.cam.s as f32,
            boost: 8f32.powi(self.lod() as i32),
        };
        self.queue.write_buffer(&self.cam_buf, 0, bytemuck::bytes_of(&cam));
        let (w, h) = (self.config.width as f32, self.config.height as f32);
        let ui = CamU { center_hi: [w * 0.5, h * 0.5], center_lo: [0.0, 0.0], viewport: [w, h], scale: 1.0, boost: 1.0 };
        self.queue.write_buffer(&self.ui_cam_buf, 0, bytemuck::bytes_of(&ui));
    }

    fn encode(&mut self, enc: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let view = self.view_rect();
        let lod = self.lod();
        let s = self.cam.s as f32;
        let mut draws = 0;
        let (mut nr, mut nl) = (0u32, 0u32);
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(self.bg), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.cam_bg, &[]);

            if let Some((idx, buf)) = self.rect_lods.get(lod) {
                idx.query(&view, 0.0, &mut self.ranges);
                pass.set_pipeline(&self.rect_pipe);
                pass.set_vertex_buffer(0, buf.slice(..));
                for &(a, b) in &self.ranges {
                    pass.draw(0..6, a..b);
                    nr += b - a;
                    draws += 1;
                }
            }
            if let Some(buf) = &self.line_buf {
                pass.set_pipeline(&self.line_pipe);
                pass.set_vertex_buffer(0, buf.slice(..));
                for (max_len, idx, base) in &self.line_classes {
                    if max_len * s < 0.5 {
                        continue;
                    }
                    idx.query(&view, 0.5 / s, &mut self.ranges);
                    for &(a, b) in &self.ranges {
                        pass.draw(0..6, a + base..b + base);
                        nl += b - a;
                        draws += 1;
                    }
                }
            }
            if let (Some(buf), true) = (&self.glyph_buf, self.glyph_count > 0) {
                pass.set_pipeline(&self.glyph_pipe);
                pass.set_bind_group(1, &self.atlas_bg, &[]);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..6, 0..self.glyph_count);
                draws += 1;
            }
            if let (Some(buf), true) = (&self.ov_rects.buf, self.ov_rects.count > 0) {
                pass.set_pipeline(&self.rect_pipe);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..6, 0..self.ov_rects.count);
            }
            if let (Some(buf), true) = (&self.ov_lines.buf, self.ov_lines.count > 0) {
                pass.set_pipeline(&self.line_pipe);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..6, 0..self.ov_lines.count);
            }
            if let (Some(buf), true) = (&self.ov_glyphs.buf, self.ov_glyphs.count > 0) {
                pass.set_pipeline(&self.glyph_pipe);
                pass.set_bind_group(1, &self.atlas_bg, &[]);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..6, 0..self.ov_glyphs.count);
            }
            pass.set_bind_group(0, &self.ui_cam_bg, &[]);
            if let (Some(buf), true) = (&self.ui_rects.buf, self.ui_rects.count > 0) {
                pass.set_pipeline(&self.rect_pipe);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..6, 0..self.ui_rects.count);
            }
            if let (Some(buf), true) = (&self.ui_lines.buf, self.ui_lines.count > 0) {
                pass.set_pipeline(&self.line_pipe);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..6, 0..self.ui_lines.count);
            }
            if let (Some(buf), true) = (&self.ui_glyphs.buf, self.ui_glyphs.count > 0) {
                pass.set_pipeline(&self.glyph_pipe);
                pass.set_bind_group(1, &self.atlas_bg, &[]);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..6, 0..self.ui_glyphs.count);
            }
        }
        self.stats.rects = nr;
        self.stats.lines = nl;
        self.stats.glyphs = self.glyph_count;
        self.stats.draws = draws;
    }

    /// Blocks until the GPU has finished all submitted work (for benchmarks).
    pub fn wait_idle(&self) {
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }
}
