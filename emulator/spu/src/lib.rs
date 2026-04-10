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
                // SPUSTAT: mirror SPUCNT bits [5:0], plus idle flags
                // Bit 10: Data Transfer Busy (0 = idle — what BIOS waits for)
                // Return: lower 6 bits from SPUCNT, transfer not busy
                let cnt = self.regs[SPUCNT];
                (cnt & 0x3F) // transfer idle (bit 10 = 0)
            }
            _ => self.regs[idx],
        }
    }

    pub fn write16(&mut self, offset: u32, value: u16) {
        let idx = (offset as usize / 2) & 0x1FF;
        self.regs[idx] = value;

        match offset {
            0x1AA => {
                // SPUCNT — update SPUSTAT to mirror
                // Nothing special needed; SPUSTAT read computes dynamically
            }
            0x1AC => {
                // Transfer Control — BIOS writes here then polls SPUSTAT
                // We treat transfers as instant (no busy flag)
            }
            _ => {}
        }
    }
}
