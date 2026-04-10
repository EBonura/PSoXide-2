mod config;

use anyhow::Result;
use clap::Parser;
use config::Config;
use core::bus::Bus;
use core::cpu::Cpu;
use std::path::PathBuf;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

#[derive(Parser)]
#[command(name = "psoxide", about = "PSoXide-2 — PS1 Emulator")]
struct Args {
    /// Path to PS1 BIOS file (512KB). Overrides config.
    #[arg(short, long)]
    bios: Option<PathBuf>,

    /// Path to game disc image (.cue file). Overrides config.
    #[arg(short, long)]
    game: Option<PathBuf>,
}

const DISPLAY_WIDTH: u32 = 1024;
const DISPLAY_HEIGHT: u32 = 512;
const CYCLES_PER_FRAME: u64 = 33_868_800 / 60; // NTSC ~564480

struct App {
    window: Option<Arc<Window>>,
    wgpu_state: Option<WgpuState>,
    cpu: Cpu,
    bus: Bus,
    frame_count: u64,
}

struct WgpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    vram_texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
}

impl App {
    fn new(bios_path: &PathBuf) -> Result<Self> {
        let mut bus = Bus::new();
        bus.load_bios(bios_path)?;
        let mut cpu = Cpu::new();
        cpu.reset();
        Ok(Self {
            window: None,
            wgpu_state: None,
            cpu,
            bus,
            frame_count: 0,
        })
    }

    fn run_frame(&mut self) {
        let target = self.cpu.regs.cycle + CYCLES_PER_FRAME;
        while self.cpu.regs.cycle < target {
            self.cpu.step(&mut self.bus);
        }
        self.frame_count += 1;
    }

    fn render(&mut self) {
        let Some(state) = &self.wgpu_state else { return };

        // Upload VRAM to texture
        let rgba = self.bus.gpu.vram.to_rgba8(0, 0, 1024, 512);
        state.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &state.vram_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(1024 * 4),
                rows_per_image: Some(512),
            },
            wgpu::Extent3d {
                width: 1024,
                height: 512,
                depth_or_array_layers: 1,
            },
        );

        let output = match state.surface.get_current_texture() {
            Ok(t) => t,
            Err(_) => return,
        };
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = state.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
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
                ..Default::default()
            });
            pass.set_pipeline(&state.pipeline);
            pass.set_bind_group(0, &state.bind_group, &[]);
            pass.draw(0..6, 0..1); // fullscreen quad
        }

        state.queue.submit(std::iter::once(encoder.finish()));
        output.present();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() { return; }

        let attrs = Window::default_attributes()
            .with_title("PSoXide-2")
            .with_inner_size(LogicalSize::new(DISPLAY_WIDTH, DISPLAY_HEIGHT));
        let window = Arc::new(event_loop.create_window(attrs).unwrap());
        self.window = Some(window.clone());

        // Init wgpu
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone()).unwrap();

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        })).unwrap();

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor::default(), None,
        )).unwrap();

        let size = window.inner_size();
        let config = surface.get_default_config(&adapter, size.width.max(1), size.height.max(1)).unwrap();
        surface.configure(&device, &config);

        // VRAM texture
        let vram_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("VRAM"),
            size: wgpu::Extent3d { width: 1024, height: 512, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let vram_view = vram_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // Shader
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(include_str!("fullscreen.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&vram_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        self.wgpu_state = Some(WgpuState {
            surface,
            device,
            queue,
            config,
            vram_texture,
            bind_group,
            pipeline,
        });

        event_loop.set_control_flow(ControlFlow::Poll);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(state) = &mut self.wgpu_state {
                    state.config.width = size.width.max(1);
                    state.config.height = size.height.max(1);
                    state.surface.configure(&state.device, &state.config);
                }
            }
            WindowEvent::RedrawRequested => {
                self.run_frame();
                self.render();

                if self.frame_count % 60 == 0 {
                    let nonzero = self.bus.gpu.vram.data.iter().filter(|&&p| p != 0).count();
                    eprintln!("frame {:4} | PC={:08X} | VRAM={} | Status={:08X} ISTAT={:08X} IMASK={:08X}",
                        self.frame_count, self.cpu.regs.pc, nonzero,
                        self.cpu.regs.cp0[12], self.bus.read_istat(), self.bus.read_imask());
                    if let Some(w) = &self.window {
                        w.set_title(&format!(
                            "PSoXide-2 | frame {} | PC={:08X} | VRAM pixels: {}",
                            self.frame_count, self.cpu.regs.pc, nonzero
                        ));
                    }
                }

                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let args = Args::parse();
    let cfg = Config::load().unwrap_or_default();

    // CLI args override config file
    let bios = args.bios.or(cfg.bios)
        .ok_or_else(|| anyhow::anyhow!(
            "No BIOS path. Pass --bios or set it in config.toml"
        ))?;
    let _game = args.game.or(cfg.game);

    tracing::info!("PSoXide-2 starting");

    let mut app = App::new(&bios)?;
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut app)?;

    Ok(())
}
