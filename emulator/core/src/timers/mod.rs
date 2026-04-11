/// Root counters — matching PCSX-Redux psxcounters.cc architecture.
///
/// 4 counters total:
///   0-2: hardware root counters (mapped to 0x1F801100-0x1F801128)
///   3:   virtual "base" counter driving hsync/vblank timing
///
/// Counter values are computed on-read from (cycle - cycleStart) / rate.
/// Each counter has two states: CountToTarget and CountToOverflow.
/// IRQs fire via pending_irqs bitmask (caller drains into ISTAT).

const PSX_CLOCK: u64 = 33_868_800;
const COUNTER_COUNT: usize = 4;
const COUNT_TO_OVERFLOW: u32 = 0;
const COUNT_TO_TARGET: u32 = 1;

// NTSC timing
const FRAME_RATE: u32 = 60;
const HSYNC_TOTAL: u32 = 263;
const VBLANK_START: u32 = 243;

// Mode flag bits (matching Redux enum)
const RC_COUNT_TO_TARGET: u16 = 0x0008;
const RC_IRQ_ON_TARGET: u16 = 0x0010;
const RC_IRQ_ON_OVERFLOW: u16 = 0x0020;
const RC_IRQ_REGENERATE: u16 = 0x0040;
const RC0_PIXEL_CLOCK: u16 = 0x0100;
const RC1_HSYNC_CLOCK: u16 = 0x0100;
const RC2_DISABLE: u16 = 0x0001;
const RC2_ONE_EIGHTH_CLOCK: u16 = 0x0200;
const RC_IRQ_REQUEST: u16 = 0x0400;
const RC_COUNT_EQ_TARGET: u16 = 0x0800;
const RC_OVERFLOW: u16 = 0x1000;

#[derive(Clone)]
struct Rcnt {
    mode: u16,
    target: u16,
    rate: u32,
    irq: u32,           // IRQ bitmask for ISTAT (0x10, 0x20, 0x40)
    counter_state: u32, // CountToTarget or CountToOverflow
    irq_state: bool,
    cycle: u64,         // cycles from cycleStart until next event fires
    cycle_start: u64,   // reference cycle for counter value computation
}

impl Rcnt {
    fn new() -> Self {
        Self {
            mode: 0,
            target: 0,
            rate: 1,
            irq: 0,
            counter_state: COUNT_TO_OVERFLOW,
            irq_state: false,
            cycle: 0,
            cycle_start: 0,
        }
    }
}

pub struct Timers {
    rcnts: [Rcnt; COUNTER_COUNT],
    hsync_count: u32,
    /// Next cycle at which update() needs to run
    pub next_counter: u64,
    /// Accumulated IRQ bitmask — caller drains into ISTAT
    pending_irqs: u32,
}

impl Timers {
    pub fn new() -> Self {
        let mut t = Self {
            rcnts: [Rcnt::new(), Rcnt::new(), Rcnt::new(), Rcnt::new()],
            hsync_count: 0,
            next_counter: 0,
            pending_irqs: 0,
        };
        t.init(0);
        t
    }

    pub fn init(&mut self, cycle: u64) {
        // Counter 0: dot clock (pixel clock when bit 8 set, else system clock)
        self.rcnts[0].rate = 1;
        self.rcnts[0].irq = 0x10; // IRQ4

        // Counter 1: hsync clock when bit 8 set, else system clock
        self.rcnts[1].rate = 1;
        self.rcnts[1].irq = 0x20; // IRQ5

        // Counter 2: 1/8 system clock when bit 9 set, else system clock
        self.rcnts[2].rate = 1;
        self.rcnts[2].irq = 0x40; // IRQ6

        // Counter 3: virtual base counter (hsync driver)
        self.rcnts[3].rate = 1;
        self.rcnts[3].mode = RC_COUNT_TO_TARGET;
        self.rcnts[3].target =
            (PSX_CLOCK / (FRAME_RATE as u64 * HSYNC_TOTAL as u64)) as u16;

        for i in 0..COUNTER_COUNT {
            self.write_counter_internal(i, 0, cycle);
        }

        self.hsync_count = 0;
        self.recompute_next(cycle);
    }

    // ======== Internal helpers ========

    fn fire_irq(&mut self, mask: u32) {
        // DIAG: track timer 2 IRQ fires
        if mask & 0x40 != 0 {
            use std::sync::atomic::{AtomicU32, Ordering};
            static T2_COUNT: AtomicU32 = AtomicU32::new(0);
            let n = T2_COUNT.fetch_add(1, Ordering::Relaxed);
            if n < 5 {
                eprintln!("TIMER2_IRQ #{}: mode={:04X} target={} counter_state={}",
                    n, self.rcnts[2].mode, self.rcnts[2].target, self.rcnts[2].counter_state);
            }
        }
        self.pending_irqs |= mask;
    }

    /// Reset the VBlank phase so the next VBlank is a full frame away.
    /// Called after fast_boot to match the state the real BIOS produces:
    /// the shell's main loop is VSync-locked, so loadAndExec/exec runs
    /// right after a VBlank was processed, placing the next VBlank ~243
    /// hsyncs (~564K cycles) in the future. This gives the game time to
    /// complete its exception handler setup before the first interrupt.
    pub fn reset_vblank_phase(&mut self, cycle: u64) {
        self.hsync_count = 0;
        self.rcnts[3].cycle_start = cycle;
        self.pending_irqs &= !0x01; // clear any pending VBlank
        self.recompute_next(cycle);
    }

    /// Drain accumulated IRQs. Caller should OR result into ISTAT.
    pub fn drain_irqs(&mut self) -> u32 {
        let irqs = self.pending_irqs;
        self.pending_irqs = 0;
        irqs
    }

    /// Set counter value and determine next event (target or overflow).
    /// Matching Redux writeCounterInternal().
    fn write_counter_internal(&mut self, index: usize, value: u32, cycle: u64) {
        let value = value & 0xFFFF;
        let rate = self.rcnts[index].rate as u64;
        let target = self.rcnts[index].target;

        self.rcnts[index].cycle_start = cycle.wrapping_sub(value as u64 * rate);

        if value < target as u32 {
            self.rcnts[index].cycle = target as u64 * rate;
            self.rcnts[index].counter_state = COUNT_TO_TARGET;
        } else {
            self.rcnts[index].cycle = 0xFFFF_u64 * rate;
            self.rcnts[index].counter_state = COUNT_TO_OVERFLOW;
        }
    }

    /// Read current counter value from cycle delta.
    /// Matching Redux readCounterInternal().
    fn read_counter_internal(&self, index: usize, cycle: u64) -> u32 {
        let count = cycle.wrapping_sub(self.rcnts[index].cycle_start)
            / self.rcnts[index].rate as u64;
        (count & 0xFFFF) as u32
    }

    /// Recompute next_counter deadline across all 4 counters.
    /// Matching Redux set().
    fn recompute_next(&mut self, cycle: u64) {
        let mut next: i64 = i64::MAX;

        for i in 0..COUNTER_COUNT {
            let elapsed = cycle.wrapping_sub(self.rcnts[i].cycle_start);
            let count_to_update = self.rcnts[i].cycle as i64 - elapsed as i64;

            if count_to_update < 0 {
                next = 0;
                break;
            }
            if count_to_update < next {
                next = count_to_update;
            }
        }

        self.next_counter = cycle.wrapping_add(next as u64);
    }

    /// Handle counter reaching target or overflow.
    /// Fires IRQs, resets counter, sets mode flags.
    /// Matching Redux reset().
    fn reset(&mut self, index: usize, cycle: u64) {
        // Copy fields to avoid borrow conflicts
        let counter_state = self.rcnts[index].counter_state;
        let mode = self.rcnts[index].mode;
        let target = self.rcnts[index].target;
        let rate = self.rcnts[index].rate as u64;
        let cycle_start = self.rcnts[index].cycle_start;
        let irq = self.rcnts[index].irq;
        let irq_state = self.rcnts[index].irq_state;

        if counter_state == COUNT_TO_TARGET {
            let count = if mode & RC_COUNT_TO_TARGET != 0 {
                // Reset to 0 on target — compute overshoot
                let c = cycle.wrapping_sub(cycle_start) / rate;
                c.wrapping_sub(target as u64)
            } else {
                // Don't reset — just read current value
                self.read_counter_internal(index, cycle) as u64
            };

            self.write_counter_internal(index, count as u32, cycle);

            if mode & RC_IRQ_ON_TARGET != 0 {
                if (mode & RC_IRQ_REGENERATE != 0) || !irq_state {
                    self.fire_irq(irq);
                    self.rcnts[index].irq_state = true;
                }
            }

            self.rcnts[index].mode |= RC_COUNT_EQ_TARGET;
        } else if counter_state == COUNT_TO_OVERFLOW {
            let count = cycle.wrapping_sub(cycle_start) / rate;
            let count = count.wrapping_sub(0xFFFF);

            self.write_counter_internal(index, count as u32, cycle);

            if mode & RC_IRQ_ON_OVERFLOW != 0 {
                if (mode & RC_IRQ_REGENERATE != 0) || !irq_state {
                    self.fire_irq(irq);
                    self.rcnts[index].irq_state = true;
                }
            }

            self.rcnts[index].mode |= RC_OVERFLOW;
        }

        self.rcnts[index].mode |= RC_IRQ_REQUEST;
        self.recompute_next(cycle);
    }

    // ======== Public API ========

    /// Main update — called from branchTest when cycle >= next_counter.
    /// Matching Redux update() (minus SPU/SIO1 sync).
    pub fn update(&mut self, cycle: u64) {
        // Counter 0
        if cycle.wrapping_sub(self.rcnts[0].cycle_start) >= self.rcnts[0].cycle {
            self.reset(0, cycle);
        }

        // Counter 1
        if cycle.wrapping_sub(self.rcnts[1].cycle_start) >= self.rcnts[1].cycle {
            self.reset(1, cycle);
        }

        // Counter 2
        if cycle.wrapping_sub(self.rcnts[2].cycle_start) >= self.rcnts[2].cycle {
            self.reset(2, cycle);
        }

        // Counter 3 (base — hsync driver)
        if cycle.wrapping_sub(self.rcnts[3].cycle_start) >= self.rcnts[3].cycle {
            self.reset(3, cycle);

            self.hsync_count += 1;

            // VBlank IRQ at scanline 243 (NTSC)
            if self.hsync_count == VBLANK_START {
                self.fire_irq(0x01); // IRQ0 = VBlank
                // DIAG: count VBlank fires
                use std::sync::atomic::{AtomicU32, Ordering};
                static VB_COUNT: AtomicU32 = AtomicU32::new(0);
                let n = VB_COUNT.fetch_add(1, Ordering::Relaxed);
                if n < 5 || n % 1000 == 0 {
                    eprintln!("VBLANK_FIRE #{}: cycle={} hsync={} pending_irqs={:04X} ctr3_state={} ctr3_cycle={} ctr3_start={}",
                        n, cycle, self.hsync_count, self.pending_irqs,
                        self.rcnts[3].counter_state, self.rcnts[3].cycle, self.rcnts[3].cycle_start);
                }
            }

            // Frame boundary — reset scanline counter
            if self.hsync_count >= HSYNC_TOTAL {
                self.hsync_count = 0;
            }
        }
    }

    /// Write counter value register. Matching Redux writeCounter().
    pub fn write_counter(&mut self, index: usize, value: u32, cycle: u64) {
        self.update(cycle);
        self.write_counter_internal(index, value, cycle);
        self.recompute_next(cycle);
    }

    /// Write mode register. Matching Redux writeMode().
    pub fn write_mode(&mut self, index: usize, value: u32, cycle: u64) {
        self.update(cycle);
        self.rcnts[index].mode = value as u16;
        self.rcnts[index].irq_state = false;

        match index {
            0 => {
                self.rcnts[0].rate = if value as u16 & RC0_PIXEL_CLOCK != 0 { 5 } else { 1 };
            }
            1 => {
                self.rcnts[1].rate = if value as u16 & RC1_HSYNC_CLOCK != 0 {
                    (PSX_CLOCK / (FRAME_RATE as u64 * HSYNC_TOTAL as u64)) as u32
                } else {
                    1
                };
            }
            2 => {
                self.rcnts[2].rate = if value as u16 & RC2_ONE_EIGHTH_CLOCK != 0 {
                    8
                } else {
                    1
                };
                if value as u16 & RC2_DISABLE != 0 {
                    self.rcnts[2].rate = 0xFFFF_FFFF;
                }
            }
            _ => {}
        }

        self.write_counter_internal(index, 0, cycle);
        self.recompute_next(cycle);
    }

    /// Write target register. Matching Redux writeTarget().
    pub fn write_target(&mut self, index: usize, value: u32, cycle: u64) {
        self.update(cycle);
        self.rcnts[index].target = value as u16;
        let count = self.read_counter_internal(index, cycle);
        self.write_counter_internal(index, count, cycle);
        self.recompute_next(cycle);
    }

    /// Read counter value. Matching Redux readCounter().
    pub fn read_counter(&mut self, index: usize, cycle: u64) -> u32 {
        self.update(cycle);
        self.read_counter_internal(index, cycle)
    }

    /// Read mode register. Clears bits 11-12 on read. Matching Redux readMode().
    pub fn read_mode(&mut self, index: usize, cycle: u64) -> u32 {
        self.update(cycle);
        let mode = self.rcnts[index].mode;
        self.rcnts[index].mode &= 0xE7FF; // Clear CountEqTarget + Overflow flags
        mode as u32
    }

    /// Read target register.
    pub fn read_target(&self, index: usize) -> u32 {
        self.rcnts[index].target as u32
    }
}
