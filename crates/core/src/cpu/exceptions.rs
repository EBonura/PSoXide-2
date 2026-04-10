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

pub fn exception(cpu: &mut Cpu, _bus: &mut Bus, exc: Exception) {
    let code = (exc as u32) << 2;
    let status = cpu.regs.cp0[CP0_STATUS];

    // Push the kernel/user + interrupt enable stack (shift left by 2)
    cpu.regs.cp0[CP0_STATUS] = (status & !0x3F) | ((status & 0x0F) << 2);

    // Set ExcCode in Cause
    cpu.regs.cp0[CP0_CAUSE] = (cpu.regs.cp0[CP0_CAUSE] & !0x7C) | code;

    // Set EPC to current instruction (back up if in delay slot)
    if cpu.in_delay_slot {
        cpu.regs.cp0[CP0_EPC] = cpu.regs.pc.wrapping_sub(8);
        cpu.regs.cp0[CP0_CAUSE] |= 0x8000_0000; // BD bit
    } else {
        cpu.regs.cp0[CP0_EPC] = cpu.regs.pc.wrapping_sub(4);
        cpu.regs.cp0[CP0_CAUSE] &= !0x8000_0000;
    }

    // Exception vector: BEV determines base
    let vector = if status & 0x0040_0000 != 0 {
        // BEV = 1: bootstrap vectors
        0xBFC0_0180
    } else {
        // BEV = 0: normal vectors
        0x8000_0080
    };

    cpu.regs.pc = vector;
    cpu.in_delay_slot = false;
    cpu.next_is_delay_slot = false;
}
