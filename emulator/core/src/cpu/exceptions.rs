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

/// Raw exception — takes pre-built cause code directly (matches Redux calling convention).
/// Used by branchTest which passes 0x400 for hardware interrupts.
pub fn exception_raw(cpu: &mut Cpu, code: u32) {
    // Set EPC & PC
    if cpu.in_delay_slot {
        cpu.regs.cp0[CP0_CAUSE] = code | 0x8000_0000; // BD bit
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

    cpu.in_delay_slot = false;
    cpu.next_is_delay_slot = false;
}

/// Exception handler — matches PCSX-Redux r3000a.cc line 279-301 exactly.
///
/// At this point, cpu.regs.pc has already been advanced past the current
/// instruction (pc += 4 happened in step()). So:
///   - Non-BD: EPC = pc (the next instruction — resume point after handler)
///   - BD: EPC = pc - 4 (the branch instruction — re-execute the branch)
///     Cause bit 31 (BD) is set to indicate delay slot.
pub fn exception(cpu: &mut Cpu, _bus: &mut Bus, exc: Exception) {
    let code = (exc as u32) << 2;

    // Set EPC & PC — matching Redux lines 282-293
    if cpu.in_delay_slot {
        cpu.regs.cp0[CP0_CAUSE] = code | 0x8000_0000; // BD bit + ExcCode
        cpu.regs.cp0[CP0_EPC] = cpu.regs.pc.wrapping_sub(4);
    } else {
        cpu.regs.cp0[CP0_CAUSE] = code; // ExcCode only
        cpu.regs.cp0[CP0_EPC] = cpu.regs.pc;
    }

    // Exception vector — BEV bit (Status bit 22) selects vector base
    if cpu.regs.cp0[CP0_STATUS] & 0x0040_0000 != 0 {
        cpu.regs.pc = 0xBFC0_0180;
    } else {
        cpu.regs.pc = 0x8000_0080;
    }

    // Push the interrupt/kernel mode stack — Redux line 300
    // Status bits [5:0] = {KUo, IEo, KUp, IEp, KUc, IEc}
    // Push: shift current pair into previous, previous into old
    let status = cpu.regs.cp0[CP0_STATUS];
    cpu.regs.cp0[CP0_STATUS] = (status & !0x3F) | ((status & 0x0F) << 2);

    cpu.in_delay_slot = false;
    cpu.next_is_delay_slot = false;
}
