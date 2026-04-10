use crate::bus::Bus;
use super::{Cpu, exceptions};
use super::registers::*;

// Instruction field extraction
#[inline(always)] fn op(code: u32) -> u32 { code >> 26 }
#[inline(always)] fn rs(code: u32) -> usize { ((code >> 21) & 0x1F) as usize }
#[inline(always)] fn rt(code: u32) -> usize { ((code >> 16) & 0x1F) as usize }
#[inline(always)] fn rd(code: u32) -> usize { ((code >> 11) & 0x1F) as usize }
#[inline(always)] fn sa(code: u32) -> u32 { (code >> 6) & 0x1F }
#[inline(always)] fn funct(code: u32) -> u32 { code & 0x3F }
#[inline(always)] fn imm16(code: u32) -> u16 { code as u16 }
#[inline(always)] fn imm_se(code: u32) -> u32 { (code as i16) as i32 as u32 }
#[inline(always)] fn imm_ze(code: u32) -> u32 { code & 0xFFFF }
#[inline(always)] fn target(code: u32) -> u32 { code & 0x03FF_FFFF }

impl Cpu {
    pub fn execute(&mut self, bus: &mut Bus, code: u32) {
        match op(code) {
            0x00 => self.execute_special(bus, code),
            0x01 => self.execute_regimm(bus, code),
            0x02 => self.op_j(code),
            0x03 => self.op_jal(code),
            0x04 => self.op_beq(code),
            0x05 => self.op_bne(code),
            0x06 => self.op_blez(code),
            0x07 => self.op_bgtz(code),
            0x08 => self.op_addi(bus, code),
            0x09 => self.op_addiu(code),
            0x0A => self.op_slti(code),
            0x0B => self.op_sltiu(code),
            0x0C => self.op_andi(code),
            0x0D => self.op_ori(code),
            0x0E => self.op_xori(code),
            0x0F => self.op_lui(code),
            0x10 => self.execute_cop0(bus, code),
            0x12 => self.execute_cop2(bus, code),
            0x20 => self.op_lb(bus, code),
            0x21 => self.op_lh(bus, code),
            0x22 => self.op_lwl(bus, code),
            0x23 => self.op_lw(bus, code),
            0x24 => self.op_lbu(bus, code),
            0x25 => self.op_lhu(bus, code),
            0x26 => self.op_lwr(bus, code),
            0x28 => self.op_sb(bus, code),
            0x29 => self.op_sh(bus, code),
            0x2A => self.op_swl(bus, code),
            0x2B => self.op_sw(bus, code),
            0x2E => self.op_swr(bus, code),
            0x32 => self.op_lwc2(bus, code),
            0x3A => self.op_swc2(bus, code),
            _ => {
                tracing::warn!("Unhandled opcode {:02X} at PC {:08X}", op(code), self.regs.pc.wrapping_sub(4));
            }
        }
    }

    fn execute_special(&mut self, bus: &mut Bus, code: u32) {
        match funct(code) {
            0x00 => self.op_sll(code),
            0x02 => self.op_srl(code),
            0x03 => self.op_sra(code),
            0x04 => self.op_sllv(code),
            0x06 => self.op_srlv(code),
            0x07 => self.op_srav(code),
            0x08 => self.op_jr(code),
            0x09 => self.op_jalr(code),
            0x0C => self.op_syscall(bus),
            0x0D => self.op_break(bus),
            0x10 => self.op_mfhi(code),
            0x11 => self.op_mthi(code),
            0x12 => self.op_mflo(code),
            0x13 => self.op_mtlo(code),
            0x18 => self.op_mult(code),
            0x19 => self.op_multu(code),
            0x1A => self.op_div(code),
            0x1B => self.op_divu(code),
            0x20 => self.op_add(bus, code),
            0x21 => self.op_addu(code),
            0x22 => self.op_sub(bus, code),
            0x23 => self.op_subu(code),
            0x24 => self.op_and(code),
            0x25 => self.op_or(code),
            0x26 => self.op_xor(code),
            0x27 => self.op_nor(code),
            0x2A => self.op_slt(code),
            0x2B => self.op_sltu(code),
            _ => {
                tracing::warn!("Unhandled SPECIAL funct {:02X} at PC {:08X}", funct(code), self.regs.pc.wrapping_sub(4));
            }
        }
    }

    fn execute_regimm(&mut self, _bus: &mut Bus, code: u32) {
        let rt_field = rt(code) as u32;
        let rs_val = self.regs.gpr[rs(code)] as i32;
        let offset = (imm_se(code) << 2).wrapping_add(self.regs.pc);

        let link = rt_field & 0x10 != 0; // bit 4 = link
        let branch = match rt_field & 1 {
            0 => rs_val < 0,  // BLTZ / BLTZAL
            1 => rs_val >= 0, // BGEZ / BGEZAL
            _ => unreachable!(),
        };

        if link {
            self.regs.set_gpr(31, self.regs.pc); // RA = PC (already advanced past delay slot target)
        }

        if branch {
            self.branch(offset);
        }
    }

    fn execute_cop0(&mut self, bus: &mut Bus, code: u32) {
        match rs(code) as u32 {
            0x00 => { // MFC0
                let val = self.regs.cp0[rd(code)];
                self.cancel_delayed_load(rt(code) as u32);
                self.delayed_load(rt(code) as u32, val);
            }
            0x02 => { // CFC0
                let val = self.regs.cp0[rd(code)];
                self.cancel_delayed_load(rt(code) as u32);
                self.delayed_load(rt(code) as u32, val);
            }
            0x04 | 0x06 => { // MTC0 / CTC0
                let val = self.regs.gpr[rt(code)];
                let reg = rd(code);
                // Matching Redux MTC0(): special handling for Status and Cause
                match reg {
                    CP0_STATUS => {
                        self.regs.cp0[CP0_STATUS] = val;
                        self.test_sw_ints(bus);
                    }
                    CP0_CAUSE => {
                        // Only bits 8-9 (SW interrupt flags) are writable
                        self.regs.cp0[CP0_CAUSE] = (self.regs.cp0[CP0_CAUSE] & !0x0300) | (val & 0x0300);
                        self.test_sw_ints(bus);
                    }
                    _ => {
                        self.regs.cp0[reg] = val;
                    }
                }
            }
            0x10 => { // RFE — matching Redux psxRFE()
                let status = self.regs.cp0[CP0_STATUS];
                self.regs.cp0[CP0_STATUS] = (status & 0xFFFF_FFF0) | ((status & 0x3C) >> 2);
                self.test_sw_ints(bus);
            }
            _ => {
                tracing::warn!("Unhandled COP0 rs={:02X} at PC {:08X}", rs(code), self.regs.pc.wrapping_sub(4));
            }
        }
    }

    fn execute_cop2(&mut self, _bus: &mut Bus, code: u32) {
        let rs_field = rs(code) as u32;
        match rs_field {
            0x00 => { // MFC2
                let val = self.regs.cp2d[rd(code)];
                self.cancel_delayed_load(rt(code) as u32);
                self.delayed_load(rt(code) as u32, val);
            }
            0x02 => { // CFC2
                let val = self.regs.cp2c[rd(code)];
                self.cancel_delayed_load(rt(code) as u32);
                self.delayed_load(rt(code) as u32, val);
            }
            0x04 => { // MTC2
                let val = self.regs.gpr[rt(code)];
                self.regs.cp2d[rd(code)] = val;
            }
            0x06 => { // CTC2
                let val = self.regs.gpr[rt(code)];
                self.regs.cp2c[rd(code)] = val;
            }
            0x10..=0x1F => {
                // GTE command — stub for now
                tracing::trace!("GTE command {:08X} at PC {:08X}", code, self.regs.pc.wrapping_sub(4));
            }
            _ => {
                tracing::warn!("Unhandled COP2 rs={:02X} at PC {:08X}", rs_field, self.regs.pc.wrapping_sub(4));
            }
        }
    }

    // ======== J-type ========

    fn op_j(&mut self, code: u32) {
        let target_addr = (target(code) << 2) | (self.regs.pc & 0xF000_0000);
        self.branch(target_addr);
    }

    fn op_jal(&mut self, code: u32) {
        self.regs.set_gpr(31, self.regs.pc); // RA = return address
        let target_addr = (target(code) << 2) | (self.regs.pc & 0xF000_0000);
        self.branch(target_addr);
    }

    // ======== Branch ========

    fn op_beq(&mut self, code: u32) {
        if self.regs.gpr[rs(code)] == self.regs.gpr[rt(code)] {
            let offset = imm_se(code) << 2;
            self.branch(self.regs.pc.wrapping_add(offset));
        }
    }

    fn op_bne(&mut self, code: u32) {
        if self.regs.gpr[rs(code)] != self.regs.gpr[rt(code)] {
            let offset = imm_se(code) << 2;
            self.branch(self.regs.pc.wrapping_add(offset));
        }
    }

    fn op_blez(&mut self, code: u32) {
        if (self.regs.gpr[rs(code)] as i32) <= 0 {
            let offset = imm_se(code) << 2;
            self.branch(self.regs.pc.wrapping_add(offset));
        }
    }

    fn op_bgtz(&mut self, code: u32) {
        if (self.regs.gpr[rs(code)] as i32) > 0 {
            let offset = imm_se(code) << 2;
            self.branch(self.regs.pc.wrapping_add(offset));
        }
    }

    // ======== Arithmetic Immediate ========

    fn op_addi(&mut self, bus: &mut Bus, code: u32) {
        let s = self.regs.gpr[rs(code)] as i32;
        let imm = imm_se(code) as i32;
        match s.checked_add(imm) {
            Some(result) => {
                self.cancel_delayed_load(rt(code) as u32);
                self.regs.set_gpr(rt(code), result as u32);
            }
            None => exceptions::exception(self, bus, exceptions::Exception::ArithmeticOverflow),
        }
    }

    fn op_addiu(&mut self, code: u32) {
        let result = self.regs.gpr[rs(code)].wrapping_add(imm_se(code));
        self.cancel_delayed_load(rt(code) as u32);
        self.regs.set_gpr(rt(code), result);
    }

    fn op_slti(&mut self, code: u32) {
        let result = (self.regs.gpr[rs(code)] as i32) < (imm_se(code) as i32);
        self.cancel_delayed_load(rt(code) as u32);
        self.regs.set_gpr(rt(code), result as u32);
    }

    fn op_sltiu(&mut self, code: u32) {
        let result = self.regs.gpr[rs(code)] < imm_se(code);
        self.cancel_delayed_load(rt(code) as u32);
        self.regs.set_gpr(rt(code), result as u32);
    }

    fn op_andi(&mut self, code: u32) {
        let result = self.regs.gpr[rs(code)] & imm_ze(code);
        self.cancel_delayed_load(rt(code) as u32);
        self.regs.set_gpr(rt(code), result);
    }

    fn op_ori(&mut self, code: u32) {
        let result = self.regs.gpr[rs(code)] | imm_ze(code);
        self.cancel_delayed_load(rt(code) as u32);
        self.regs.set_gpr(rt(code), result);
    }

    fn op_xori(&mut self, code: u32) {
        let result = self.regs.gpr[rs(code)] ^ imm_ze(code);
        self.cancel_delayed_load(rt(code) as u32);
        self.regs.set_gpr(rt(code), result);
    }

    fn op_lui(&mut self, code: u32) {
        let result = (imm16(code) as u32) << 16;
        self.cancel_delayed_load(rt(code) as u32);
        self.regs.set_gpr(rt(code), result);
    }

    // ======== Arithmetic Register ========

    fn op_add(&mut self, bus: &mut Bus, code: u32) {
        let s = self.regs.gpr[rs(code)] as i32;
        let t = self.regs.gpr[rt(code)] as i32;
        match s.checked_add(t) {
            Some(result) => {
                self.cancel_delayed_load(rd(code) as u32);
                self.regs.set_gpr(rd(code), result as u32);
            }
            None => exceptions::exception(self, bus, exceptions::Exception::ArithmeticOverflow),
        }
    }

    fn op_addu(&mut self, code: u32) {
        let result = self.regs.gpr[rs(code)].wrapping_add(self.regs.gpr[rt(code)]);
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), result);
    }

    fn op_sub(&mut self, bus: &mut Bus, code: u32) {
        let s = self.regs.gpr[rs(code)] as i32;
        let t = self.regs.gpr[rt(code)] as i32;
        match s.checked_sub(t) {
            Some(result) => {
                self.cancel_delayed_load(rd(code) as u32);
                self.regs.set_gpr(rd(code), result as u32);
            }
            None => exceptions::exception(self, bus, exceptions::Exception::ArithmeticOverflow),
        }
    }

    fn op_subu(&mut self, code: u32) {
        let result = self.regs.gpr[rs(code)].wrapping_sub(self.regs.gpr[rt(code)]);
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), result);
    }

    fn op_and(&mut self, code: u32) {
        let result = self.regs.gpr[rs(code)] & self.regs.gpr[rt(code)];
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), result);
    }

    fn op_or(&mut self, code: u32) {
        let result = self.regs.gpr[rs(code)] | self.regs.gpr[rt(code)];
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), result);
    }

    fn op_xor(&mut self, code: u32) {
        let result = self.regs.gpr[rs(code)] ^ self.regs.gpr[rt(code)];
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), result);
    }

    fn op_nor(&mut self, code: u32) {
        let result = !(self.regs.gpr[rs(code)] | self.regs.gpr[rt(code)]);
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), result);
    }

    fn op_slt(&mut self, code: u32) {
        let result = (self.regs.gpr[rs(code)] as i32) < (self.regs.gpr[rt(code)] as i32);
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), result as u32);
    }

    fn op_sltu(&mut self, code: u32) {
        let result = self.regs.gpr[rs(code)] < self.regs.gpr[rt(code)];
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), result as u32);
    }

    // ======== Shift ========

    fn op_sll(&mut self, code: u32) {
        let result = self.regs.gpr[rt(code)] << sa(code);
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), result);
    }

    fn op_srl(&mut self, code: u32) {
        let result = self.regs.gpr[rt(code)] >> sa(code);
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), result);
    }

    fn op_sra(&mut self, code: u32) {
        let result = (self.regs.gpr[rt(code)] as i32) >> sa(code);
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), result as u32);
    }

    fn op_sllv(&mut self, code: u32) {
        let result = self.regs.gpr[rt(code)] << (self.regs.gpr[rs(code)] & 0x1F);
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), result);
    }

    fn op_srlv(&mut self, code: u32) {
        let result = self.regs.gpr[rt(code)] >> (self.regs.gpr[rs(code)] & 0x1F);
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), result);
    }

    fn op_srav(&mut self, code: u32) {
        let result = (self.regs.gpr[rt(code)] as i32) >> (self.regs.gpr[rs(code)] & 0x1F);
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), result as u32);
    }

    // ======== Multiply / Divide ========

    fn op_mult(&mut self, code: u32) {
        let result = (self.regs.gpr[rs(code)] as i32 as i64) * (self.regs.gpr[rt(code)] as i32 as i64);
        self.regs.lo = result as u32;
        self.regs.hi = (result >> 32) as u32;
    }

    fn op_multu(&mut self, code: u32) {
        let result = (self.regs.gpr[rs(code)] as u64) * (self.regs.gpr[rt(code)] as u64);
        self.regs.lo = result as u32;
        self.regs.hi = (result >> 32) as u32;
    }

    fn op_div(&mut self, code: u32) {
        let n = self.regs.gpr[rs(code)] as i32;
        let d = self.regs.gpr[rt(code)] as i32;
        if d == 0 {
            self.regs.lo = if n >= 0 { 0xFFFF_FFFF } else { 1 };
            self.regs.hi = n as u32;
        } else if n as u32 == 0x8000_0000 && d == -1 {
            self.regs.lo = 0x8000_0000;
            self.regs.hi = 0;
        } else {
            self.regs.lo = (n / d) as u32;
            self.regs.hi = (n % d) as u32;
        }
    }

    fn op_divu(&mut self, code: u32) {
        let n = self.regs.gpr[rs(code)];
        let d = self.regs.gpr[rt(code)];
        if d == 0 {
            self.regs.lo = 0xFFFF_FFFF;
            self.regs.hi = n;
        } else {
            self.regs.lo = n / d;
            self.regs.hi = n % d;
        }
    }

    fn op_mfhi(&mut self, code: u32) {
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), self.regs.hi);
    }

    fn op_mthi(&mut self, code: u32) {
        self.regs.hi = self.regs.gpr[rs(code)];
    }

    fn op_mflo(&mut self, code: u32) {
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), self.regs.lo);
    }

    fn op_mtlo(&mut self, code: u32) {
        self.regs.lo = self.regs.gpr[rs(code)];
    }

    // ======== Jump Register ========

    fn op_jr(&mut self, code: u32) {
        let target = self.regs.gpr[rs(code)];
        self.branch(target);
    }

    fn op_jalr(&mut self, code: u32) {
        let target = self.regs.gpr[rs(code)];
        self.cancel_delayed_load(rd(code) as u32);
        self.regs.set_gpr(rd(code), self.regs.pc);
        self.branch(target);
    }

    // ======== System ========

    fn op_syscall(&mut self, bus: &mut Bus) {
        exceptions::exception(self, bus, exceptions::Exception::Syscall);
    }

    fn op_break(&mut self, bus: &mut Bus) {
        exceptions::exception(self, bus, exceptions::Exception::Break);
    }

    // ======== Load ========

    fn op_lb(&mut self, bus: &mut Bus, code: u32) {
        let addr = self.regs.gpr[rs(code)].wrapping_add(imm_se(code));
        let val = bus.read8(addr) as i8 as i32 as u32;
        self.delayed_load(rt(code) as u32, val);
    }

    fn op_lbu(&mut self, bus: &mut Bus, code: u32) {
        let addr = self.regs.gpr[rs(code)].wrapping_add(imm_se(code));
        let val = bus.read8(addr) as u32;
        self.delayed_load(rt(code) as u32, val);
    }

    fn op_lh(&mut self, bus: &mut Bus, code: u32) {
        let addr = self.regs.gpr[rs(code)].wrapping_add(imm_se(code));
        if addr & 1 != 0 {
            self.regs.cp0[CP0_BADVADDR] = addr;
            exceptions::exception(self, bus, exceptions::Exception::LoadAddressError);
            return;
        }
        let val = bus.read16(addr) as i16 as i32 as u32;
        self.delayed_load(rt(code) as u32, val);
    }

    fn op_lhu(&mut self, bus: &mut Bus, code: u32) {
        let addr = self.regs.gpr[rs(code)].wrapping_add(imm_se(code));
        if addr & 1 != 0 {
            self.regs.cp0[CP0_BADVADDR] = addr;
            exceptions::exception(self, bus, exceptions::Exception::LoadAddressError);
            return;
        }
        let val = bus.read16(addr) as u32;
        self.delayed_load(rt(code) as u32, val);
    }

    fn op_lw(&mut self, bus: &mut Bus, code: u32) {
        let addr = self.regs.gpr[rs(code)].wrapping_add(imm_se(code));
        if addr & 3 != 0 {
            self.regs.cp0[CP0_BADVADDR] = addr;
            exceptions::exception(self, bus, exceptions::Exception::LoadAddressError);
            return;
        }
        let val = bus.read32(addr);
        self.delayed_load(rt(code) as u32, val);
    }

    fn op_lwl(&mut self, bus: &mut Bus, code: u32) {
        let addr = self.regs.gpr[rs(code)].wrapping_add(imm_se(code));
        let aligned = addr & !3;
        let mem = bus.read32(aligned);
        let rt_idx = rt(code);

        // Get current value (may come from pending delayed load)
        let cur = self.regs.gpr[rt_idx];

        let result = match addr & 3 {
            0 => (cur & 0x00FF_FFFF) | (mem << 24),
            1 => (cur & 0x0000_FFFF) | (mem << 16),
            2 => (cur & 0x0000_00FF) | (mem << 8),
            3 => mem,
            _ => unreachable!(),
        };
        let mask = match addr & 3 {
            0 => 0x00FF_FFFF,
            1 => 0x0000_FFFF,
            2 => 0x0000_00FF,
            3 => 0,
            _ => unreachable!(),
        };

        self.delayed_load_masked(rt_idx as u32, result & !mask, mask);
    }

    fn op_lwr(&mut self, bus: &mut Bus, code: u32) {
        let addr = self.regs.gpr[rs(code)].wrapping_add(imm_se(code));
        let aligned = addr & !3;
        let mem = bus.read32(aligned);
        let rt_idx = rt(code);

        let cur = self.regs.gpr[rt_idx];

        let result = match addr & 3 {
            0 => mem,
            1 => (cur & 0xFF00_0000) | (mem >> 8),
            2 => (cur & 0xFFFF_0000) | (mem >> 16),
            3 => (cur & 0xFFFF_FF00) | (mem >> 24),
            _ => unreachable!(),
        };
        let mask = match addr & 3 {
            0 => 0,
            1 => 0xFF00_0000,
            2 => 0xFFFF_0000,
            3 => 0xFFFF_FF00,
            _ => unreachable!(),
        };

        self.delayed_load_masked(rt_idx as u32, result & !mask, mask);
    }

    // ======== Store ========

    fn op_sb(&mut self, bus: &mut Bus, code: u32) {
        if self.regs.cp0[CP0_STATUS] & 0x10000 != 0 { return; } // Cache isolation
        let addr = self.regs.gpr[rs(code)].wrapping_add(imm_se(code));
        bus.write8(addr, self.regs.gpr[rt(code)] as u8);
    }

    fn op_sh(&mut self, bus: &mut Bus, code: u32) {
        if self.regs.cp0[CP0_STATUS] & 0x10000 != 0 { return; }
        let addr = self.regs.gpr[rs(code)].wrapping_add(imm_se(code));
        if addr & 1 != 0 {
            self.regs.cp0[CP0_BADVADDR] = addr;
            exceptions::exception(self, bus, exceptions::Exception::StoreAddressError);
            return;
        }
        bus.write16(addr, self.regs.gpr[rt(code)] as u16);
    }

    fn op_sw(&mut self, bus: &mut Bus, code: u32) {
        if self.regs.cp0[CP0_STATUS] & 0x10000 != 0 { return; } // Cache isolation — drop write
        let addr = self.regs.gpr[rs(code)].wrapping_add(imm_se(code));
        if addr & 3 != 0 {
            self.regs.cp0[CP0_BADVADDR] = addr;
            exceptions::exception(self, bus, exceptions::Exception::StoreAddressError);
            return;
        }
        bus.write32(addr, self.regs.gpr[rt(code)]);
    }

    fn op_swl(&mut self, bus: &mut Bus, code: u32) {
        if self.regs.cp0[CP0_STATUS] & 0x10000 != 0 { return; }
        let addr = self.regs.gpr[rs(code)].wrapping_add(imm_se(code));
        let aligned = addr & !3;
        let mem = bus.read32(aligned);
        let val = self.regs.gpr[rt(code)];

        let result = match addr & 3 {
            0 => (mem & 0xFFFF_FF00) | (val >> 24),
            1 => (mem & 0xFFFF_0000) | (val >> 16),
            2 => (mem & 0xFF00_0000) | (val >> 8),
            3 => val,
            _ => unreachable!(),
        };
        bus.write32(aligned, result);
    }

    fn op_swr(&mut self, bus: &mut Bus, code: u32) {
        if self.regs.cp0[CP0_STATUS] & 0x10000 != 0 { return; }
        let addr = self.regs.gpr[rs(code)].wrapping_add(imm_se(code));
        let aligned = addr & !3;
        let mem = bus.read32(aligned);
        let val = self.regs.gpr[rt(code)];

        let result = match addr & 3 {
            0 => val,
            1 => (mem & 0x0000_00FF) | (val << 8),
            2 => (mem & 0x0000_FFFF) | (val << 16),
            3 => (mem & 0x00FF_FFFF) | (val << 24),
            _ => unreachable!(),
        };
        bus.write32(aligned, result);
    }

    // ======== COP2 Load/Store ========

    fn op_lwc2(&mut self, bus: &mut Bus, code: u32) {
        let addr = self.regs.gpr[rs(code)].wrapping_add(imm_se(code));
        if addr & 3 != 0 {
            self.regs.cp0[CP0_BADVADDR] = addr;
            exceptions::exception(self, bus, exceptions::Exception::LoadAddressError);
            return;
        }
        let val = bus.read32(addr);
        self.regs.cp2d[rt(code)] = val;
    }

    fn op_swc2(&mut self, bus: &mut Bus, code: u32) {
        let addr = self.regs.gpr[rs(code)].wrapping_add(imm_se(code));
        if addr & 3 != 0 {
            self.regs.cp0[CP0_BADVADDR] = addr;
            exceptions::exception(self, bus, exceptions::Exception::StoreAddressError);
            return;
        }
        let val = self.regs.cp2d[rt(code)];
        bus.write32(addr, val);
    }
}
