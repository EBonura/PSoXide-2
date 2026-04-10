pub mod exceptions;
pub mod icache;
pub mod interpreter;
pub mod registers;

use crate::bus::Bus;
use registers::Registers;

pub struct Cpu {
    pub regs: Registers,
    pub icache_addr: [u32; 1024], // 4KB / 4 bytes = 1024 word entries
    pub icache_code: [u32; 1024],
    delayed_load: [DelayedLoadSlot; 2],
    current_delayed_load: usize,
    pub in_delay_slot: bool,
    pub next_is_delay_slot: bool,
}

#[derive(Clone, Copy, Default)]
struct DelayedLoadSlot {
    index: u32,
    value: u32,
    mask: u32,
    pc_value: u32,
    active: bool,
    pc_active: bool,
}

impl Cpu {
    pub fn new() -> Self {
        Self {
            regs: Registers::new(),
            icache_addr: [0xFFFF_FFFF; 1024], // Invalid tags
            icache_code: [0; 1024],
            delayed_load: [DelayedLoadSlot::default(); 2],
            current_delayed_load: 0,
            in_delay_slot: false,
            next_is_delay_slot: false,
        }
    }

    pub fn reset(&mut self) {
        self.regs = Registers::new();
        self.regs.pc = 0xBFC0_0000; // BIOS entry (KSEG1)
        self.regs.cp0[registers::CP0_STATUS] = 0x1090_0000; // COP0 enabled, BEV=1, TS=1
        self.regs.cp0[registers::CP0_PRID] = 0x0000_0002; // R3000A revision
        self.delayed_load = [DelayedLoadSlot::default(); 2];
        self.current_delayed_load = 0;
        self.in_delay_slot = false;
        self.next_is_delay_slot = false;
        self.icache_addr = [0xFFFF_FFFF; 1024];
        self.icache_code = [0; 1024];
    }

    pub fn step(&mut self, bus: &mut Bus) {
        if self.next_is_delay_slot {
            self.in_delay_slot = true;
            self.next_is_delay_slot = false;
        }

        let pc = self.regs.pc;
        let code = self.read_icache(pc, bus);
        self.regs.current_instruction = code;
        self.regs.pc = pc.wrapping_add(4);
        self.regs.cycle += 2; // BIAS

        self.execute(bus, code);

        // Toggle delayed load slot and flush
        self.current_delayed_load ^= 1;
        self.flush_current_delayed_load();

        // Handle delayed PC load
        let slot = &mut self.delayed_load[self.current_delayed_load];
        if slot.pc_active {
            self.regs.pc = slot.pc_value;
            slot.pc_active = false;
        }

        if self.in_delay_slot {
            self.in_delay_slot = false;
            // Intercept BIOS calls
            self.intercept_bios(bus);
            self.branch_test(bus);
        }
    }

    fn delayed_load(&mut self, reg: u32, value: u32) {
        let slot = &mut self.delayed_load[self.current_delayed_load];
        slot.active = true;
        slot.index = reg;
        slot.value = value;
        slot.mask = 0;
    }

    fn delayed_load_masked(&mut self, reg: u32, value: u32, mask: u32) {
        let slot = &mut self.delayed_load[self.current_delayed_load];
        slot.active = true;
        slot.index = reg;
        slot.value = value;
        slot.mask = mask;
    }

    fn delayed_pc_load(&mut self, value: u32) {
        let slot = &mut self.delayed_load[self.current_delayed_load];
        slot.pc_active = true;
        slot.pc_value = value;
    }

    fn flush_current_delayed_load(&mut self) {
        let slot = self.delayed_load[self.current_delayed_load];
        if slot.active {
            let reg = slot.index as usize;
            if reg != 0 {
                let current = self.regs.gpr[reg];
                self.regs.gpr[reg] = (current & slot.mask) | slot.value;
            }
            self.delayed_load[self.current_delayed_load].active = false;
        }
    }

    fn cancel_delayed_load(&mut self, index: u32) {
        let other = self.current_delayed_load ^ 1;
        if self.delayed_load[other].index == index {
            self.delayed_load[other].active = false;
        }
    }

    fn branch(&mut self, target: u32) {
        self.next_is_delay_slot = true;
        self.delayed_pc_load(target);
    }

    fn intercept_bios(&self, bus: &mut Bus) {
        let pc = self.regs.pc;
        match pc {
            0x000000A0 | 0x000000B0 | 0x000000C0 => {
                let call = self.regs.gpr[9]; // t1 = function number
                match pc {
                    0xA0 => {
                        if call == 0x3C || call == 0x3E {
                            // putchar / puts
                            let ch = self.regs.gpr[4] as u8;
                            if ch.is_ascii() && ch != 0 {
                                eprint!("{}", ch as char);
                            }
                        }
                    }
                    0xB0 => {
                        if call == 0x3D || call == 0x3F {
                            let ch = self.regs.gpr[4] as u8;
                            if ch.is_ascii() && ch != 0 {
                                eprint!("{}", ch as char);
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn branch_test(&mut self, bus: &mut Bus) {
        let cycle = self.regs.cycle;

        // Update timers — check if VBlank fires
        let vblank = bus.timers.update(cycle, &mut bus.scheduler);
        if vblank {
            bus.set_irq(0); // IRQ0 = VBlank
        }

        // Check scheduled interrupts
        if bus.scheduler.interrupt_flags != 0 && bus.scheduler.lowest_target <= cycle {
            let fired = bus.scheduler.check_interrupts(cycle);
            bus.handle_fired_interrupts(fired);
        }

        // Check if any interrupt is pending and enabled
        let istat = bus.read_istat();
        let imask = bus.read_imask();
        if istat & imask != 0 {
            let status = self.regs.cp0[registers::CP0_STATUS];
            // IEc (bit 0) = interrupt enable current
            if status & 0x0401 == 0x0401 {
                // COP0 enabled and interrupts enabled
                self.regs.cp0[registers::CP0_CAUSE] |= 0x0400; // IP2 = hardware interrupt
                exceptions::exception(self, bus, exceptions::Exception::Interrupt);
            }
        }
    }
}
