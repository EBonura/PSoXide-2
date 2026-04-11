pub mod exceptions;
pub mod icache;
pub mod interpreter;
pub mod registers;

use crate::bus::Bus;
use registers::Registers;
use std::sync::atomic::{AtomicU64, Ordering};

// Heartbeat: next cycle threshold for periodic status dump
static HEARTBEAT_NEXT: AtomicU64 = AtomicU64::new(100_000_000);

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
        bus.diag_cpu_pc = pc;

        // DIAG (code identity): the first time pc enters 0x80042018..0x80042348,
        // compare live RAM against the BIOS ROM source at BFC2A018..BFC2A348
        // (shell text sequence match already confirmed statically).
        {
            use std::sync::atomic::{AtomicBool, Ordering};
            static DONE_FIRST: AtomicBool = AtomicBool::new(false);
            if (0x80042018..0x80042348).contains(&pc)
                && !DONE_FIRST.swap(true, Ordering::Relaxed)
            {
                eprintln!("FIRST_EXEC_IN_BLOCK: pc={:08X} cyc={} ra={:08X}",
                    pc, self.regs.cycle, self.regs.gpr[31]);
                eprintln!("=== RAM vs ROM 0x80042018..0x80042348 (first execution) ===");
                let mut mismatches = 0u32;
                let words = ((0x42348u32 - 0x42018u32) / 4) as usize;
                for i in 0..words {
                    let ram_off = 0x42018usize + i * 4;
                    let rom_off = 0x2A018usize + i * 4;
                    let ram_w = u32::from_le_bytes([
                        bus.ram[ram_off], bus.ram[ram_off+1],
                        bus.ram[ram_off+2], bus.ram[ram_off+3]]);
                    let rom_w = u32::from_le_bytes([
                        bus.bios[rom_off], bus.bios[rom_off+1],
                        bus.bios[rom_off+2], bus.bios[rom_off+3]]);
                    if ram_w != rom_w {
                        if mismatches < 32 {
                            eprintln!("  {:08X} RAM={:08X} ROM={:08X}",
                                0x80042018u32 + (i as u32) * 4, ram_w, rom_w);
                        }
                        mismatches += 1;
                    }
                }
                eprintln!("  mismatches first-exec: {} / {}", mismatches, words);
            }
        }

        // DIAG (code identity at stuck point): repeat RAM-vs-ROM compare at
        // pc==0x800422C8 to prove whether the code drifted between first
        // execution and stuck time.
        if pc == 0x800422C8 {
            use std::sync::atomic::{AtomicBool, Ordering};
            static DONE_STUCK: AtomicBool = AtomicBool::new(false);
            if !DONE_STUCK.swap(true, Ordering::Relaxed) {
                eprintln!("=== RAM vs ROM 0x80042018..0x80042348 (stuck point) ===");
                let mut mismatches = 0u32;
                let words = ((0x42348u32 - 0x42018u32) / 4) as usize;
                for i in 0..words {
                    let ram_off = 0x42018usize + i * 4;
                    let rom_off = 0x2A018usize + i * 4;
                    let ram_w = u32::from_le_bytes([
                        bus.ram[ram_off], bus.ram[ram_off+1],
                        bus.ram[ram_off+2], bus.ram[ram_off+3]]);
                    let rom_w = u32::from_le_bytes([
                        bus.bios[rom_off], bus.bios[rom_off+1],
                        bus.bios[rom_off+2], bus.bios[rom_off+3]]);
                    if ram_w != rom_w {
                        if mismatches < 32 {
                            eprintln!("  {:08X} RAM={:08X} ROM={:08X}",
                                0x80042018u32 + (i as u32) * 4, ram_w, rom_w);
                        }
                        mismatches += 1;
                    }
                }
                eprintln!("  mismatches stuck-pt: {} / {}", mismatches, words);
            }
        }

        // DIAG (bad s1 writer): trace the FIRST instruction that produces a
        // clearly-bogus loop-limit value in s1 (r17). We consider any value
        // with all 4 bytes non-zero AND >= 0x10000000 to be bad (normal loop
        // counts are well below 0x100).
        {
            use std::sync::atomic::{AtomicBool, Ordering};
            static DETECTED_S1: AtomicBool = AtomicBool::new(false);
            if !DETECTED_S1.load(Ordering::Relaxed) {
                let s1 = self.regs.gpr[17];
                if s1 >= 0x1000_0000
                    && (s1 & 0xFF) != 0 && ((s1 >> 8) & 0xFF) != 0
                    && ((s1 >> 16) & 0xFF) != 0 && ((s1 >> 24) & 0xFF) != 0
                {
                    DETECTED_S1.store(true, Ordering::Relaxed);
                    let writer_pc = pc.wrapping_sub(4);
                    let writer_instr = bus.read32(writer_pc);
                    eprintln!("FIRST_BAD_S1: s1={:08X} cyc={} next_pc={:08X} writer_pc={:08X} writer_instr={:08X}",
                        s1, self.regs.cycle, pc, writer_pc, writer_instr);
                    eprintln!("  writer context:");
                    for i in 0..8u32 {
                        let a = writer_pc.wrapping_sub(16).wrapping_add(i * 4);
                        let v = bus.read32(a);
                        let tag = if a == writer_pc { " <<< writer" } else { "" };
                        eprintln!("    {:08X}: {:08X}{}", a, v, tag);
                    }
                    eprintln!("  regs: ra={:08X} sp={:08X} s0={:08X} a0={:08X} a1={:08X} v0={:08X} v1={:08X}",
                        self.regs.gpr[31], self.regs.gpr[29], self.regs.gpr[16],
                        self.regs.gpr[4], self.regs.gpr[5], self.regs.gpr[2], self.regs.gpr[3]);
                }
            }
        }

        // DIAG: record the FIRST pc observed in the 0x80042000..0x80043000
        // range and capture how we got there. Also trace the JAL site and
        // its delay slot to see whether the branch target is actually
        // being fetched.
        {
            use std::sync::atomic::{AtomicU32, Ordering};
            static REGION_FIRST_PC: AtomicU32 = AtomicU32::new(0);
            if (0x80042000..0x80043000).contains(&pc)
                && REGION_FIRST_PC.load(Ordering::Relaxed) == 0
            {
                REGION_FIRST_PC.store(pc, Ordering::Relaxed);
                eprintln!("FIRST_ENTRY_INTO_BLOCK: pc={:08X} cyc={} ra={:08X} a0={:08X} s0={:08X}",
                    pc, self.regs.cycle,
                    self.regs.gpr[31], self.regs.gpr[4], self.regs.gpr[16]);
            }
            if pc == 0x80041B28 {
                static JAL_HITS: AtomicU32 = AtomicU32::new(0);
                let n = JAL_HITS.fetch_add(1, Ordering::Relaxed);
                if n < 3 {
                    eprintln!("JAL_0x80041B28 #{} cyc={} a0={:08X}",
                        n, self.regs.cycle, self.regs.gpr[4]);
                }
            }
            if pc == 0x80041B2C {
                static DS_HITS: AtomicU32 = AtomicU32::new(0);
                let n = DS_HITS.fetch_add(1, Ordering::Relaxed);
                if n < 3 {
                    eprintln!("DELAY_0x80041B2C #{} cyc={} (delay slot of JAL)", n, self.regs.cycle);
                }
            }
            if pc == 0x800421E8 {
                static E_HITS: AtomicU32 = AtomicU32::new(0);
                let n = E_HITS.fetch_add(1, Ordering::Relaxed);
                if n < 3 {
                    eprintln!("ENTRY_0x800421E8 #{} cyc={} ra={:08X}",
                        n, self.regs.cycle, self.regs.gpr[31]);
                }
            }
        }

        // DIAG: one-shot at stuck PC — dump live register state plus the
        // exact input pointers the function is reading from. The function
        // reads three independent pointers (s2,s3,s4), walks them with
        // stride 0x14, averages halfwords three at a time, and writes the
        // result to *v0. The outer loop at 800422E0..800422F0 AND at
        // 800422CC..800422D4 are the inner/outer iterators.
        if pc == 0x800422C8 {
            use std::sync::atomic::{AtomicBool, Ordering};
            static DONE: AtomicBool = AtomicBool::new(false);
            if !DONE.swap(true, Ordering::Relaxed) {
                eprintln!("\n=== RAM CONTENTS 0x800422A0..0x80042300 (stuck range) ===");
                for i in 0..24u32 {
                    let a = 0x800422A0u32 + i * 4;
                    let v = bus.read32(a);
                    let tag = match a {
                        0x800422C8 => " <<< pc(1)",
                        0x800422D8 => " <<< pc(2)",
                        0x800422DC => " <<< pc(3)",
                        0x800422E0 => " <<< pc(4)",
                        _ => "",
                    };
                    eprintln!("  {:08X}: {:08X}{}", a, v, tag);
                }
                eprintln!("\n=== RAM CONTENTS 0x80042040..0x800420B0 (loop head area) ===");
                for i in 0..28u32 {
                    let a = 0x80042040u32 + i * 4;
                    let v = bus.read32(a);
                    eprintln!("  {:08X}: {:08X}", a, v);
                }
                eprintln!("\n=== RAM CONTENTS near 0x80043428 (caller of 0x80042CD0) ===");
                for i in 0..16u32 {
                    let a = 0x80043418u32 + i * 4;
                    let v = bus.read32(a);
                    let tag = if a == 0x8004342C { " <<< JAL site" } else { "" };
                    eprintln!("  {:08X}: {:08X}{}", a, v, tag);
                }
                // Input buffer the stuck function reads from: is it truly empty?
                eprintln!("\n=== INPUT BUFFER 0x80010000..0x80010200 (all zero?) ===");
                let mut nonzero = 0u32;
                for i in 0..128u32 {
                    let a = 0x80010000u32 + i * 4;
                    let v = bus.read32(a);
                    if v != 0 {
                        nonzero += 1;
                        eprintln!("  {:08X}: {:08X}", a, v);
                    }
                }
                eprintln!("  Nonzero words in 0x80010000..0x80010200: {}", nonzero);
                let r2  = self.regs.gpr[2];
                let r3  = self.regs.gpr[3];
                let r4  = self.regs.gpr[4];
                let s0  = self.regs.gpr[16];
                let s2  = self.regs.gpr[18];
                let s3  = self.regs.gpr[19];
                let s4  = self.regs.gpr[20];
                let s5  = self.regs.gpr[21];
                let s6  = self.regs.gpr[22];
                let s7  = self.regs.gpr[23];
                let t2  = self.regs.gpr[10];
                let t8  = self.regs.gpr[24];
                eprintln!("\n=== STUCK LOOP ACTUAL REGS @ cyc={} ===", self.regs.cycle);
                eprintln!("  v0/r2  = {:08X}   (output buffer ptr)", r2);
                eprintln!("  v1/r3  = {:08X}", r3);
                eprintln!("  a0/r4  = {:08X}   (input record ptr, stride 0x14)", r4);
                eprintln!("  s0     = {:08X}", s0);
                eprintln!("  s2     = {:08X}", s2);
                eprintln!("  s3     = {:08X}", s3);
                eprintln!("  s4     = {:08X}", s4);
                eprintln!("  s5     = {:08X}   (input source hi halfword)", s5);
                eprintln!("  s6     = {:08X}", s6);
                eprintln!("  s7     = {:08X}", s7);
                eprintln!("  t2/r10 = {:08X}", t2);
                eprintln!("  t8/r24 = {:08X}", t8);
                // The caller passed a0 = *(0x80089ED8) at 80041B24
                let g_arg = bus.read32(0x80089ED8);
                eprintln!("  *(0x80089ED8) = {:08X}   (function input arg from caller)", g_arg);
                // What's at s2/s3/s4 if they're valid?
                for (name, r) in &[("s2", s2), ("s3", s3), ("s4", s4), ("r2", r2), ("r3", r3), ("t2", t2)] {
                    let phys = r & 0x1FFFFFFF;
                    if phys < 0x00200000 {
                        eprintln!("  *({}={:08X}) first 4 words:", name, r);
                        for i in 0..4u32 {
                            let v = bus.read32(r.wrapping_add(i*4));
                            eprintln!("      +{:02X}: {:08X}", i*4, v);
                        }
                    }
                }
            }
        }

        // DIAG: EvCB scanner audit. The BIOS loop at 0x80042160..0x800422F4 is
        // the event dispatcher (HandleEvents-style). It iterates over the EvCB
        // table at 0x800DFEE0 and for each pending entry, ANDs the status
        // halfwords with the pending mask at *(0x800DFF0C) and jalrs the
        // slot handler. If the mask bit is never cleared, the outer loop
        // at BFC422F0 repeats forever.
        //
        // Fire once when the inner loop reaches 0x800422C8 (confirmed by
        // the heartbeat as the loop tail) and dump the structures it scans.
        if pc == 0x800422C8 {
            use std::sync::atomic::{AtomicU32, Ordering};
            static HITS: AtomicU32 = AtomicU32::new(0);
            let n = HITS.fetch_add(1, Ordering::Relaxed);
            if n < 2 {
                eprintln!("\n=== EvCB SCANNER STATE @ cyc={} ===", self.regs.cycle);
                eprintln!("  pc=800422C8 (inside HandleEvents inner loop)");
                eprintln!("  s1={:08X} s3={:08X} r16={:08X} (loop counter / limit / mask)",
                    self.regs.gpr[17], self.regs.gpr[19], self.regs.gpr[16]);
                eprintln!("  r18={:08X} r20={:08X}", self.regs.gpr[18], self.regs.gpr[20]);
                // Kernel globals read by the loop
                let g_count    = bus.read32(0x80089D48);
                let g_scanflag = bus.read32(0x80089D4C);
                let g_tbl_ptr  = bus.read32(0x80089D50);
                let g_tbl2_ptr = bus.read32(0x80089D54);
                let pending    = bus.read32(0x800DFF0C);
                eprintln!("  *(0x80089D48) count   = {:08X}", g_count);
                eprintln!("  *(0x80089D4C) scan    = {:08X}", g_scanflag);
                eprintln!("  *(0x80089D50) tbl     = {:08X}", g_tbl_ptr);
                eprintln!("  *(0x80089D54) tbl2    = {:08X}", g_tbl2_ptr);
                eprintln!("  *(0x800DFF0C) pending = {:08X}  <-- loop continues while nonzero", pending);
                // EvCB table starts at 0x800DFEE0, stride 0x1C, count limit from
                // BFC42164: r14 = *(0x80089D48). Dump the first 16 entries.
                eprintln!("  EvCB table @ 0x800DFEE0 (stride 0x1C):");
                for i in 0..16u32 {
                    let base = 0x800DFEE0u32 + i * 0x1C;
                    // EvCB layout (from real PS1 BIOS):
                    //   +0x00 class (e.g. 0xF0000001 = VBlank)
                    //   +0x04 status (1=disabled, 2=enabled, 4=pending, 0=free)
                    //   +0x08 spec
                    //   +0x0C mode
                    //   +0x10 handler pointer
                    //   +0x14..0x1C reserved
                    let class  = bus.read32(base);
                    let status = bus.read32(base + 0x04);
                    let spec   = bus.read32(base + 0x08);
                    let mode   = bus.read32(base + 0x0C);
                    let handler = bus.read32(base + 0x10);
                    eprintln!("    [{:2}] @{:08X}: class={:08X} stat={:08X} spec={:08X} mode={:08X} handler={:08X}",
                        i, base, class, status, spec, mode, handler);
                }
            }
        }

        // Heartbeat every 100M cycles
        {
            let nb = HEARTBEAT_NEXT.load(Ordering::Relaxed);
            if self.regs.cycle >= nb {
                HEARTBEAT_NEXT.store(nb + 100_000_000, Ordering::Relaxed);
                let imask = bus.read_imask();
                let istat = bus.read_istat();
                let status = self.regs.cp0[registers::CP0_STATUS];
                let cause = self.regs.cp0[registers::CP0_CAUSE];
                let badvaddr = self.regs.cp0[registers::CP0_BADVADDR];
                // DIAG: loop state for 0x80042050+ scanner
                let a0 = self.regs.gpr[4];
                let t1 = self.regs.gpr[9];
                let s1 = self.regs.gpr[17];
                let v0 = self.regs.gpr[2];
                let a0_val = if (a0 & 0x1FFF_FFFF) < 0x00200000 { bus.read32(a0) } else { 0xDEAD };
                // DIAG: also read directly from bus.ram[] slice to bypass any routing
                let a0_phys = (a0 & 0x1FFF_FFFF) as usize;
                let raw_slice = if a0_phys + 4 <= bus.ram.len() {
                    u32::from_le_bytes([bus.ram[a0_phys], bus.ram[a0_phys+1], bus.ram[a0_phys+2], bus.ram[a0_phys+3]])
                } else { 0xDEADBEEF };
                eprintln!("HEARTBEAT: cyc={} pc={:08X} IMASK={:04X} ISTAT={:04X} status={:08X} cause={:08X} epc={:08X} badvaddr={:08X} a0={:08X} a0_val={:08X} raw_slice={:08X} t1={:08X} s1={:08X} v0={:08X}",
                    self.regs.cycle, pc, imask, istat, status, cause,
                    self.regs.cp0[registers::CP0_EPC], badvaddr,
                    a0, a0_val, raw_slice, t1, s1, v0);
                // DIAG: one-shot dump of mesh buffer at 0x10000-0x10040 and caller header
                static DUMPED_BUF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                if !DUMPED_BUF.swap(true, Ordering::Relaxed) {
                    eprintln!("  MESH BUF @ 0x80010000..+0x40:");
                    for i in 0..16u32 {
                        let addr = 0x80010000u32 + i * 4;
                        eprintln!("    {:08X}: {:08X}", addr, bus.read32(addr));
                    }
                    eprintln!("  caller header (a0-40..a0) = 0xA001xxxx:");
                    let base = a0.wrapping_sub(40);
                    for i in 0..10u32 {
                        let addr = base.wrapping_add(i * 4);
                        eprintln!("    {:08X}: {:08X}", addr, bus.read32(addr));
                    }
                }
                // One-shot: dump instructions and registers at stuck PC
                static DUMPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                if !DUMPED.load(Ordering::Relaxed) {
                    DUMPED.store(true, Ordering::Relaxed);
                    eprintln!("  INSTR DUMP around PC={:08X}:", pc);
                    for i in 0..16u32 {
                        let addr = (pc & !3).wrapping_sub(8).wrapping_add(i * 4);
                        let instr = bus.read32(addr);
                        let marker = if addr == pc { " <<< PC" } else { "" };
                        eprintln!("    {:08X}: {:08X}{}", addr, instr, marker);
                    }
                    // Dump all saved registers
                    eprintln!("  REGS: s0={:08X} s1={:08X} s2={:08X} s3={:08X} s4={:08X} s5={:08X} s6={:08X} s7={:08X}",
                        self.regs.gpr[16], self.regs.gpr[17], self.regs.gpr[18], self.regs.gpr[19],
                        self.regs.gpr[20], self.regs.gpr[21], self.regs.gpr[22], self.regs.gpr[23]);
                    eprintln!("  REGS: a0={:08X} a1={:08X} v0={:08X} v1={:08X} ra={:08X} sp={:08X} k0={:08X} k1={:08X}",
                        self.regs.gpr[4], self.regs.gpr[5], self.regs.gpr[2], self.regs.gpr[3],
                        self.regs.gpr[31], self.regs.gpr[29], self.regs.gpr[26], self.regs.gpr[27]);
                    // Dump exception handler at 0x80
                    eprintln!("  EXCEPTION HANDLER at 0x80:");
                    for i in 0..8u32 {
                        let addr = 0x80 + i * 4;
                        eprintln!("    {:08X}: {:08X}", addr, bus.read32(addr));
                    }
                    // SIO state dump
                    eprintln!("  SIO: {}", bus.sio.debug_dump());
                    // Dump code at EPC (faulting instruction)
                    let epc = self.regs.cp0[registers::CP0_EPC];
                    eprintln!("  CODE at EPC={:08X}:", epc);
                    for i in 0..8u32 {
                        let addr = epc.wrapping_sub(8).wrapping_add(i * 4);
                        let instr = bus.read32(addr);
                        let marker = if addr == epc { " <<< EPC" } else { "" };
                        eprintln!("    {:08X}: {:08X}{}", addr, instr, marker);
                    }
                    // Also dump 0x1540-0x15B0 (game's exception handler body)
                    eprintln!("  HANDLER at 0x1540:");
                    for i in 0..28u32 {
                        let addr = 0x80001540 + i * 4;
                        eprintln!("    {:08X}: {:08X}", addr, bus.read32(addr));
                    }
                }
            }
        }

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

    fn intercept_bios(&mut self, bus: &mut Bus) {
        let pc = self.regs.pc;
        let call = self.regs.gpr[9] & 0xFF; // t1 = function number

        // Fast boot: hijack at the shell entry point (PC == 0x80030000).
        //
        // This is the canonical "shell reached" hook used by pcsx-redux
        // (src/core/ui.cc shellReached(), src/core/DynaRec_*/recompiler.cc
        // at m_pc == 0x80030000). The retail BIOS loads the shell from ROM
        // and jalrs into it — the code that prepares $a0 for that call is
        // at BFC07010 (`lui $a0, 0x8003`). By this point the BIOS kernel
        // has fully initialized: C0(07) InstallExcHandlers installed the
        // exception dispatch at 0x80000080 -> 0x0C80, C0(12) InstallDevices
        // ran, the A/B/C dispatchers at 0xA0/0xB0/0xC0 are in place, events
        // are open, IMASK is configured. This is the faithful post-kernel
        // handoff point — much closer to what the BIOS's own loadAndExec/
        // exec() produces than intercepting a mid-init syscall.
        //
        // Fast boot is OPT-IN via `PSOXIDE_FAST_BOOT=1`. The default is
        // native boot, which lets the BIOS shell run unmodified. Without
        // the env var set, this hook is completely inert and cannot
        // preempt the native path.
        //
        // IMPORTANT: do NOT trigger on A0(0x72). Per psx-spx kernelbios
        // docs, A0(0x54)/A0(0x71) is `_96_init` and A0(0x56)/A0(0x72) is
        // `_96_remove` — CD subsystem bring-up and teardown respectively.
        // These are ordinary CD-subsystem calls and carry no meaning for
        // "the game is ready to run". The retail shell calls _96_remove
        // during its own early init.
        if pc == 0x80030000 && bus.cdrom.has_disc() {
            use std::sync::atomic::{AtomicBool, Ordering};
            static FAST_BOOT_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            let enabled = *FAST_BOOT_ENABLED.get_or_init(|| {
                std::env::var("PSOXIDE_FAST_BOOT").map(|v| v == "1").unwrap_or(false)
            });
            if enabled {
                static FAST_BOOTED: AtomicBool = AtomicBool::new(false);
                if !FAST_BOOTED.swap(true, Ordering::Relaxed) {
                    if let Some((entry_pc, gp, sp)) = bus.fast_boot() {
                        eprintln!(
                            "FAST_BOOT: shell entry reached, jumping to game PC={:08X} GP={:08X} SP={:08X}",
                            entry_pc, gp, sp
                        );
                        // Set GP, SP, FP — matching exec() in psxexec.s
                        self.regs.pc = entry_pc;
                        if gp != 0 {
                            self.regs.gpr[28] = gp;
                        }
                        self.regs.gpr[29] = sp;
                        self.regs.gpr[30] = sp; // FP = SP
                        // Preserve BIOS Status register — do NOT override.
                        // Only ensure CU2 (GTE, bit 30) is usable since games need it.
                        // The BIOS at shell-entry time has a well-defined Status;
                        // the game's own enterCriticalSection/leaveCriticalSection
                        // will manage IEc as its init progresses.
                        self.regs.cp0[registers::CP0_STATUS] |= 0x4000_0000; // CU2
                        // Clear delay slots — we are short-circuiting the jalr
                        // that was about to enter the shell.
                        self.in_delay_slot = false;
                        self.next_is_delay_slot = false;
                        self.delayed_load = [DelayedLoadSlot::default(); 2];
                        return;
                    }
                }
            }
        }

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
                    0x40 => {
                        // SystemErrorUnresolvedException — diagnose the loop
                        use std::sync::atomic::{AtomicU32, Ordering};
                        static N40: AtomicU32 = AtomicU32::new(0);
                        let n = N40.fetch_add(1, Ordering::Relaxed);
                        if n < 8 {
                            let cause = self.regs.cp0[registers::CP0_CAUSE];
                            let epc = self.regs.cp0[registers::CP0_EPC];
                            let excode = (cause >> 2) & 0x1F;
                            let bd = (cause >> 31) & 1;
                            let status = self.regs.cp0[registers::CP0_STATUS];
                            let instr = bus.read32(epc);
                            let instr_next = bus.read32(epc.wrapping_add(4));
                            eprintln!("A0(40) SysErr #{}: ExCode={} Cause={:08X} EPC={:08X} BD={} Status={:08X}",
                                n, excode, cause, epc, bd, status);
                            eprintln!("  instr@EPC={:08X} next={:08X} ra={:08X} sp={:08X} t9={:08X}",
                                instr, instr_next, self.regs.gpr[31], self.regs.gpr[29], self.regs.gpr[25]);
                            eprintln!("  a0={:08X} a1={:08X} a2={:08X} a3={:08X} v0={:08X}",
                                self.regs.gpr[4], self.regs.gpr[5], self.regs.gpr[6],
                                self.regs.gpr[7], self.regs.gpr[2]);
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
    /// Only checks SW interrupts (Cause bits 8-9). HW interrupts are checked
    /// exclusively in branch_test(), which runs after delayed PC loads are
    /// resolved — matching pcsx-redux exactly.
    pub fn test_sw_ints(&mut self, _bus: &mut Bus) {
        if self.regs.cp0[registers::CP0_CAUSE] & self.regs.cp0[registers::CP0_STATUS] & 0x0300 != 0
            && self.regs.cp0[registers::CP0_STATUS] & 0x1 != 0
        {
            let in_delay_slot = self.in_delay_slot;
            self.in_delay_slot = false;
            exceptions::exception_raw(self, self.regs.cp0[registers::CP0_CAUSE], in_delay_slot);
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
