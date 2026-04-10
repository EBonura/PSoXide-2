use crate::bus::Bus;
use super::Cpu;

/// I-cache implementation.
///
/// TODO: Proper icache with invalidation on RAM writes.
/// For now, disabled (always reads from bus) to ensure correctness.
/// Redux invalidates icache on every RAM write via Clear(), which requires
/// the Bus to have a reference back to the CPU. We'll add that later.
impl Cpu {
    pub fn read_icache(&mut self, pc: u32, bus: &mut Bus) -> u32 {
        // Bypass cache for correctness — always read from bus
        bus.read32(pc)
    }

    /// Write to icache (used during cache isolation mode — Status bit 16)
    pub fn write_icache(&mut self, addr: u32, value: u32) {
        let cache_idx = ((addr & 0xFFF) >> 2) as usize;
        self.icache_addr[cache_idx] = addr & 0x00FF_FFFF;
        self.icache_code[cache_idx] = value;
    }

    /// Invalidate a cache line
    pub fn invalidate_icache_line(&mut self, addr: u32) {
        let line_cache = (addr & 0xFFF) as usize & !0xF;
        for i in 0..4usize {
            self.icache_addr[(line_cache >> 2) + i] = 0xFFFF_FFFF;
        }
    }
}
