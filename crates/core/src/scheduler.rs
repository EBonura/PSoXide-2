#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PsxInt {
    Sio = 0,
    Sio1 = 1,
    CdRom = 2,
    CdRead = 3,
    GpuDma = 4,
    MdecOutDma = 5,
    SpuDma = 6,
    GpuBusy = 7,
    MdecInDma = 8,
    GpuOtcDma = 9,
    CdRomDma = 10,
    SpuAsync = 11,
    CdRomDecodedBuf = 12,
    CdRomLid = 13,
    CdRomPlay = 14,
}

pub struct Scheduler {
    pub interrupt_flags: u32,
    pub int_targets: [u64; 16],
    pub lowest_target: u64,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            interrupt_flags: 0,
            int_targets: [u64::MAX; 16],
            lowest_target: u64::MAX,
        }
    }

    pub fn schedule(&mut self, irq: PsxInt, current_cycle: u64, delay: u32) {
        let idx = irq as usize;
        let target = current_cycle + delay as u64;
        self.interrupt_flags |= 1 << idx;
        self.int_targets[idx] = target;
        if target < self.lowest_target {
            self.lowest_target = target;
        }
    }

    pub fn cancel(&mut self, irq: PsxInt) {
        let idx = irq as usize;
        self.interrupt_flags &= !(1 << idx);
        self.int_targets[idx] = u64::MAX;
        self.recalc_lowest();
    }

    fn recalc_lowest(&mut self) {
        self.lowest_target = u64::MAX;
        let flags = self.interrupt_flags;
        for i in 0..16 {
            if flags & (1 << i) != 0 && self.int_targets[i] < self.lowest_target {
                self.lowest_target = self.int_targets[i];
            }
        }
    }

    /// Returns a bitmask of interrupts that fired this check
    pub fn check_interrupts(&mut self, cycle: u64) -> u32 {
        let flags = self.interrupt_flags;
        if flags == 0 {
            return 0;
        }

        let mut fired = 0u32;
        let mut new_lowest = u64::MAX;

        for i in 0..16u32 {
            let mask = 1 << i;
            if flags & mask == 0 {
                continue;
            }
            let target = self.int_targets[i as usize];
            if cycle >= target {
                self.interrupt_flags &= !mask;
                self.int_targets[i as usize] = u64::MAX;
                fired |= mask;
            } else if target < new_lowest {
                new_lowest = target;
            }
        }

        self.lowest_target = new_lowest;
        fired
    }
}
