use std::sync::mpsc::Receiver;
use std::sync::Arc;

use anyhow::{Context, Result};
use capture::Frame;
use wgpu::util::DeviceExt;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowAttributes},
};

// ─── Shader ──────────────────────────────────────────────────────────────────

const SHADER: &str = r#"
@group(0) @binding(0) var frame_tex: texture_2d<f32>;
@group(0) @binding(1) var frame_smp: sampler;

struct Vert {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> Vert {
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    var out: Vert;
    out.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv  = vec2<f32>(x, y);
    return out;
}

@fragment
fn fs_main(in: Vert) -> @location(0) vec4<f32> {
    return textureSample(frame_tex, frame_smp, in.uv);
}
"#;

// ─── GpuState ────────────────────────────────────────────────────────────────

struct GpuState {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    tex_width: u32,
    tex_height: u32,
}

impl GpuState {
    fn new(window: Arc<Window>, frame: &Frame) -> Result<Self> {
        let desc = wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        };
        let instance = wgpu::Instance::new(desc);

        // instance.request_adapter(options);

        let surface = instance
            .create_surface(window.clone())
            .context("create_surface")?;

        let options = wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
        };

        let adapter = pollster::block_on(instance.request_adapter(&options))
        .context("nenhum adapter wgpu compativel")?;

        let desc = &wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: Default::default(),
        };

        let (device, queue) = pollster::block_on(adapter.request_device(
            desc,
            None,
        ))
        .context("request_device")?;

        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| *f == wgpu::TextureFormat::Bgra8Unorm)
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        surface.configure(&device, &config);

        let shader_module_descriptor = wgpu::ShaderModuleDescriptor {
            label: Some("fullscreen"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        };
        let shader = device.create_shader_module(shader_module_descriptor);

        let bind_group_layout_descriptor= wgpu::BindGroupLayoutDescriptor {
            label: Some("frame_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
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
        };

        let bind_group_layout = device.create_bind_group_layout(&bind_group_layout_descriptor   );

        let pipeline_layout_descriptor = wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        };

        let layout = device.create_pipeline_layout(&pipeline_layout_descriptor);

        let render_pipeline_descriptor = wgpu::RenderPipelineDescriptor {
            label: Some("fullscreen_pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        };

        let pipeline = device.create_render_pipeline(&render_pipeline_descriptor);

        let sample_descriptor: wgpu::SamplerDescriptor<'_> = wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        };

        let sampler = device.create_sampler(&sample_descriptor);

        let (texture, bind_group) =
            Self::make_texture_and_bg(&device, &queue, &bind_group_layout, &sampler, frame);

        Ok(Self {
            device,
            queue,
            surface,
            config,
            pipeline,
            bind_group_layout,
            sampler,
            texture,
            bind_group,
            tex_width: frame.width,
            tex_height: frame.height,
        })
    }

    fn make_texture_and_bg(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bgl: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        frame: &Frame,
    ) -> (wgpu::Texture, wgpu::BindGroup) {
        let texture = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("frame_tex"),
                size: wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &frame.data,
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame_bg"),
            layout: bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });

        (texture, bind_group)
    }

    fn update_frame(&mut self, frame: &Frame) {
        if frame.width != self.tex_width || frame.height != self.tex_height {
            let (tex, bg) = Self::make_texture_and_bg(
                &self.device,
                &self.queue,
                &self.bind_group_layout,
                &self.sampler,
                frame,
            );
            self.texture = tex;
            self.bind_group = bg;
            self.tex_width = frame.width;
            self.tex_height = frame.height;
        } else {
            self.queue.write_texture(
                self.texture.as_image_copy(),
                &frame.data,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(frame.width * 4),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    fn render(&mut self) -> Result<()> {
        let output = self
            .surface
            .get_current_texture()
            .context("get_current_texture")?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1); // fullscreen triangle
        }

        self.queue.submit(std::iter::once(enc.finish()));
        output.present();
        Ok(())
    }

    fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
    }
}

// ─── Renderer (ApplicationHandler) ───────────────────────────────────────────

pub struct Renderer {
    frame_rx: Receiver<Frame>,
    pending: Option<Frame>,
    gpu: Option<GpuState>,
    window: Option<Arc<Window>>,
}

impl Renderer {
    pub fn new(frame_rx: Receiver<Frame>) -> Self {
        Self {
            frame_rx,
            pending: None,
            gpu: None,
            window: None,
        }
    }

    /// Inicia o event loop — bloqueia ate a janela fechar. Deve rodar na thread principal.
    pub fn run(mut self) -> Result<()> {
        let event_loop = EventLoop::new().context("EventLoop::new()")?;
        event_loop.set_control_flow(ControlFlow::Poll);
        event_loop
            .run_app(&mut self)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }
}

impl ApplicationHandler for Renderer {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // No macOS, janela so pode ser criada dentro de resumed()
        let attrs = WindowAttributes::default()
            .with_title("screenshare — fase 1")
            .with_inner_size(winit::dpi::LogicalSize::new(1280u32, 720u32));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("create_window falhou: {e}");
                event_loop.exit();
                return;
            }
        };
        self.window = Some(window.clone());

        // Espera o primeiro frame para inicializar o GpuState com as dimensoes corretas
        let first_frame = loop {
            if let Ok(f) = self.frame_rx.try_recv() {
                break f;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };

        match GpuState::new(window, &first_frame) {
            Ok(gpu) => {
                self.gpu = Some(gpu);
                self.pending = Some(first_frame);
            }
            Err(e) => {
                eprintln!("GpuState::new falhou: {e}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
            }

            WindowEvent::RedrawRequested => {
                if let Some(gpu) = &mut self.gpu {
                    if let Some(frame) = self.pending.take() {
                        gpu.update_frame(&frame);
                    }
                    if let Err(e) = gpu.render() {
                        eprintln!("render: {e}");
                    }
                }
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Drena o channel; fica so com o frame mais recente
        while let Ok(frame) = self.frame_rx.try_recv() {
            self.pending = Some(frame);
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}
