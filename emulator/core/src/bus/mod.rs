pub mod dma;

use crate::scheduler::Scheduler;
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
    pub dma: dma::DmaController,
}

impl Bus {
    pub fn new() -> Self {
        Self {
            ram: vec![0u8; 0x0020_0000].into_boxed_slice().try_into().unwrap(),
            bios: vec![0u8; 0x0008_0000].into_boxed_slice().try_into().unwrap(),
            scratchpad: vec![0u8; 0x400].into_boxed_slice().try_into().unwrap(),
            hw_regs: vec![0u8; 0x1_0000].into_boxed_slice().try_into().unwrap(),
            gpu: Gpu::new(),
            spu: Spu::new(),
            cdrom: CdRom::new(),
            timers: Timers::new(),
            scheduler: Scheduler::new(),
            dma: dma::DmaController::new(),
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
        match phys {
            0x0000_0000..=0x001F_FFFF => {
                let off = (phys & 0x1F_FFFF) as usize;
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
            // Joypad/SIO
            0x1040 => 0xFF, // RX data (no controller)
            0x1044 => 0x0005, // SIO status: TX ready, TX empty
            0x1048 => 0, // SIO mode
            0x104A => 0, // SIO control
            0x104E => 0, // SIO baud

            // Interrupt
            0x1070 => self.read_hw_reg16(offset),
            0x1074 => self.read_hw_reg16(offset),

            // Timers
            0x1100 => self.timers.read_counter(0),
            0x1104 => self.timers.read_mode(0),
            0x1108 => self.timers.read_target(0),
            0x1110 => self.timers.read_counter(1),
            0x1114 => self.timers.read_mode(1),
            0x1118 => self.timers.read_target(1),
            0x1120 => self.timers.read_counter(2),
            0x1124 => self.timers.read_mode(2),
            0x1128 => self.timers.read_target(2),

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
            // Joypad
            0x1040 => 0xFFFF_FFFF, // RX data (no controller connected)
            0x1044 => 0x0000_0005, // SIO status

            // Interrupt
            0x1070 => self.read_hw_reg32(offset),
            0x1074 => self.read_hw_reg32(offset),

            // DMA registers
            0x1080..=0x10F4 => self.read_hw_reg32(offset),

            // GPU
            0x1810 => self.gpu.read_data(),
            0x1814 => self.gpu.read_status(),

            // Timers
            0x1100 => self.timers.read_counter(0) as u32,
            0x1104 => self.timers.read_mode(0) as u32,
            0x1108 => self.timers.read_target(0) as u32,
            0x1110 => self.timers.read_counter(1) as u32,
            0x1114 => self.timers.read_mode(1) as u32,
            0x1118 => self.timers.read_target(1) as u32,
            0x1120 => self.timers.read_counter(2) as u32,
            0x1124 => self.timers.read_mode(2) as u32,
            0x1128 => self.timers.read_target(2) as u32,

            // Memory control
            0x1000..=0x1024 => self.read_hw_reg32(offset),

            // SPU
            0x1C00..=0x1FFF => {
                let spu_offset = offset - 0x1C00;
                self.spu.read16(spu_offset as u32) as u32
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
            // CD-ROM
            0x1800..=0x1803 => self.cdrom.write(offset - 0x1800, value),
            _ => {
                tracing::trace!("HW write8: {:04X} = {:02X}", offset, value);
                self.hw_regs[offset as usize] = value;
            }
        }
    }

    fn hw_write16(&mut self, phys: u32, value: u16) {
        let offset = phys & 0xFFFF;
        match offset {
            // Joypad/SIO
            0x1040..=0x104E => {
                tracing::trace!("SIO write16: {:04X} = {:04X}", offset, value);
                self.write_hw_reg16(offset, value);
            }

            // Interrupt
            0x1070 => {
                // ISTAT — writing 1s acknowledges (clears) bits
                let current = self.read_hw_reg16(0x1070);
                self.write_hw_reg16(0x1070, current & value);
            }
            0x1074 => self.write_hw_reg16(offset, value),

            // Timers
            0x1100 => self.timers.write_counter(0, value, 0),
            0x1104 => self.timers.write_mode(0, value, 0),
            0x1108 => self.timers.write_target(0, value),
            0x1110 => self.timers.write_counter(1, value, 0),
            0x1114 => self.timers.write_mode(1, value, 0),
            0x1118 => self.timers.write_target(1, value),
            0x1120 => self.timers.write_counter(2, value, 0),
            0x1124 => self.timers.write_mode(2, value, 0),
            0x1128 => self.timers.write_target(2, value),

            // SPU
            0x1C00..=0x1FFF => {
                let spu_offset = offset - 0x1C00;
                self.spu.write16(spu_offset as u32, value);
            }

            _ => {
                tracing::trace!("HW write16: {:04X} = {:04X}", offset, value);
                self.write_hw_reg16(offset, value);
            }
        }
    }

    fn hw_write32(&mut self, phys: u32, value: u32) {
        let offset = phys & 0xFFFF;
        match offset {
            // Joypad
            0x1040..=0x104E => {
                tracing::trace!("SIO write32: {:04X} = {:08X}", offset, value);
                self.write_hw_reg32(offset, value);
            }

            // Interrupt
            0x1070 => {
                let current = self.read_hw_reg32(0x1070);
                self.write_hw_reg32(0x1070, current & value);
            }
            0x1074 => self.write_hw_reg32(offset, value),

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
            0x1100 => self.timers.write_counter(0, value as u16, 0),
            0x1104 => self.timers.write_mode(0, value as u16, 0),
            0x1108 => self.timers.write_target(0, value as u16),
            0x1110 => self.timers.write_counter(1, value as u16, 0),
            0x1114 => self.timers.write_mode(1, value as u16, 0),
            0x1118 => self.timers.write_target(1, value as u16),
            0x1120 => self.timers.write_counter(2, value as u16, 0),
            0x1124 => self.timers.write_mode(2, value as u16, 0),
            0x1128 => self.timers.write_target(2, value as u16),

            // Memory control
            0x1000..=0x1024 => {
                tracing::trace!("Memory control write: {:04X} = {:08X}", offset, value);
                self.write_hw_reg32(offset, value);
            }

            // SPU
            0x1C00..=0x1FFF => {
                let spu_offset = offset - 0x1C00;
                self.spu.write16(spu_offset as u32, value as u16);
            }

            // RAM size
            0x1060 => {
                tracing::trace!("RAM size config: {:08X}", value);
                self.write_hw_reg32(offset, value);
            }

            _ => {
                tracing::trace!("HW write32: {:04X} = {:08X}", offset, value);
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
                2 => self.set_irq(4),  // CDROM -> IRQ2 (bit 2)... wait, CDROM is IRQ bit 2
                3 => self.set_irq(2),  // CDROM read
                4 => self.dma_gpu_interrupt(),
                9 => self.dma_otc_interrupt(),
                _ => tracing::trace!("Scheduler fired interrupt {} (unhandled)", i),
            }
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
            2 => dma::dma_gpu(self, madr, bcr, chcr),
            6 => dma::dma_otc(self, madr, bcr, chcr),
            _ => {
                tracing::warn!("DMA channel {} not implemented", channel);
            }
        }
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
