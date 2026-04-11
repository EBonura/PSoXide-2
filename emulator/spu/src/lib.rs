/// SPU register stub — handles SPUCNT/SPUSTAT mirroring per pcsx-redux.
/// The BIOS polls SPUSTAT during init expecting bits 0-5 to mirror SPUCNT.
pub struct Spu {
    pub ram: Box<[u8; 0x8_0000]>,    // 512KB SPU RAM
    pub regs: Box<[u16; 0x200]>,     // 512 x 16-bit registers (0x000..0x3FF)
    spu_ctrl: u16,                   // SPUCNT (0x1AA) — stored separately for SPUSTAT derivation
    spu_stat: u16,                   // SPUSTAT upper bits (IRQ flag, DMA status, etc.)
    spu_addr: u32,                   // Current SPU RAM transfer address (in bytes)
}

// Key register offsets (from SPU base 0x1F801C00), as byte offsets
const OFF_SPU_ADDR: u32  = 0x1A6;    // Data Transfer Address
const OFF_SPU_DATA: u32  = 0x1A8;    // Data Transfer FIFO
const OFF_SPUCNT: u32    = 0x1AA;    // SPU Control
const _OFF_TRANSFER: u32 = 0x1AC;    // Transfer Control (not used yet)
const OFF_SPUSTAT: u32   = 0x1AE;    // SPU Status

impl Spu {
    pub fn new() -> Self {
        Self {
            ram: vec![0u8; 0x8_0000].into_boxed_slice().try_into().unwrap(),
            regs: vec![0u16; 0x200].into_boxed_slice().try_into().unwrap(),
            spu_ctrl: 0,
            spu_stat: 0,
            spu_addr: 0,
        }
    }

    pub fn read16(&mut self, offset: u32) -> u16 {
        let idx = (offset as usize / 2) & 0x1FF;
        match offset {
            OFF_SPUCNT => self.spu_ctrl,
            OFF_SPUSTAT => {
                // pcsx-redux: (spuStat & ~0x3F) | (spuCtrl & 0x3F)
                // Bits 0-5 mirror SPUCNT mode bits; bits 6+ from internal state.
                (self.spu_stat & !0x3F) | (self.spu_ctrl & 0x3F)
            }
            OFF_SPU_ADDR => (self.spu_addr >> 3) as u16,
            OFF_SPU_DATA => {
                // pcsx-redux: read from spuMem[spuAddr>>1], then advance address
                let addr = self.spu_addr as usize;
                let val = if addr + 1 < self.ram.len() {
                    u16::from_le_bytes([self.ram[addr], self.ram[addr + 1]])
                } else {
                    0
                };
                self.spu_addr = (self.spu_addr + 2) & 0x7_FFFF;
                val
            }
            _ => self.regs[idx],
        }
    }

    /// DMA write: Main RAM → SPU RAM.  Matches pcsx-redux writeDMAMem.
    /// `data` is a slice of 16-bit words; spu_addr advances and wraps.
    pub fn dma_write(&mut self, data: &[u16]) {
        for &word in data {
            let addr = (self.spu_addr as usize) & 0x7_FFFE;
            let bytes = word.to_le_bytes();
            self.ram[addr] = bytes[0];
            self.ram[addr + 1] = bytes[1];
            self.spu_addr = (self.spu_addr + 2) & 0x7_FFFF;
        }
    }

    /// DMA read: SPU RAM → Main RAM.  Matches pcsx-redux readDMAMem.
    pub fn dma_read(&mut self, buf: &mut [u16]) {
        for slot in buf.iter_mut() {
            let addr = (self.spu_addr as usize) & 0x7_FFFE;
            *slot = u16::from_le_bytes([self.ram[addr], self.ram[addr + 1]]);
            self.spu_addr = (self.spu_addr + 2) & 0x7_FFFF;
        }
    }

    pub fn write16(&mut self, offset: u32, value: u16) {
        let idx = (offset as usize / 2) & 0x1FF;

        match offset {
            OFF_SPUCNT => {
                // pcsx-redux: spuCtrl = val (stores as-is, no bit clearing)
                self.spu_ctrl = value;
            }
            OFF_SPUSTAT => {
                // Read-only on real HW. pcsx-redux: spuStat = val & 0xF800
                self.spu_stat = value & 0xF800;
            }
            OFF_SPU_ADDR => {
                self.spu_addr = (value as u32) << 3;
            }
            OFF_SPU_DATA => {
                let addr = self.spu_addr as usize;
                if addr + 1 < self.ram.len() {
                    let bytes = value.to_le_bytes();
                    self.ram[addr] = bytes[0];
                    self.ram[addr + 1] = bytes[1];
                }
                self.spu_addr = (self.spu_addr + 2) & 0x7_FFFF;
            }
            _ => {
                self.regs[idx] = value;
            }
        }

        // Also mirror to regs array for generic readback of other registers
        if offset != OFF_SPUCNT && offset != OFF_SPUSTAT {
            self.regs[idx] = value;
        }
    }
}
