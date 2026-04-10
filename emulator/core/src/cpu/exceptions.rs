use crate::bus::Bus;
use super::Cpu;
use super::registers::*;

#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub enum Exception {
    Interrupt = 0,
    LoadAddressError = 4,
    StoreAddressError = 5,
    InstructionBusError = 6,
    DataBusError = 7,
    Syscall = 8,
    Break = 9,
    ReservedInstruction = 10,
    CoprocessorUnusable = 11,
    ArithmeticOverflow = 12,
}

/// Raw exception entry — matching Redux r3000a.cc exception().
pub fn exception_raw(cpu: &mut Cpu, code: u32, bd: bool) {
    if bd {
        cpu.regs.cp0[CP0_CAUSE] = code | 0x8000_0000;
        cpu.regs.cp0[CP0_EPC] = cpu.regs.pc.wrapping_sub(4);
    } else {
        cpu.regs.cp0[CP0_CAUSE] = code;
        cpu.regs.cp0[CP0_EPC] = cpu.regs.pc;
    }

    if cpu.regs.cp0[CP0_STATUS] & 0x0040_0000 != 0 {
        cpu.regs.pc = 0xBFC0_0180;
    } else {
        cpu.regs.pc = 0x8000_0080;
    }

    let status = cpu.regs.cp0[CP0_STATUS];
    cpu.regs.cp0[CP0_STATUS] = (status & !0x3F) | ((status & 0x0F) << 2);

    // Trace first 15 exceptions
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let seq = N.fetch_add(1, Ordering::Relaxed);
    if seq < 15 {
        let exc = (cpu.regs.cp0[CP0_CAUSE] >> 2) & 0x1F;
        eprintln!("EXC {:>2} e={:<2} PC={:08X} EPC={:08X} S={:08X}",
            seq, exc, cpu.regs.pc, cpu.regs.cp0[CP0_EPC], cpu.regs.cp0[CP0_STATUS]);
    }
}

/// Synchronous exception — matching Redux pc -= 4 pattern.
pub fn exception(cpu: &mut Cpu, _bus: &mut Bus, exc: Exception) {
    cpu.regs.pc = cpu.regs.pc.wrapping_sub(4);
    let bd = cpu.in_delay_slot;
    let code = (exc as u32) << 2;

    exception_raw(cpu, code, bd);

    if bd {
        cpu.delayed_load[cpu.current_delayed_load].pc_active = false;
    }
}
