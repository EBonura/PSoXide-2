pub mod commands;
pub mod command_buffer;
pub mod display;
pub mod rasterizer;
pub mod status;
pub mod vram;

use commands::CommandProcessor;
use display::DisplayConfig;
use status::GpuStatus;
use vram::Vram;

pub struct Gpu {
    pub vram: Vram,
    pub status: GpuStatus,
    pub display: DisplayConfig,
    pub command_processor: CommandProcessor,
}

impl Gpu {
    pub fn new() -> Self {
        Self {
            vram: Vram::new(),
            status: GpuStatus::new(),
            display: DisplayConfig::new(),
            command_processor: CommandProcessor::new(),
        }
    }

    pub fn reset(&mut self) {
        self.vram.clear();
        self.status = GpuStatus::new();
        self.display = DisplayConfig::new();
        self.command_processor = CommandProcessor::new();
    }

    pub fn gp0_write(&mut self, data: u32) {
        self.command_processor.gp0_write(&mut self.vram, &mut self.status, &mut self.display, data);
    }

    pub fn gp1_write(&mut self, data: u32) {
        self.command_processor.gp1_write(&mut self.vram, &mut self.status, &mut self.display, data);
    }

    pub fn read_data(&mut self) -> u32 {
        self.command_processor.read_data(&mut self.vram)
    }

    pub fn read_status(&self) -> u32 {
        self.status.read()
    }
}
