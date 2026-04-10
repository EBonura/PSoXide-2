use crate::scheduler::Scheduler;

const CPU_CLOCK: u64 = 33_868_800;
const NTSC_FPS: u64 = 60;
const NTSC_SCANLINES: u64 = 263;
const NTSC_VBLANK_START: u64 = 243;
const CYCLES_PER_SCANLINE: u64 = CPU_CLOCK / (NTSC_FPS * NTSC_SCANLINES); // ~2147

pub struct Timers {
    pub counters: [Counter; 3],
    // Virtual counter 3: VBlank tracking
    pub hsync_count: u64,
    pub next_hsync: u64,
    pub next_vblank: u64,
    pub in_vblank: bool,
    pub frame_count: u64,
}

#[derive(Clone)]
pub struct Counter {
    pub value: u16,
    pub target: u16,
    pub mode: u16,
    pub cycle_start: u64,
    pub paused: bool,
}

impl Counter {
    fn new() -> Self {
        Self {
            value: 0,
            target: 0,
            mode: 0,
            cycle_start: 0,
            paused: false,
        }
    }
}

impl Timers {
    pub fn new() -> Self {
        Self {
            counters: [Counter::new(), Counter::new(), Counter::new()],
            hsync_count: 0,
            next_hsync: CYCLES_PER_SCANLINE,
            next_vblank: NTSC_VBLANK_START * CYCLES_PER_SCANLINE,
            in_vblank: false,
            frame_count: 0,
        }
    }

    pub fn update(&mut self, cycle: u64, scheduler: &mut Scheduler) -> bool {
        let mut vblank_fired = false;

        // Check HSync (scanline boundary)
        while cycle >= self.next_hsync {
            self.hsync_count += 1;
            self.next_hsync += CYCLES_PER_SCANLINE;

            // Update counter 1 if in HSync clock mode
            if self.counters[1].mode & 0x100 != 0 && !self.counters[1].paused {
                self.counters[1].value = self.counters[1].value.wrapping_add(1);
                self.check_counter_irq(1, scheduler, cycle);
            }
        }

        // Check VBlank
        if cycle >= self.next_vblank {
            if !self.in_vblank {
                self.in_vblank = true;
                vblank_fired = true;
                self.next_vblank = (self.frame_count + 1) * NTSC_SCANLINES * CYCLES_PER_SCANLINE;
            } else {
                // End of VBlank, start new frame
                self.in_vblank = false;
                self.frame_count += 1;
                self.hsync_count = 0;
                self.next_vblank = self.frame_count * NTSC_SCANLINES * CYCLES_PER_SCANLINE
                    + NTSC_VBLANK_START * CYCLES_PER_SCANLINE;
            }
        }

        // Update counters 0 and 2 based on system clock
        for i in [0usize, 2] {
            if self.counters[i].paused {
                continue;
            }
            let rate = self.counter_rate(i);
            if rate == 0 {
                continue;
            }
            let elapsed = cycle.saturating_sub(self.counters[i].cycle_start);
            let ticks = (elapsed / rate) as u16;
            if ticks > 0 {
                self.counters[i].value = self.counters[i].value.wrapping_add(ticks);
                self.counters[i].cycle_start = cycle;
                self.check_counter_irq(i, scheduler, cycle);
            }
        }

        vblank_fired
    }

    fn counter_rate(&self, index: usize) -> u64 {
        match index {
            0 => {
                if self.counters[0].mode & 0x100 != 0 {
                    11 // Pixel clock (~3.07MHz, ~11 CPU clocks per pixel)
                } else {
                    1 // System clock
                }
            }
            2 => {
                if self.counters[2].mode & 0x200 != 0 {
                    8 // 1/8 system clock
                } else {
                    1 // System clock
                }
            }
            _ => 1,
        }
    }

    fn check_counter_irq(&mut self, index: usize, _scheduler: &mut Scheduler, _cycle: u64) {
        let counter = &mut self.counters[index];
        let mode = counter.mode;

        // Check target hit
        if mode & 0x10 != 0 && counter.value >= counter.target {
            // IRQ on target
            counter.mode |= 0x0800; // Set target reached flag
            if mode & 0x08 != 0 {
                counter.value = 0; // Reset to 0 on target
            }
        }

        // Check overflow
        if mode & 0x20 != 0 && counter.value == 0xFFFF {
            counter.mode |= 0x1000; // Set overflow flag
        }
    }

    pub fn read_counter(&self, index: usize) -> u16 {
        self.counters[index].value
    }

    pub fn read_mode(&mut self, index: usize) -> u16 {
        let mode = self.counters[index].mode;
        // Reading mode resets bits 11 and 12
        self.counters[index].mode &= !0x1800;
        mode
    }

    pub fn read_target(&self, index: usize) -> u16 {
        self.counters[index].target
    }

    pub fn write_counter(&mut self, index: usize, value: u16, cycle: u64) {
        self.counters[index].value = value;
        self.counters[index].cycle_start = cycle;
    }

    pub fn write_mode(&mut self, index: usize, value: u16, cycle: u64) {
        self.counters[index].mode = value;
        self.counters[index].value = 0;
        self.counters[index].cycle_start = cycle;
    }

    pub fn write_target(&mut self, index: usize, value: u16) {
        self.counters[index].target = value;
    }

    pub fn vblank_irq_pending(&self) -> bool {
        self.in_vblank
    }
}
