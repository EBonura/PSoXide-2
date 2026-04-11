/// GPUSTAT register (read from 0x1F801814)
pub struct GpuStatus {
    pub raw: u32,
}

impl GpuStatus {
    pub fn new() -> Self {
        Self {
            // Initial state: ready for commands, DMA off, display disabled
            // Bit 26: Ready to receive command word
            // Bit 27: Ready to send VRAM to CPU
            // Bit 28: Ready to receive DMA block
            raw: 0x1480_2000,
        }
    }

    pub fn read(&self) -> u32 {
        self.raw
    }

    pub fn set_bit(&mut self, bit: u32, val: bool) {
        if val {
            self.raw |= 1 << bit;
        } else {
            self.raw &= !(1 << bit);
        }
    }

    pub fn set_dma_direction(&mut self, dir: u32) {
        self.raw = (self.raw & !0x6000_0000) | ((dir & 3) << 29);
    }

    pub fn set_display_disabled(&mut self, disabled: bool) {
        self.set_bit(23, disabled);
    }

    pub fn set_interlace_field(&mut self, odd: bool) {
        self.set_bit(31, odd);
    }

    /// Toggle GPUSTAT bit 31 (interlace/field flag).
    /// Matching pcsx-redux SoftGPU::vblank(): `m_statusRet ^= 0x80000000`.
    /// Called once per VBlank. The retail BIOS shell's waitVSync polls this
    /// bit and waits for it to toggle between frames.
    pub fn toggle_interlace_field(&mut self) {
        self.raw ^= 0x8000_0000;
    }
}
