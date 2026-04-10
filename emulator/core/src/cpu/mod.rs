pub mod exceptions;
pub mod icache;
pub mod interpreter;
pub mod registers;

use crate::bus::Bus;
use registers::Registers;

// Ring buffer trace for parity debugging
const TRACE_SIZE: usize = 64;

#[derive(Clone, Copy, Default)]
pub struct TraceEntry {
    pub pc: u32,
    pub instr: u32,
    pub t9: u32,      // $25
    pub ra: u32,      // $31
    pub k0: u32,      // $26
    pub k1: u32,      // $27
    pub sp: u32,      // $29
    pub status: u32,
    pub epc: u32,
    pub in_ds: bool,
}

pub static mut TRACE_BUF: [TraceEntry; TRACE_SIZE] = [TraceEntry {
    pc: 0, instr: 0, t9: 0, ra: 0, k0: 0, k1: 0, sp: 0, status: 0, epc: 0, in_ds: false,
}; TRACE_SIZE];
pub static mut TRACE_POS: usize = 0;
pub static mut TRACE_DUMPED: bool = false;

pub fn dump_trace_ring() {
    unsafe {
        if TRACE_DUMPED { return; }
        TRACE_DUMPED = true;
        eprintln!("\n=== TRACE RING (last {} instructions before crash) ===", TRACE_SIZE);
        eprintln!("{:<10} {:<10} {:<5} {:<10} {:<10} {:<10} {:<10} {:<10} {:<10}",
            "PC", "INSTR", "DS", "t9", "ra", "k0", "sp", "STATUS", "EPC");
        for i in 0..TRACE_SIZE {
            let idx = (TRACE_POS + i) % TRACE_SIZE;
            let e = &TRACE_BUF[idx];
            if e.pc == 0 && e.instr == 0 { continue; }
            eprintln!("{:08X}  {:08X}  {:<5} {:08X}  {:08X}  {:08X}  {:08X}  {:08X}  {:08X}",
                e.pc, e.instr, if e.in_ds { "DS" } else { "" },
                e.t9, e.ra, e.k0, e.sp, e.status, e.epc);
        }
        eprintln!("=== END TRACE ===\n");
    }
}

pub struct Cpu {
    pub regs: Registers,
    pub icache_addr: [u32; 1024], // 4KB / 4 bytes = 1024 word entries
    pub icache_code: [u32; 1024],
    pub delayed_load: [DelayedLoadSlot; 2],
    pub current_delayed_load: usize,
    pub in_delay_slot: bool,
    pub next_is_delay_slot: bool,
}

#[derive(Clone, Copy, Default)]
pub struct DelayedLoadSlot {
    pub index: u32,
    pub value: u32,
    pub mask: u32,
    pub pc_value: u32,
    pub active: bool,
    pub pc_active: bool,
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

        // Update bus cycle before execute so timer writes see current cycle
        bus.last_cycle = self.regs.cycle;

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
        let call = self.regs.gpr[9] & 0xFF; // t1 = function number
        match pc {
            0xA0 => {
                match call {
                    0x03 => {
                        // write(a0=fd, a1=buf, a2=len)
                        if self.regs.gpr[4] == 1 { // stdout only
                            let mut addr = self.regs.gpr[5];
                            let len = self.regs.gpr[6];
                            for _ in 0..len {
                                let ch = bus.read8(addr);
                                if ch.is_ascii() && ch != 0 { eprint!("{}", ch as char); }
                                addr = addr.wrapping_add(1);
                            }
                        }
                    }
                    0x09 | 0x3C => {
                        // putc / putchar
                        let ch = self.regs.gpr[4] as u8;
                        if ch.is_ascii() && ch != 0 { eprint!("{}", ch as char); }
                    }
                    0x3E => {
                        // puts(a0=string_ptr) — read string from memory
                        let mut addr = self.regs.gpr[4];
                        for _ in 0..1024 {
                            let ch = bus.read8(addr);
                            if ch == 0 { break; }
                            if ch.is_ascii() { eprint!("{}", ch as char); }
                            addr = addr.wrapping_add(1);
                        }
                    }
                    _ => {
                        tracing::debug!("BIOS A0({:02X}) a0={:08X} a1={:08X} a2={:08X} ra={:08X}",
                            call, self.regs.gpr[4], self.regs.gpr[5],
                            self.regs.gpr[6], self.regs.gpr[31]);
                    }
                }
            }
            0xB0 => {
                match call {
                    0x35 => {
                        // write(a0=fd, a1=buf, a2=len)
                        if self.regs.gpr[4] == 1 {
                            let mut addr = self.regs.gpr[5];
                            let len = self.regs.gpr[6];
                            for _ in 0..len {
                                let ch = bus.read8(addr);
                                if ch.is_ascii() && ch != 0 { eprint!("{}", ch as char); }
                                addr = addr.wrapping_add(1);
                            }
                        }
                    }
                    0x3B | 0x3D => {
                        // putc / putchar
                        let ch = self.regs.gpr[4] as u8;
                        if ch.is_ascii() && ch != 0 { eprint!("{}", ch as char); }
                    }
                    0x3F => {
                        // puts(a0=string_ptr)
                        let mut addr = self.regs.gpr[4];
                        for _ in 0..1024 {
                            let ch = bus.read8(addr);
                            if ch == 0 { break; }
                            if ch.is_ascii() { eprint!("{}", ch as char); }
                            addr = addr.wrapping_add(1);
                        }
                    }
                    _ => {
                        tracing::debug!("BIOS B0({:02X}) a0={:08X} a1={:08X} a2={:08X} ra={:08X}",
                            call, self.regs.gpr[4], self.regs.gpr[5],
                            self.regs.gpr[6], self.regs.gpr[31]);
                    }
                }
            }
            0xC0 => {
                tracing::debug!("BIOS C0({:02X}) a0={:08X} a1={:08X} a2={:08X} ra={:08X}",
                    call, self.regs.gpr[4], self.regs.gpr[5], self.regs.gpr[6], self.regs.gpr[31]);
            }
            _ => {}
        }
    }

    /// Software interrupt test — matching Redux psxTestSWInts().
    /// Called after MTC0 to Status/Cause and after RFE.
    pub fn test_sw_ints(&mut self, bus: &mut Bus) {
        if self.regs.cp0[registers::CP0_CAUSE] & self.regs.cp0[registers::CP0_STATUS] & 0x0300 != 0
            && self.regs.cp0[registers::CP0_STATUS] & 0x1 != 0
        {
            let in_delay_slot = self.in_delay_slot;
            self.in_delay_slot = false;
            exceptions::exception_raw(self, self.regs.cp0[registers::CP0_CAUSE], in_delay_slot);
            return; // SW interrupt fired — don't also fire HW
        }

        // Hardware interrupt check — on real R3000A, a pending unmasked IRQ
        // fires at the next instruction boundary after IEc is enabled.
        // Since we only check HW IRQs at branch boundaries (branch_test),
        // MTC0 / RFE enabling IEc can miss the delivery window.
        // This extends the check to Status-modifying instructions.
        let status = self.regs.cp0[registers::CP0_STATUS];
        if (status & 0x401) == 0x401 {
            let istat = bus.read_istat();
            let imask = bus.read_imask();
            if (istat & imask) != 0 {
                let in_delay_slot = self.in_delay_slot;
                self.in_delay_slot = false;
                exceptions::exception_raw(self, 0x400, in_delay_slot);
            }
        }
    }

    fn branch_test(&mut self, bus: &mut Bus) {
        let cycle = self.regs.cycle;
        bus.last_cycle = cycle;

        // Update counters — matching Redux branchTest() counter check
        if cycle >= bus.timers.next_counter {
            bus.timers.update(cycle);
            bus.drain_timer_irqs();
        }

        // Check scheduled interrupts (SIO, CDROM, DMA, etc.)
        if bus.scheduler.interrupt_flags != 0 && bus.scheduler.lowest_target <= cycle {
            let fired = bus.scheduler.check_interrupts(cycle);
            bus.handle_fired_interrupts(fired);
        }

        // Check if any hardware interrupt is pending and enabled
        // Matching Redux branchTest() lines 404-417
        let istat = bus.read_istat();
        let imask = bus.read_imask();

        if (istat & imask) != 0
            && (self.regs.cp0[registers::CP0_STATUS] & 0x401) == 0x401
        {
            // Fire interrupt exception — matching Redux: exception(0x400, 0)
            exceptions::exception_raw(self, 0x400, false);
        }
    }
}
