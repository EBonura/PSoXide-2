// COP0 register indices
pub const CP0_INDEX: usize = 0;
pub const CP0_RANDOM: usize = 1;
pub const CP0_ENTRYLO: usize = 2;
pub const CP0_BPC: usize = 3;
pub const CP0_CONTEXT: usize = 4;
pub const CP0_BDA: usize = 5;
pub const CP0_PIDMASK: usize = 6;
pub const CP0_DCIC: usize = 7;
pub const CP0_BADVADDR: usize = 8;
pub const CP0_BDAM: usize = 9;
pub const CP0_ENTRYHI: usize = 10;
pub const CP0_BPCM: usize = 11;
pub const CP0_STATUS: usize = 12;
pub const CP0_CAUSE: usize = 13;
pub const CP0_EPC: usize = 14;
pub const CP0_PRID: usize = 15;

// GPR names for debugging
pub const GPR_NAMES: [&str; 32] = [
    "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3",
    "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7",
    "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7",
    "t8", "t9", "k0", "k1", "gp", "sp", "fp", "ra",
];

#[derive(Clone)]
pub struct Registers {
    pub gpr: [u32; 32],  // General purpose (R0 always 0)
    pub hi: u32,
    pub lo: u32,
    pub pc: u32,
    pub cp0: [u32; 32],  // COP0
    pub cp2d: [u32; 32], // COP2 data (GTE)
    pub cp2c: [u32; 32], // COP2 control (GTE)
    pub cycle: u64,
    pub current_instruction: u32,
}

impl Registers {
    pub fn new() -> Self {
        Self {
            gpr: [0; 32],
            hi: 0,
            lo: 0,
            pc: 0,
            cp0: [0; 32],
            cp2d: [0; 32],
            cp2c: [0; 32],
            cycle: 0,
            current_instruction: 0,
        }
    }

    /// Write to GPR, enforcing R0 = 0
    #[inline(always)]
    pub fn set_gpr(&mut self, index: usize, value: u32) {
        if index != 0 {
            self.gpr[index] = value;
        }
    }
}
