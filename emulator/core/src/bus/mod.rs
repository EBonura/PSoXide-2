pub mod dma;

use crate::scheduler::Scheduler;
use crate::sio::Sio;
use crate::timers::Timers;
use cdrom::CdRom;
use gpu::Gpu;
use spu::Spu;
use std::path::Path;

pub struct Bus {
    pub ram: Box<[u8; 0x0020_0000]>,      // 2MB
    pub bios: Box<[u8; 0x0008_0000]>,     // 512KB
    pub scratchpad: Box<[u8; 0x400]>,     // 1KB
    pub hw_regs: Box<[u8; 0x1_0000]>,     // 64KB hardware register backing
    pub gpu: Gpu,
    pub spu: Spu,
    pub cdrom: CdRom,
    pub timers: Timers,
    pub scheduler: Scheduler,
    pub sio: Sio,
    pub dma: dma::DmaController,
    pub last_cycle: u64,
    pub diag_cpu_pc: u32,
}

impl Bus {
    pub fn new() -> Self {
        // Reset state: all hardware registers zero. The real PS1 powers up
        // with IMASK=0 and ISTAT=0; the BIOS configures them during init.
        // (Previous versions pre-set IMASK bit 0 as a WaitEvent workaround;
        // empirical testing confirmed it was inert — the BIOS writes IMASK=0
        // at BFC06894 very early, overwriting any preseed.)
        let hw_regs: Box<[u8; 0x1_0000]> =
            vec![0u8; 0x1_0000].into_boxed_slice().try_into().unwrap();
        Self {
            ram: vec![0u8; 0x0020_0000].into_boxed_slice().try_into().unwrap(),
            bios: vec![0u8; 0x0008_0000].into_boxed_slice().try_into().unwrap(),
            scratchpad: vec![0u8; 0x400].into_boxed_slice().try_into().unwrap(),
            hw_regs,
            gpu: Gpu::new(),
            spu: Spu::new(),
            cdrom: CdRom::new(),
            timers: Timers::new(),
            scheduler: Scheduler::new(),
            sio: Sio::new(),
            dma: dma::DmaController::new(),
            last_cycle: 0,
            diag_cpu_pc: 0,
        }
    }

    pub fn load_bios(&mut self, path: &Path) -> anyhow::Result<()> {
        let data = std::fs::read(path)?;
        if data.len() != 0x0008_0000 {
            anyhow::bail!("BIOS file must be exactly 512KB, got {} bytes", data.len());
        }
        self.bios[..].copy_from_slice(&data);
        tracing::info!("BIOS loaded: {} bytes", data.len());
        Ok(())
    }

    pub fn load_disc(&mut self, path: &Path) -> anyhow::Result<()> {
        self.cdrom.load_disc(path).map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(())
    }

    /// Fast boot: read SYSTEM.CNF from disc, load the PS-X EXE into RAM,
    /// and return (entry_pc, gp, sp) for the CPU to jump to.
    /// Returns None if no disc or parsing fails.
    pub fn fast_boot(&mut self) -> Option<(u32, u32, u32)> {
        if !self.cdrom.has_disc() { return None; }

        // Read Primary Volume Descriptor at LBA 16
        let pvd = self.cdrom.read_sector_data(16)?;
        if pvd[0] != 1 || &pvd[1..6] != b"CD001" {
            eprintln!("FAST_BOOT: bad PVD signature");
            return None;
        }

        // Root directory entry is at PVD offset 156, length 34
        let root_lba = u32::from_le_bytes([pvd[158], pvd[159], pvd[160], pvd[161]]);
        let root_size = u32::from_le_bytes([pvd[166], pvd[167], pvd[168], pvd[169]]);
        eprintln!("FAST_BOOT: root dir LBA={} size={}", root_lba, root_size);

        // Read root directory and find SYSTEM.CNF
        let system_cnf_entry = self.find_file_in_dir(root_lba, root_size, "SYSTEM.CNF");
        let (cnf_lba, cnf_size) = match system_cnf_entry {
            Some(e) => e,
            None => {
                eprintln!("FAST_BOOT: SYSTEM.CNF not found");
                return None;
            }
        };

        // Read SYSTEM.CNF
        let mut cnf_data = vec![0u8; cnf_size as usize];
        let sectors_needed = (cnf_size as usize + 2047) / 2048;
        for i in 0..sectors_needed {
            if let Some(sector) = self.cdrom.read_sector_data(cnf_lba + i as u32) {
                let start = i * 2048;
                let end = (start + 2048).min(cnf_size as usize);
                cnf_data[start..end].copy_from_slice(&sector[..end - start]);
            }
        }
        let cnf_text = String::from_utf8_lossy(&cnf_data);
        eprintln!("FAST_BOOT: SYSTEM.CNF:\n{}", cnf_text.trim());

        // Parse BOOT line: "BOOT = cdrom:\SLUS_007.35;1" or similar
        let boot_file = cnf_text.lines()
            .find(|l| l.starts_with("BOOT"))
            .and_then(|l| l.split('=').nth(1))
            .map(|s| s.trim())
            .and_then(|s| {
                // Strip "cdrom:\" or "cdrom:" prefix
                let s = s.strip_prefix("cdrom:\\").or_else(|| s.strip_prefix("cdrom:")).unwrap_or(s);
                // Strip ";1" version suffix
                Some(s.split(';').next().unwrap_or(s).to_uppercase())
            });
        let boot_file = match boot_file {
            Some(f) => f,
            None => {
                eprintln!("FAST_BOOT: no BOOT line in SYSTEM.CNF");
                return None;
            }
        };
        eprintln!("FAST_BOOT: boot file = {}", boot_file);

        // Parse optional SP from SYSTEM.CNF
        let mut sp = 0x801FFF00u32; // default
        if let Some(stack_line) = cnf_text.lines().find(|l| l.starts_with("STACK")) {
            if let Some(val) = stack_line.split('=').nth(1) {
                if let Ok(v) = u32::from_str_radix(val.trim().trim_start_matches("0x").trim_start_matches("0X"), 16) {
                    sp = v;
                }
            }
        }

        // Find the EXE file in root directory
        let (exe_lba, exe_size) = match self.find_file_in_dir(root_lba, root_size, &boot_file) {
            Some(e) => e,
            None => {
                eprintln!("FAST_BOOT: EXE {} not found in root dir", boot_file);
                return None;
            }
        };
        eprintln!("FAST_BOOT: EXE at LBA={} size={}", exe_lba, exe_size);

        // Read EXE header (first sector)
        let header = self.cdrom.read_sector_data(exe_lba)?;
        if &header[0..8] != b"PS-X EXE" {
            eprintln!("FAST_BOOT: bad EXE magic");
            return None;
        }

        let entry_pc = u32::from_le_bytes([header[16], header[17], header[18], header[19]]);
        let init_gp = u32::from_le_bytes([header[20], header[21], header[22], header[23]]);
        let dest_addr = u32::from_le_bytes([header[24], header[25], header[26], header[27]]);
        let file_size = u32::from_le_bytes([header[28], header[29], header[30], header[31]]);
        let bss_addr = u32::from_le_bytes([header[40], header[41], header[42], header[43]]);
        let bss_size = u32::from_le_bytes([header[44], header[45], header[46], header[47]]);
        let sp_base = u32::from_le_bytes([header[48], header[49], header[50], header[51]]);
        let sp_off = u32::from_le_bytes([header[52], header[53], header[54], header[55]]);

        if sp_base != 0 { sp = sp_base.wrapping_add(sp_off); }

        eprintln!("FAST_BOOT: PC={:08X} GP={:08X} dest={:08X} size={:08X} SP={:08X} BSS={:08X}+{:X}",
            entry_pc, init_gp, dest_addr, file_size, sp, bss_addr, bss_size);

        // Load EXE data (after the 2048-byte header) into RAM
        // The header is the first sector; data starts at the second sector
        let data_sectors = (file_size as usize + 2047) / 2048;
        let mut loaded = 0usize;
        for i in 0..data_sectors {
            if let Some(sector) = self.cdrom.read_sector_data(exe_lba + 1 + i as u32) {
                let copy_len = (file_size as usize - loaded).min(2048);
                let ram_addr = (dest_addr as usize + loaded) & 0x1F_FFFF;
                self.ram[ram_addr..ram_addr + copy_len].copy_from_slice(&sector[..copy_len]);
                loaded += copy_len;
            }
        }
        eprintln!("FAST_BOOT: loaded {} bytes to {:08X}", loaded, dest_addr);

        // Clear BSS — matching BIOS exec() which zeroes bss_addr..bss_addr+bss_size
        if bss_size > 0 && bss_addr != 0 {
            let bss_phys = (bss_addr & 0x1F_FFFF) as usize;
            let bss_end = (bss_phys + bss_size as usize).min(self.ram.len());
            for b in &mut self.ram[bss_phys..bss_end] { *b = 0; }
            eprintln!("FAST_BOOT: cleared BSS {:08X}..{:08X}", bss_addr, bss_addr + bss_size);
        }

        // Clear pending ISTAT so stale VBlank/timer IRQs from the BIOS boot
        // don't fire the instant the game re-enables IMASK + IEc.
        self.write_hw_reg32(0x1070, 0); // ISTAT = 0

        // Reset VBlank phase so the next VBlank is a full frame away.
        // This matches the real BIOS: the shell's main loop is VSync-locked,
        // so exec() runs right after a VBlank was serviced. The next VBlank
        // is ~564K cycles in the future, giving the game time to set up its
        // exception handler and enable interrupts at its own pace.
        self.timers.reset_vblank_phase(self.last_cycle);

        Some((entry_pc, init_gp, sp))
    }

    /// Search an ISO9660 directory for a file by name.
    /// Returns (lba, size) of the file if found.
    fn find_file_in_dir(&self, dir_lba: u32, dir_size: u32, name: &str) -> Option<(u32, u32)> {
        let sectors = (dir_size as usize + 2047) / 2048;
        let name_upper = name.to_uppercase();

        for s in 0..sectors {
            let sector = self.cdrom.read_sector_data(dir_lba + s as u32)?;
            let mut pos = 0;
            while pos < 2048 {
                let rec_len = sector[pos] as usize;
                if rec_len == 0 { break; }
                if pos + rec_len > 2048 { break; }

                let entry_lba = u32::from_le_bytes([
                    sector[pos + 2], sector[pos + 3], sector[pos + 4], sector[pos + 5],
                ]);
                let entry_size = u32::from_le_bytes([
                    sector[pos + 10], sector[pos + 11], sector[pos + 12], sector[pos + 13],
                ]);
                let name_len = sector[pos + 32] as usize;
                if name_len > 0 && pos + 33 + name_len <= 2048 {
                    let entry_name = &sector[pos + 33..pos + 33 + name_len];
                    let entry_str = std::str::from_utf8(entry_name).unwrap_or("");
                    // ISO9660 names may have ";1" version suffix
                    let entry_base = entry_str.split(';').next().unwrap_or(entry_str);
                    if entry_base.eq_ignore_ascii_case(&name_upper) {
                        return Some((entry_lba, entry_size));
                    }
                }
                pos += rec_len;
            }
        }
        None
    }

    // ======== Memory Read ========

    pub fn read8(&mut self, addr: u32) -> u8 {
        let phys = addr & 0x1FFF_FFFF;
        match phys {
            0x0000_0000..=0x001F_FFFF => {
                self.ram[(phys & 0x1F_FFFF) as usize]
            }
            0x1F00_0000..=0x1F7F_FFFF => {
                0xFF // EXP1 (unmapped)
            }
            0x1F80_0000..=0x1F80_03FF => {
                self.scratchpad[(phys & 0x3FF) as usize]
            }
            0x1F80_1000..=0x1F80_2FFF => {
                self.hw_read8(phys)
            }
            0x1FC0_0000..=0x1FC7_FFFF => {
                self.bios[(phys & 0x7_FFFF) as usize]
            }
            _ => {
                tracing::trace!("Read8 unmapped: {:08X}", addr);
                0xFF
            }
        }
    }

    pub fn read16(&mut self, addr: u32) -> u16 {
        let phys = addr & 0x1FFF_FFFF;
        match phys {
            0x0000_0000..=0x001F_FFFF => {
                let off = (phys & 0x1F_FFFF) as usize;
                u16::from_le_bytes([self.ram[off], self.ram[off + 1]])
            }
            0x1F80_0000..=0x1F80_03FF => {
                let off = (phys & 0x3FF) as usize;
                u16::from_le_bytes([self.scratchpad[off], self.scratchpad[off + 1]])
            }
            0x1F80_1000..=0x1F80_2FFF => {
                self.hw_read16(phys)
            }
            0x1FC0_0000..=0x1FC7_FFFF => {
                let off = (phys & 0x7_FFFF) as usize;
                u16::from_le_bytes([self.bios[off], self.bios[off + 1]])
            }
            _ => {
                tracing::trace!("Read16 unmapped: {:08X}", addr);
                0xFFFF
            }
        }
    }

    pub fn read32(&mut self, addr: u32) -> u32 {
        let phys = addr & 0x1FFF_FFFF;
        match phys {
            0x0000_0000..=0x001F_FFFF => {
                let off = (phys & 0x1F_FFFF) as usize;
                u32::from_le_bytes([self.ram[off], self.ram[off+1], self.ram[off+2], self.ram[off+3]])
            }
            0x1F00_0000..=0x1F7F_FFFF => {
                0xFFFF_FFFF // EXP1
            }
            0x1F80_0000..=0x1F80_03FF => {
                let off = (phys & 0x3FF) as usize;
                u32::from_le_bytes([self.scratchpad[off], self.scratchpad[off+1], self.scratchpad[off+2], self.scratchpad[off+3]])
            }
            0x1F80_1000..=0x1F80_2FFF => {
                self.hw_read32(phys)
            }
            0x1FC0_0000..=0x1FC7_FFFF => {
                let off = (phys & 0x7_FFFF) as usize;
                u32::from_le_bytes([self.bios[off], self.bios[off+1], self.bios[off+2], self.bios[off+3]])
            }
            0x1FFE_0000..=0x1FFE_01FF => {
                // Cache control register (KSEG2)
                0
            }
            _ => {
                tracing::trace!("Read32 unmapped: {:08X}", addr);
                0
            }
        }
    }

    // ======== Memory Write ========

    pub fn write8(&mut self, addr: u32, value: u8) {
        let phys = addr & 0x1FFF_FFFF;
        if phys >= 0x10000 && phys < 0x10100 {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            if n < 20 { eprintln!("W8_10000 #{}: [{:08X}]={:02X} pc={:08X}", n, addr, value, self.diag_cpu_pc); }
        }
        if phys >= 0x42018 && phys < 0x42348 {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            if n < 20 {
                eprintln!("W8_CODE #{}: [{:08X}]={:02X} pc={:08X}", n, addr, value, self.diag_cpu_pc);
            }
        }
        match phys {
            0x0000_0000..=0x001F_FFFF => {
                self.ram[(phys & 0x1F_FFFF) as usize] = value;
            }
            0x1F80_0000..=0x1F80_03FF => {
                self.scratchpad[(phys & 0x3FF) as usize] = value;
            }
            0x1F80_1000..=0x1F80_2FFF => {
                self.hw_write8(phys, value);
            }
            _ => {
                tracing::trace!("Write8 unmapped: {:08X} = {:02X}", addr, value);
            }
        }
    }

    pub fn write16(&mut self, addr: u32, value: u16) {
        let phys = addr & 0x1FFF_FFFF;
        if phys >= 0x10000 && phys < 0x10100 {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            if n < 20 { eprintln!("W16_10000 #{}: [{:08X}]={:04X} pc={:08X}", n, addr, value, self.diag_cpu_pc); }
        }
        if phys >= 0x42018 && phys < 0x42348 {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            if n < 20 {
                eprintln!("W16_CODE #{}: [{:08X}]={:04X} pc={:08X}", n, addr, value, self.diag_cpu_pc);
            }
        }
        match phys {
            0x0000_0000..=0x001F_FFFF => {
                let off = (phys & 0x1F_FFFF) as usize;
                let bytes = value.to_le_bytes();
                self.ram[off] = bytes[0];
                self.ram[off + 1] = bytes[1];
            }
            0x1F80_0000..=0x1F80_03FF => {
                let off = (phys & 0x3FF) as usize;
                let bytes = value.to_le_bytes();
                self.scratchpad[off] = bytes[0];
                self.scratchpad[off + 1] = bytes[1];
            }
            0x1F80_1000..=0x1F80_2FFF => {
                self.hw_write16(phys, value);
            }
            _ => {
                tracing::trace!("Write16 unmapped: {:08X} = {:04X}", addr, value);
            }
        }
    }

    pub fn write32(&mut self, addr: u32, value: u32) {
        let phys = addr & 0x1FFF_FFFF;
        // DIAG: track first N writes to 0x10000..0x10100 (the BIOS mesh buffer)
        if phys >= 0x10000 && phys < 0x10100 {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            if n < 20 {
                eprintln!("W32_10000 #{}: [{:08X}]={:08X} pc={:08X}", n, addr, value, self.diag_cpu_pc);
            }
        }
        // DIAG (code identity): first 40 writes into RAM 0x42018..0x42348
        // (the stuck function's instruction range). If this region is being
        // populated by a memcpy from BIOS ROM, we'll see the sequential
        // writes here.
        if phys >= 0x42018 && phys < 0x42348 {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            if n < 40 {
                eprintln!("W32_CODE #{}: [{:08X}]={:08X} pc={:08X} cyc={}",
                    n, addr, value, self.diag_cpu_pc, self.last_cycle);
            }
        }
        match phys {
            0x0000_0000..=0x001F_FFFF => {
                let off = (phys & 0x1F_FFFF) as usize;
                if off == 0x80 {
                    use std::sync::atomic::{AtomicBool, Ordering};
                    static DUMPED_HANDLER_INSTALL: AtomicBool = AtomicBool::new(false);
                    if !DUMPED_HANDLER_INSTALL.swap(true, Ordering::Relaxed) {
                        // Skip the BIOS's own write
                    } else {
                        eprintln!("GAME_HANDLER_INSTALL: write32 [0x80]={:08X} from pc={:08X}", value, self.diag_cpu_pc);
                        // Dump game code around the install PC from RAM
                        let install_pc = self.diag_cpu_pc;
                        eprintln!("  INSTALL FUNCTION context (pc-0x40 to pc+0x60):");
                        for i in 0..40u32 {
                            let a = install_pc.wrapping_sub(0x40).wrapping_add(i * 4);
                            let phys_a = (a & 0x1FFF_FFFF) as usize;
                            if phys_a + 3 < self.ram.len() {
                                let val = u32::from_le_bytes([
                                    self.ram[phys_a], self.ram[phys_a+1],
                                    self.ram[phys_a+2], self.ram[phys_a+3]]);
                                eprintln!("    {:08X}: {:08X}{}", a, val,
                                    if a == install_pc { " <<< INSTALL PC" } else { "" });
                            }
                        }
                    }
                }
                let bytes = value.to_le_bytes();
                self.ram[off..off+4].copy_from_slice(&bytes);
            }
            0x1F80_0000..=0x1F80_03FF => {
                let off = (phys & 0x3FF) as usize;
                let bytes = value.to_le_bytes();
                self.scratchpad[off..off+4].copy_from_slice(&bytes);
            }
            0x1F80_1000..=0x1F80_2FFF => {
                self.hw_write32(phys, value);
            }
            0x1FFE_0000..=0x1FFE_01FF => {
                // Cache control register
                tracing::trace!("Cache control write: {:08X} = {:08X}", addr, value);
            }
            _ => {
                tracing::trace!("Write32 unmapped: {:08X} = {:08X}", addr, value);
            }
        }
    }

    // ======== Hardware Register I/O ========

    fn hw_read8(&mut self, phys: u32) -> u8 {
        let offset = phys & 0xFFFF;
        match offset {
            // SIO0 — 8-bit read from data register
            0x1040 => self.sio.read8(),
            // CD-ROM
            0x1800..=0x1803 => self.cdrom.read(offset - 0x1800),
            _ => {
                tracing::trace!("HW read8: {:04X}", offset);
                self.hw_regs[offset as usize]
            }
        }
    }

    fn hw_read16(&mut self, phys: u32) -> u16 {
        let offset = phys & 0xFFFF;
        match offset {
            // SIO0 (controller/memory card)
            0x1040 => { let b = self.sio.read8(); b as u16 | ((self.sio.read8() as u16) << 8) }
            0x1044 => self.sio.read_status16(),
            0x1048 => self.sio.read_mode16(),
            0x104A => self.sio.read_ctrl16(),
            0x104E => self.sio.read_baud16(),

            // Interrupt
            0x1070 => self.read_hw_reg16(offset),
            0x1074 => self.read_hw_reg16(offset),

            // Timers
            0x1100 => { let v = self.timers.read_counter(0, self.last_cycle); self.drain_timer_irqs(); v as u16 }
            0x1104 => { let v = self.timers.read_mode(0, self.last_cycle); self.drain_timer_irqs(); v as u16 }
            0x1108 => self.timers.read_target(0) as u16,
            0x1110 => { let v = self.timers.read_counter(1, self.last_cycle); self.drain_timer_irqs(); v as u16 }
            0x1114 => { let v = self.timers.read_mode(1, self.last_cycle); self.drain_timer_irqs(); v as u16 }
            0x1118 => self.timers.read_target(1) as u16,
            0x1120 => { let v = self.timers.read_counter(2, self.last_cycle); self.drain_timer_irqs(); v as u16 }
            0x1124 => { let v = self.timers.read_mode(2, self.last_cycle); self.drain_timer_irqs(); v as u16 }
            0x1128 => self.timers.read_target(2) as u16,

            // SPU
            0x1C00..=0x1FFF => {
                let spu_offset = offset - 0x1C00;
                self.spu.read16(spu_offset as u32)
            }

            _ => {
                tracing::trace!("HW read16: {:04X}", offset);
                self.read_hw_reg16(offset)
            }
        }
    }

    fn hw_read32(&mut self, phys: u32) -> u32 {
        let offset = phys & 0xFFFF;
        match offset {
            // SIO0
            0x1040 => {
                let b0 = self.sio.read8() as u32;
                let b1 = self.sio.read8() as u32;
                let b2 = self.sio.read8() as u32;
                let b3 = self.sio.read8() as u32;
                b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
            }
            0x1044 => self.sio.read_status32(),

            // Interrupt
            0x1070 => self.read_hw_reg32(offset),
            0x1074 => self.read_hw_reg32(offset),

            // DMA registers
            0x1080..=0x10F4 => self.read_hw_reg32(offset),

            // GPU
            0x1810 => self.gpu.read_data(),
            0x1814 => self.gpu.read_status(),

            // Timers
            0x1100 => { let v = self.timers.read_counter(0, self.last_cycle); self.drain_timer_irqs(); v }
            0x1104 => { let v = self.timers.read_mode(0, self.last_cycle); self.drain_timer_irqs(); v }
            0x1108 => self.timers.read_target(0),
            0x1110 => { let v = self.timers.read_counter(1, self.last_cycle); self.drain_timer_irqs(); v }
            0x1114 => { let v = self.timers.read_mode(1, self.last_cycle); self.drain_timer_irqs(); v }
            0x1118 => self.timers.read_target(1),
            0x1120 => { let v = self.timers.read_counter(2, self.last_cycle); self.drain_timer_irqs(); v }
            0x1124 => { let v = self.timers.read_mode(2, self.last_cycle); self.drain_timer_irqs(); v }
            0x1128 => self.timers.read_target(2),

            // Memory control
            0x1000..=0x1024 => self.read_hw_reg32(offset),

            // SPU — 32-bit reads split into two 16-bit register reads
            // Matching Redux: read32 falls through to m_hard which reflects
            // the last write16 values; we read both halves directly.
            0x1C00..=0x1FFF => {
                let spu_offset = offset - 0x1C00;
                let lo = self.spu.read16(spu_offset as u32) as u32;
                let hi = self.spu.read16((spu_offset + 2) as u32) as u32;
                lo | (hi << 16)
            }

            _ => {
                tracing::trace!("HW read32: {:04X}", offset);
                self.read_hw_reg32(offset)
            }
        }
    }

    fn hw_write8(&mut self, phys: u32, value: u8) {
        let offset = phys & 0xFFFF;
        match offset {
            // SIO0 (controller/memory card) — 8-bit writes to data register
            0x1040 => { self.sio.write8(value); self.drain_sio_irq(); }
            // CD-ROM
            0x1800..=0x1803 => {
                // DIAG: trace CD register writes to understand the command sequence
                {
                    use std::sync::atomic::{AtomicU32, Ordering};
                    static N: AtomicU32 = AtomicU32::new(0);
                    let n = N.fetch_add(1, Ordering::Relaxed);
                    if n < 60 {
                        eprintln!("CD_WR #{}: port={} val={:02X} pc={:08X}",
                            n, offset - 0x1800, value, self.diag_cpu_pc);
                    }
                }
                let hw_regs = &mut self.hw_regs;
                self.cdrom.write(offset - 0x1800, value, &mut |bit| {
                    // set_irq inline — can't call self.set_irq due to borrow
                    let off = 0x1070usize;
                    let istat = u32::from_le_bytes([hw_regs[off], hw_regs[off+1], hw_regs[off+2], hw_regs[off+3]]);
                    let new_istat = istat | (1 << bit);
                    hw_regs[off..off+4].copy_from_slice(&new_istat.to_le_bytes());
                });
                self.drain_cdrom_irqs();
            }
            _ => {
                self.hw_regs[offset as usize] = value;
            }
        }
    }

    fn hw_write16(&mut self, phys: u32, value: u16) {
        let offset = phys & 0xFFFF;
        match offset {
            // SIO0
            0x1040 => { self.sio.write8(value as u8); self.drain_sio_irq(); }
            0x1048 => self.sio.write_mode16(value),
            0x104A => {
                self.sio.write_ctrl16(value);
                // Cancel scheduled SIO interrupt on RESET, matching Redux:
                // m_regs.interrupt &= ~(1 << PSXINT_SIO)
                if value & 0x0040 != 0 {
                    self.scheduler.cancel(crate::scheduler::PsxInt::Sio);
                }
                self.drain_sio_irq();
            }
            0x104E => self.sio.write_baud16(value),

            // Interrupt
            0x1070 => {
                let current = self.read_hw_reg16(0x1070);
                self.write_hw_reg16(0x1070, current & value);
            }
            0x1074 => {
                use std::sync::atomic::{AtomicU32, Ordering};
                static NI16: AtomicU32 = AtomicU32::new(0);
                let n = NI16.fetch_add(1, Ordering::Relaxed);
                if n < 30 {
                    eprintln!("IMASK_WR16 #{}: val=0x{:04X} pc={:08X} cyc={}",
                        n, value, self.diag_cpu_pc, self.last_cycle);
                }
                self.write_hw_reg16(offset, value)
            }

            // Timers
            0x1100 => { self.timers.write_counter(0, value as u32, self.last_cycle); self.drain_timer_irqs(); }
            0x1104 => { self.timers.write_mode(0, value as u32, self.last_cycle); self.drain_timer_irqs(); }
            0x1108 => { self.timers.write_target(0, value as u32, self.last_cycle); self.drain_timer_irqs(); }
            0x1110 => { self.timers.write_counter(1, value as u32, self.last_cycle); self.drain_timer_irqs(); }
            0x1114 => { self.timers.write_mode(1, value as u32, self.last_cycle); self.drain_timer_irqs(); }
            0x1118 => { self.timers.write_target(1, value as u32, self.last_cycle); self.drain_timer_irqs(); }
            0x1120 => { self.timers.write_counter(2, value as u32, self.last_cycle); self.drain_timer_irqs(); }
            0x1124 => { self.timers.write_mode(2, value as u32, self.last_cycle); self.drain_timer_irqs(); }
            0x1128 => { self.timers.write_target(2, value as u32, self.last_cycle); self.drain_timer_irqs(); }

            // SPU
            0x1C00..=0x1FFF => {
                let spu_offset = offset - 0x1C00;
                self.spu.write16(spu_offset as u32, value);
            }

            _ => {
                self.write_hw_reg16(offset, value);
            }
        }
    }

    fn hw_write32(&mut self, phys: u32, value: u32) {
        let offset = phys & 0xFFFF;
        match offset {
            // SIO0
            0x1040 => { self.sio.write8(value as u8); self.drain_sio_irq(); }
            0x1048 => self.sio.write_mode16(value as u16),
            0x104A => {
                self.sio.write_ctrl16(value as u16);
                if value & 0x0040 != 0 {
                    self.scheduler.cancel(crate::scheduler::PsxInt::Sio);
                }
                self.drain_sio_irq();
            }
            0x104E => self.sio.write_baud16(value as u16),

            // Interrupt
            0x1070 => {
                let current = self.read_hw_reg32(0x1070);
                self.write_hw_reg32(0x1070, current & value);
            }
            0x1074 => {
                use std::sync::atomic::{AtomicU32, Ordering};
                static NI32: AtomicU32 = AtomicU32::new(0);
                let n = NI32.fetch_add(1, Ordering::Relaxed);
                if n < 30 {
                    eprintln!("IMASK_WR32 #{}: val=0x{:08X} pc={:08X} cyc={}",
                        n, value, self.diag_cpu_pc, self.last_cycle);
                }
                self.write_hw_reg32(offset, value)
            }

            // DMA registers
            0x1080..=0x10EF => {
                let channel = ((offset - 0x1080) >> 4) as usize;
                let reg = (offset & 0xF) as usize;
                self.write_hw_reg32(offset, value);
                if reg == 8 {
                    // CHCR write — might trigger DMA
                    self.dma_exec(channel, value);
                }
            }
            0x10F0 => self.write_hw_reg32(offset, value), // DPCR
            0x10F4 => {
                // DICR — DMA interrupt control
                let old = self.read_hw_reg32(0x10F4);
                // Bits 24-30 are ack'd by writing 1
                let ack = value & 0x7F00_0000;
                let new_val = (old & 0x7F00_0000 & !ack) // clear ack'd flags
                    | (value & 0x00FF_803F); // set writable bits
                // Bit 31 = master flag (read-only, computed)
                let master_enable = new_val & (1 << 23) != 0;
                let channel_flags = (new_val >> 24) & 0x7F;
                let channel_enables = (new_val >> 16) & 0x7F;
                let master_flag = master_enable && (channel_flags & channel_enables) != 0;
                let final_val = (new_val & 0x7FFF_FFFF) | ((master_flag as u32) << 31);
                self.write_hw_reg32(0x10F4, final_val);
            }

            // GPU
            0x1810 => self.gpu.gp0_write(value),
            0x1814 => self.gpu.gp1_write(value),

            // Timers
            0x1100 => { self.timers.write_counter(0, value, self.last_cycle); self.drain_timer_irqs(); }
            0x1104 => { self.timers.write_mode(0, value, self.last_cycle); self.drain_timer_irqs(); }
            0x1108 => { self.timers.write_target(0, value & 0xFFFF, self.last_cycle); self.drain_timer_irqs(); }
            0x1110 => { self.timers.write_counter(1, value, self.last_cycle); self.drain_timer_irqs(); }
            0x1114 => { self.timers.write_mode(1, value, self.last_cycle); self.drain_timer_irqs(); }
            0x1118 => { self.timers.write_target(1, value & 0xFFFF, self.last_cycle); self.drain_timer_irqs(); }
            0x1120 => { self.timers.write_counter(2, value, self.last_cycle); self.drain_timer_irqs(); }
            0x1124 => { self.timers.write_mode(2, value, self.last_cycle); self.drain_timer_irqs(); }
            0x1128 => { self.timers.write_target(2, value & 0xFFFF, self.last_cycle); self.drain_timer_irqs(); }

            // Memory control
            0x1000..=0x1024 => {
                tracing::trace!("Memory control write: {:04X} = {:08X}", offset, value);
                self.write_hw_reg32(offset, value);
            }

            // SPU — 32-bit writes split into two 16-bit register writes
            // Matching Redux psxhw.cc write32: write16(add, low); write16(add+2, high)
            0x1C00..=0x1FFF => {
                let spu_offset = offset - 0x1C00;
                self.spu.write16(spu_offset as u32, value as u16);
                self.spu.write16((spu_offset + 2) as u32, (value >> 16) as u16);
            }

            // RAM size
            0x1060 => {
                tracing::trace!("RAM size config: {:08X}", value);
                self.write_hw_reg32(offset, value);
            }

            _ => {
                self.write_hw_reg32(offset, value);
            }
        }
    }

    // ======== Hardware register backing store helpers ========

    fn read_hw_reg16(&self, offset: u32) -> u16 {
        let off = offset as usize;
        u16::from_le_bytes([self.hw_regs[off], self.hw_regs[off + 1]])
    }

    fn write_hw_reg16(&mut self, offset: u32, value: u16) {
        let off = offset as usize;
        let bytes = value.to_le_bytes();
        self.hw_regs[off] = bytes[0];
        self.hw_regs[off + 1] = bytes[1];
    }

    fn read_hw_reg32(&self, offset: u32) -> u32 {
        let off = offset as usize;
        u32::from_le_bytes([
            self.hw_regs[off],
            self.hw_regs[off + 1],
            self.hw_regs[off + 2],
            self.hw_regs[off + 3],
        ])
    }

    fn write_hw_reg32(&mut self, offset: u32, value: u32) {
        let off = offset as usize;
        let bytes = value.to_le_bytes();
        self.hw_regs[off..off+4].copy_from_slice(&bytes);
    }

    // ======== Interrupt helpers ========

    pub fn read_istat(&self) -> u32 {
        self.read_hw_reg32(0x1070)
    }

    pub fn read_imask(&self) -> u32 {
        self.read_hw_reg32(0x1074)
    }

    /// Drain pending timer IRQs into ISTAT.
    /// Matching pcsx-redux SoftGPU::vblank(): toggle GPUSTAT bit 31 on every
    /// VBlank. The retail BIOS shell polls this bit in its waitVSync loop.
    pub fn drain_timer_irqs(&mut self) {
        let irqs = self.timers.drain_irqs();
        if irqs != 0 {
            let istat = self.read_hw_reg32(0x1070);
            self.write_hw_reg32(0x1070, istat | irqs);
            // VBlank is IRQ bit 0 (mask 0x01). On each VBlank, toggle the
            // GPU's interlace/field flag (GPUSTAT bit 31).
            if irqs & 0x01 != 0 {
                self.gpu.status.toggle_interlace_field();
            }
        }
    }

    pub fn set_irq(&mut self, bit: u32) {
        let istat = self.read_hw_reg32(0x1070);
        self.write_hw_reg32(0x1070, istat | (1 << bit));
    }

    /// Handle interrupts returned by scheduler.check_interrupts()
    pub fn handle_fired_interrupts(&mut self, fired: u32) {
        if fired == 0 {
            return;
        }
        for i in 0..16u32 {
            if fired & (1 << i) == 0 {
                continue;
            }
            match i {
                // PSXINT_SIO = 0 -> SIO interrupt
                0 => {
                    self.sio.interrupt();
                    self.set_irq(7); // IRQ7 = SIO0
                }
                // PSXINT_CDR = 2 -> CD-ROM command interrupt
                2 => {
                    let hw = &mut self.hw_regs;
                    self.cdrom.interrupt(&mut |bit| {
                        let off = 0x1070usize;
                        let istat = u32::from_le_bytes([hw[off], hw[off+1], hw[off+2], hw[off+3]]);
                        hw[off..off+4].copy_from_slice(&(istat | (1 << bit)).to_le_bytes());
                    });
                    self.drain_cdrom_irqs();
                }
                // PSXINT_CDREAD = 3 -> CD-ROM read interrupt
                3 => {
                    let hw = &mut self.hw_regs;
                    self.cdrom.read_interrupt(&mut |bit| {
                        let off = 0x1070usize;
                        let istat = u32::from_le_bytes([hw[off], hw[off+1], hw[off+2], hw[off+3]]);
                        hw[off..off+4].copy_from_slice(&(istat | (1 << bit)).to_le_bytes());
                    });
                    self.drain_cdrom_irqs();
                }
                4 => self.dma_gpu_interrupt(),
                9 => self.dma_otc_interrupt(),
                // PSXINT_CDRLID = 13 -> CD-ROM lid/seek
                13 => {
                    self.cdrom.lid_seek_interrupt();
                    self.drain_cdrom_irqs();
                }
                _ => tracing::trace!("Scheduler fired interrupt {} (unhandled)", i),
            }
        }
    }

    /// Drain SIO pending interrupt into scheduler
    fn drain_sio_irq(&mut self) {
        if self.sio.pending_irq {
            self.sio.pending_irq = false;
            let delay = self.sio.pending_irq_delay;
            self.scheduler.schedule(crate::scheduler::PsxInt::Sio, self.last_cycle, delay);
        }
    }

    /// Drain pending CD-ROM interrupt requests into the scheduler
    fn drain_cdrom_irqs(&mut self) {
        let irqs: Vec<_> = self.cdrom.pending_irqs.drain(..).collect();
        let cycle = self.scheduler.int_targets[0]; // approximate current cycle
        for irq in irqs {
            use cdrom::CdIrqType;
            let sched_irq = match irq.irq_type {
                CdIrqType::Command => crate::scheduler::PsxInt::CdRom,
                CdIrqType::Read => crate::scheduler::PsxInt::CdRead,
                CdIrqType::Lid => crate::scheduler::PsxInt::CdRomLid,
            };
            // Use a rough current cycle estimate — the caller should pass it, but
            // for now we store it when branch_test runs
            self.scheduler.schedule(sched_irq, self.last_cycle, irq.delay);
        }
    }

    // ======== DMA ========

    fn dma_exec(&mut self, channel: usize, chcr: u32) {
        // Check if DMA is enabled (trigger bit + channel enabled in DPCR)
        let dpcr = self.read_hw_reg32(0x10F0);
        let enabled = dpcr & (8 << (channel * 4)) != 0;
        let trigger = chcr & 0x0100_0000 != 0;

        if !trigger || !enabled {
            return;
        }

        let base = 0x1080 + (channel as u32) * 0x10;
        let madr = self.read_hw_reg32(base) & 0x1F_FFFC;
        let bcr = self.read_hw_reg32(base + 4);

        tracing::debug!("DMA ch{} exec: MADR={:08X} BCR={:08X} CHCR={:08X}", channel, madr, bcr, chcr);

        match channel {
            0 => dma::dma_mdec_in(self, madr, bcr, chcr),
            1 => dma::dma_mdec_out(self, madr, bcr, chcr),
            2 => dma::dma_gpu(self, madr, bcr, chcr),
            3 => dma::dma_cdrom(self, madr, bcr, chcr),
            4 => dma::dma_spu(self, madr, bcr, chcr),
            6 => dma::dma_otc(self, madr, bcr, chcr),
            _ => {
                tracing::warn!("DMA channel {} not implemented", channel);
            }
        }
    }

    pub fn cdrom_has_data(&self) -> bool {
        // Check DRQSTS flag in cdrom ctrl
        self.cdrom.read_ctrl_drq()
    }

    pub fn dma_gpu_interrupt(&mut self) {
        self.dma_channel_done(2);
    }

    pub fn dma_otc_interrupt(&mut self) {
        self.dma_channel_done(6);
    }

    fn dma_channel_done(&mut self, channel: usize) {
        // Clear trigger bit in CHCR
        let base = 0x1080 + (channel as u32) * 0x10;
        let chcr = self.read_hw_reg32(base + 8);
        self.write_hw_reg32(base + 8, chcr & !0x0100_0000);

        // Set interrupt flag in DICR
        let dicr = self.read_hw_reg32(0x10F4);
        let channel_enable = (dicr >> (16 + channel)) & 1;
        if channel_enable != 0 {
            let new_dicr = dicr | (1 << (24 + channel));
            // Check master flag
            let master_enable = new_dicr & (1 << 23) != 0;
            let flags = (new_dicr >> 24) & 0x7F;
            let enables = (new_dicr >> 16) & 0x7F;
            let master_flag = master_enable && (flags & enables) != 0;
            let final_dicr = (new_dicr & 0x7FFF_FFFF) | ((master_flag as u32) << 31);
            self.write_hw_reg32(0x10F4, final_dicr);

            if master_flag {
                // Fire DMA master IRQ (IRQ3)
                self.set_irq(3);
            }
        }
    }
}
