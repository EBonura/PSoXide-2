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

    // Trace first 50 exceptions with full register dump for StoreAddressError
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let seq = N.fetch_add(1, Ordering::Relaxed);
    if seq < 500 {
        let exc = (cpu.regs.cp0[CP0_CAUSE] >> 2) & 0x1F;
        eprintln!("EXC {:>2} e={:<2} PC={:08X} EPC={:08X} S={:08X} badvaddr={:08X}",
            seq, exc, cpu.regs.pc, cpu.regs.cp0[CP0_EPC], cpu.regs.cp0[CP0_STATUS],
            cpu.regs.cp0[CP0_BADVADDR]);
        // For StoreAddressError, dump registers
        if exc == 5 {
            eprintln!("  SAE: s3={:08X} s6={:08X} a2={:08X} v0={:08X} ra={:08X}",
                cpu.regs.gpr[19], cpu.regs.gpr[22], cpu.regs.gpr[6],
                cpu.regs.gpr[2], cpu.regs.gpr[31]);
        }
    }
}

/// Synchronous exception — matching Redux pc -= 4 pattern.
pub fn exception(cpu: &mut Cpu, bus: &mut Bus, exc: Exception) {
    cpu.regs.pc = cpu.regs.pc.wrapping_sub(4);
    let bd = cpu.in_delay_slot;
    let code = (exc as u32) << 2;

    // One-shot dump for first AdES: show faulting instruction and handler entry
    if matches!(exc, Exception::StoreAddressError | Exception::LoadAddressError) {
        use std::sync::atomic::{AtomicBool, Ordering};
        static DUMPED: AtomicBool = AtomicBool::new(false);
        if !DUMPED.swap(true, Ordering::Relaxed) {
            let fault_pc = cpu.regs.pc;
            let fault_instr = bus.read32(fault_pc);
            eprintln!("=== FIRST ADDR ERR at PC={:08X} instr={:08X} BadVAddr={:08X} ===",
                fault_pc, fault_instr, cpu.regs.cp0[CP0_BADVADDR]);
            // Dump 16 instructions around the fault
            for i in 0..16u32 {
                let addr = fault_pc.wrapping_sub(16).wrapping_add(i * 4);
                let instr = bus.read32(addr);
                let mark = if addr == fault_pc { " <<<" } else { "" };
                eprintln!("  {:08X}: {:08X}{}", addr, instr, mark);
            }
            // Dump exception handler at 0x80-0xC0
            eprintln!("  EXCEPTION HANDLER at 0x80:");
            for i in 0..16u32 {
                eprintln!("    {:08X}: {:08X}", 0x80 + i * 4, bus.read32(0x80 + i * 4));
            }
            // Dump handler continuation at 0x1540
            eprintln!("  HANDLER CONTINUATION at 0x1540:");
            for i in 0..16u32 {
                eprintln!("    {:08X}: {:08X}", 0x80001540u32.wrapping_add(i * 4), bus.read32(0x80001540u32.wrapping_add(i * 4)));
            }
            // Dump all GPRs
            for i in (0..32).step_by(4) {
                eprintln!("  r{:02}-r{:02}: {:08X} {:08X} {:08X} {:08X}",
                    i, i+3, cpu.regs.gpr[i], cpu.regs.gpr[i+1], cpu.regs.gpr[i+2], cpu.regs.gpr[i+3]);
            }
        }
    }

    exception_raw(cpu, code, bd);

    if bd {
        cpu.delayed_load[cpu.current_delayed_load].pc_active = false;
    }
}
