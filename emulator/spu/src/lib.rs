/// SPU stub — stores all register writes and returns them on read.
/// The BIOS polls SPUSTAT and transfer control during init.
pub struct Spu {
    pub ram: Box<[u8; 0x8_0000]>,    // 512KB SPU RAM
    pub regs: Box<[u16; 0x200]>,     // 512 x 16-bit registers (0x000..0x3FF)
}

// Key register offsets (from SPU base 0x1F801C00)
const SPUCNT: usize = 0x1AA / 2;     // SPU Control
const SPUSTAT: usize = 0x1AE / 2;    // SPU Status
const TRANSFER_CTRL: usize = 0x1AC / 2; // Transfer Control

impl Spu {
    pub fn new() -> Self {
        Self {
            ram: vec![0u8; 0x8_0000].into_boxed_slice().try_into().unwrap(),
            regs: vec![0u16; 0x200].into_boxed_slice().try_into().unwrap(),
        }
    }

    pub fn read16(&self, offset: u32) -> u16 {
        let idx = (offset as usize / 2) & 0x1FF;
        match offset {
            0x1AE => {
                // SPUSTAT — return 0 (fully idle).
                // Redux's full SPU would process transfers and reflect real state.
                // Our stub treats everything as instantly complete.
                0
            }
            _ => self.regs[idx],
        }
    }

    pub fn write16(&mut self, offset: u32, value: u16) {
        let idx = (offset as usize / 2) & 0x1FF;
        self.regs[idx] = value;

        match offset {
            0x1AA => {
                // SPUCNT write — auto-clear transfer mode bits (5-4) to simulate
                // instant transfer completion. On real hardware / Redux, the SPU
                // processes the transfer and clears these. Without this, SPUSTAT
                // mirrors the transfer mode forever and the BIOS times out polling.
                self.regs[SPUCNT] = value & !0x30;
            }
            _ => {}
        }
    }
}
