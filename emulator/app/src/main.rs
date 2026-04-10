mod config;

use anyhow::Result;
use clap::Parser;
use config::Config;
use core::bus::Bus;
use core::cpu::Cpu;
use core::cpu::registers::GPR_NAMES;
use std::collections::VecDeque;
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
    #[arg(short, long)]
    bios: Option<PathBuf>,
    #[arg(short, long)]
    game: Option<PathBuf>,
}

const DISPLAY_WIDTH: u32 = 1280;
const DISPLAY_HEIGHT: u32 = 720;
const CYCLES_PER_FRAME: u64 = 33_868_800 / 60;
const EXEC_HISTORY_SIZE: usize = 64;

struct ExecEntry {
    pc: u32,
    code: u32,
}

struct App {
    window: Option<Arc<Window>>,
    wgpu_state: Option<WgpuState>,
    egui_state: Option<EguiState>,
    cpu: Cpu,
    bus: Bus,
    frame_count: u64,
    exec_history: VecDeque<ExecEntry>,
    paused: bool,
    step_one: bool,
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

struct EguiState {
    ctx: egui::Context,
    winit_state: egui_winit::State,
    renderer: egui_wgpu::Renderer,
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
            egui_state: None,
            cpu,
            bus,
            frame_count: 0,
            exec_history: VecDeque::with_capacity(EXEC_HISTORY_SIZE),
            paused: false,
            step_one: false,
        })
    }

    fn run_frame(&mut self) {
        if self.paused && !self.step_one {
            return;
        }
        self.step_one = false;
        self.bus.gpu.reset_frame_counters();

        let target = self.cpu.regs.cycle + CYCLES_PER_FRAME;
        while self.cpu.regs.cycle < target {
            let pc = self.cpu.regs.pc;
            let code = self.bus.read32(pc);
            self.cpu.step(&mut self.bus);

            if self.exec_history.len() >= EXEC_HISTORY_SIZE {
                self.exec_history.pop_front();
            }
            self.exec_history.push_back(ExecEntry { pc, code });
        }
        self.frame_count += 1;

        // One-shot diagnostic at frame 60
        if self.frame_count == 60 {
            let nz = self.bus.gpu.nonzero_pixel_count(0, 0, 1024, 512);
            eprintln!("FRAME60: GP0={} GP1={} GPUSTAT={:08X} VRAM_nz={} PC={:08X} Status={:08X}",
                self.bus.gpu.gp0_count(), self.bus.gpu.gp1_count(),
                self.bus.gpu.read_status(), nz,
                self.cpu.regs.pc, self.cpu.regs.cp0[12]);
        }
    }

    fn draw_debug_ui(&mut self) {
        let Some(egui_state) = &self.egui_state else { return };
        let ctx = &egui_state.ctx;

        egui::SidePanel::left("debug_panel").default_width(340.0).show(ctx, |ui| {
            // Registers
            ui.heading("Registers");
            egui::Grid::new("regs").num_columns(4).spacing([8.0, 2.0]).show(ui, |ui| {
                for i in (0..32).step_by(2) {
                    ui.monospace(format!("{:4}={:08X}", GPR_NAMES[i], self.cpu.regs.gpr[i]));
                    ui.monospace(format!("{:4}={:08X}", GPR_NAMES[i+1], self.cpu.regs.gpr[i+1]));
                    ui.end_row();
                }
                ui.monospace(format!("  PC={:08X}", self.cpu.regs.pc));
                ui.monospace(format!("  HI={:08X}", self.cpu.regs.hi));
                ui.end_row();
                ui.monospace(format!("  LO={:08X}", self.cpu.regs.lo));
                ui.monospace(format!(" cyc={}", self.cpu.regs.cycle));
                ui.end_row();
            });

            ui.separator();
            ui.heading("COP0");
            egui::Grid::new("cop0").num_columns(2).spacing([8.0, 2.0]).show(ui, |ui| {
                ui.monospace(format!("Status={:08X}", self.cpu.regs.cp0[12]));
                ui.monospace(format!(" Cause={:08X}", self.cpu.regs.cp0[13]));
                ui.end_row();
                ui.monospace(format!("   EPC={:08X}", self.cpu.regs.cp0[14]));
                ui.monospace(format!("BadVAd={:08X}", self.cpu.regs.cp0[8]));
                ui.end_row();
            });

            ui.separator();
            ui.heading("IRQ");
            ui.monospace(format!("ISTAT={:08X}  IMASK={:08X}", self.bus.read_istat(), self.bus.read_imask()));

            ui.separator();
            ui.heading("GPU");
            ui.monospace(format!("GPUSTAT={:08X}", self.bus.gpu.read_status()));
            ui.monospace(format!("Display: {}x{} at ({},{})",
                self.bus.gpu.display.width(), self.bus.gpu.display.height(),
                self.bus.gpu.display.display_area_x, self.bus.gpu.display.display_area_y));
            let da = &self.bus.gpu.command_processor;
            ui.monospace(format!("DrawArea: ({},{})..({},{})", da.draw_area_left, da.draw_area_top, da.draw_area_right, da.draw_area_bottom));
            ui.monospace(format!("DrawOff: ({},{})", da.draw_offset_x, da.draw_offset_y));
            ui.monospace(format!("GP0={} GP1={}", self.bus.gpu.gp0_count(), self.bus.gpu.gp1_count()));
            let nz = self.bus.gpu.nonzero_pixel_count(
                self.bus.gpu.display.display_area_x,
                self.bus.gpu.display.display_area_y,
                self.bus.gpu.display.width().min(320),
                self.bus.gpu.display.height().min(240),
            );
            ui.monospace(format!("VRAM nonzero: {}", nz));

            ui.separator();
            // Controls
            ui.horizontal(|ui| {
                if ui.button(if self.paused { "Play" } else { "Pause" }).clicked() {
                    self.paused = !self.paused;
                }
                if ui.button("Step").clicked() {
                    self.paused = true;
                    self.step_one = true;
                }
            });
            ui.label(format!("Frame: {}", self.frame_count));

            ui.separator();
            ui.heading("Execution History");
            egui::ScrollArea::vertical().max_height(300.0).stick_to_bottom(true).show(ui, |ui| {
                for entry in &self.exec_history {
                    ui.monospace(format!("{:08X}: {:08X}", entry.pc, entry.code));
                }
            });
        });
    }

    fn render(&mut self) {
        if self.wgpu_state.is_none() || self.egui_state.is_none() { return; }
        let window = self.window.as_ref().unwrap().clone();

        // Begin egui frame (needs &mut self for draw_debug_ui)
        let raw_input = self.egui_state.as_mut().unwrap().winit_state.take_egui_input(&window);
        self.egui_state.as_mut().unwrap().ctx.begin_pass(raw_input);
        self.draw_debug_ui();
        let full_output = self.egui_state.as_mut().unwrap().ctx.end_pass();
        self.egui_state.as_mut().unwrap().winit_state.handle_platform_output(&window, full_output.platform_output);
        let ppp = self.egui_state.as_ref().unwrap().ctx.pixels_per_point();
        let paint_jobs = self.egui_state.as_ref().unwrap().ctx.tessellate(full_output.shapes, ppp);

        // Now borrow wgpu_state immutably
        let state = self.wgpu_state.as_ref().unwrap();

        // Upload full VRAM (1024x512) — shows everything including textures/framebuffers
        let rgba = self.bus.gpu.vram.to_rgba8(0, 0, 1024, 512);
        state.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &state.vram_texture, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
            },
            &rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(1024 * 4), rows_per_image: Some(512) },
            wgpu::Extent3d { width: 1024, height: 512, depth_or_array_layers: 1 },
        );

        let output = match state.surface.get_current_texture() {
            Ok(t) => t,
            Err(_) => return,
        };
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = state.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());

        // VRAM quad pass
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view, resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None, ..Default::default()
            });
            pass.set_pipeline(&state.pipeline);
            pass.set_bind_group(0, &state.bind_group, &[]);
            pass.draw(0..6, 0..1);
        }

        // egui pass — single borrow of egui_state to satisfy wgpu 24 lifetimes
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [state.config.width, state.config.height],
            pixels_per_point: window.scale_factor() as f32,
        };
        {
            let egui_state = self.egui_state.as_mut().unwrap();
            for (id, delta) in &full_output.textures_delta.set {
                egui_state.renderer.update_texture(&state.device, &state.queue, *id, delta);
            }
            egui_state.renderer.update_buffers(&state.device, &state.queue, &mut encoder, &paint_jobs, &screen_descriptor);
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view, resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None, ..Default::default()
                }).forget_lifetime();
                egui_state.renderer.render(&mut pass, &paint_jobs, &screen_descriptor);
            }
            for id in &full_output.textures_delta.free {
                egui_state.renderer.free_texture(id);
            }
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

        let vram_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("VRAM"),
            size: wgpu::Extent3d { width: 1024, height: 512, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1,
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

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(include_str!("fullscreen.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&vram_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None, bind_group_layouts: &[&bind_group_layout], push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None, layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_main"), buffers: &[], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState { format: config.format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None, multisample: wgpu::MultisampleState::default(),
            multiview: None, cache: None,
        });

        // egui
        let ctx = egui::Context::default();
        let winit_state = egui_winit::State::new(ctx.clone(), ctx.viewport_id(), &window, None, None, None);
        let renderer = egui_wgpu::Renderer::new(&device, config.format, None, 1, false);

        self.wgpu_state = Some(WgpuState { surface, device, queue, config, vram_texture, bind_group, pipeline });
        self.egui_state = Some(EguiState { ctx, winit_state, renderer });
        event_loop.set_control_flow(ControlFlow::Poll);
        window.request_redraw();
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Let egui handle events first
        if let Some(egui_state) = &mut self.egui_state {
            let _ = egui_state.winit_state.on_window_event(self.window.as_ref().unwrap(), &event);
        }

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
    let bios = args.bios.or(cfg.bios)
        .ok_or_else(|| anyhow::anyhow!("No BIOS path. Pass --bios or set it in config.toml"))?;
    let _game = args.game.or(cfg.game);

    let mut app = App::new(&bios)?;
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut app)?;
    Ok(())
}
