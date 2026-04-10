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

    pub fn gp0_count(&self) -> u32 { self.command_processor.gp0_count }
    pub fn gp1_count(&self) -> u32 { self.command_processor.gp1_count }
    pub fn reset_frame_counters(&mut self) {
        self.command_processor.gp0_count = 0;
        self.command_processor.gp1_count = 0;
    }

    /// Count nonzero pixels in the display area as a cheap "anything drawn?" check
    pub fn nonzero_pixel_count(&self, x: u16, y: u16, w: u16, h: u16) -> u32 {
        let mut count = 0u32;
        for py in y..y + h {
            for px in x..x + w {
                if self.vram.get_pixel(px, py) != 0 { count += 1; }
            }
        }
        count
    }
}
