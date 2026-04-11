use super::Bus;

pub struct DmaController;

impl DmaController {
    pub fn new() -> Self {
        Self
    }
}

/// DMA Channel 0: MDEC in (stub)
pub fn dma_mdec_in(bus: &mut Bus, _madr: u32, _bcr: u32, _chcr: u32) {
    tracing::warn!("DMA0 MDEC-in not implemented");
    bus.dma_channel_done(0);
}

/// DMA Channel 1: MDEC out (stub)
pub fn dma_mdec_out(bus: &mut Bus, _madr: u32, _bcr: u32, _chcr: u32) {
    tracing::warn!("DMA1 MDEC-out not implemented");
    bus.dma_channel_done(1);
}

/// DMA Channel 2: GPU
pub fn dma_gpu(bus: &mut Bus, madr: u32, bcr: u32, chcr: u32) {
    let direction = chcr & 1;
    let mode = (chcr >> 9) & 3;

    match mode {
        1 => {
            // Block mode
            let block_size = if bcr & 0xFFFF == 0 { 0x10000u32 } else { bcr & 0xFFFF };
            let block_count = if bcr >> 16 == 0 { 0x10000u32 } else { bcr >> 16 };
            let total_words = block_count * block_size;

            if direction == 1 {
                // RAM -> GPU
                let mut addr = madr;
                for _ in 0..total_words {
                    let phys = (addr & 0x1F_FFFF) as usize;
                    let word = u32::from_le_bytes([
                        bus.ram[phys], bus.ram[phys+1], bus.ram[phys+2], bus.ram[phys+3],
                    ]);
                    bus.gpu.gp0_write(word);
                    addr = addr.wrapping_add(4) & 0x1F_FFFC;
                }
            } else {
                // GPU -> RAM
                let mut addr = madr;
                for _ in 0..total_words {
                    let word = bus.gpu.read_data();
                    let phys = (addr & 0x1F_FFFF) as usize;
                    bus.ram[phys..phys+4].copy_from_slice(&word.to_le_bytes());
                    addr = addr.wrapping_add(4) & 0x1F_FFFC;
                }
            }
            tracing::debug!("DMA2 block: {} words, dir={}", total_words, direction);
        }
        2 => {
            // Linked-list mode
            if direction != 1 { return; }
            let mut addr = madr & 0x1F_FFFC;
            let mut count = 0u32;
            loop {
                let phys = (addr & 0x1F_FFFF) as usize;
                let header = u32::from_le_bytes([
                    bus.ram[phys], bus.ram[phys+1], bus.ram[phys+2], bus.ram[phys+3],
                ]);
                let num_words = (header >> 24) as u32;
                for i in 1..=num_words {
                    let wa = (addr.wrapping_add(i * 4) & 0x1F_FFFF) as usize;
                    let word = u32::from_le_bytes([
                        bus.ram[wa], bus.ram[wa+1], bus.ram[wa+2], bus.ram[wa+3],
                    ]);
                    bus.gpu.gp0_write(word);
                }
                count += 1;
                if count > 0x20_0000 || header & 0x00FF_FFFF == 0x00FF_FFFF { break; }
                addr = header & 0x1F_FFFC;
            }
            tracing::debug!("DMA2 linked-list: {} nodes", count);
        }
        _ => tracing::warn!("DMA2 unhandled mode {}", mode),
    }
    bus.dma_gpu_interrupt();
}

/// DMA Channel 3: CD-ROM -> RAM
/// Matching Redux cdr::dma(): guards on m_read (not DRQSTS).
pub fn dma_cdrom(bus: &mut Bus, madr: u32, bcr: u32, chcr: u32) {
    match chcr {
        0x11000000 | 0x11400100 => {
            // DIAG: track first few DMA3 transfers
            {
                use std::sync::atomic::{AtomicU32, Ordering};
                static N: AtomicU32 = AtomicU32::new(0);
                let n = N.fetch_add(1, Ordering::Relaxed);
                if n < 8 {
                    eprintln!("DMA3_CALL #{}: madr={:08X} bcr={:08X} chcr={:08X} m_read={}",
                        n, madr, bcr, chcr, bus.cdrom.is_read_active());
                }
            }
            if !bus.cdrom.is_read_active() {
                bus.dma_channel_done(3);
                return;
            }
            let cdsize = ((bcr & 0xFFFF) * 4) as usize;
            let cdsize = if cdsize == 0 { 2048 } else { cdsize };

            let mut buf = vec![0u8; cdsize];
            bus.cdrom.dma_read(&mut buf);

            // DIAG: log what was transferred
            {
                use std::sync::atomic::{AtomicU32, Ordering};
                static M: AtomicU32 = AtomicU32::new(0);
                let m = M.fetch_add(1, Ordering::Relaxed);
                if m < 4 {
                    eprintln!("DMA3_XFER #{}: {} bytes to {:08X}, first16={:02X?}",
                        m, cdsize, madr, &buf[..16.min(buf.len())]);
                }
            }

            let mut addr = madr & 0x1F_FFFC;
            for chunk in buf.chunks(4) {
                let phys = (addr & 0x1F_FFFF) as usize;
                let len = chunk.len().min(bus.ram.len() - phys);
                bus.ram[phys..phys+len].copy_from_slice(&chunk[..len]);
                addr = addr.wrapping_add(4) & 0x1F_FFFC;
            }
        }
        _ => tracing::warn!("DMA3 unknown chcr: {:08X}", chcr),
    }
    bus.dma_channel_done(3);
}

/// DMA Channel 4: SPU
/// Matches pcsx-redux psxdma.cc dma4() + dmaExec<4> post-transfer updates.
///   CHCR 0x01000201 = RAM → SPU (writeDMAMem)
///   CHCR 0x01000200 = SPU → RAM (readDMAMem)
///   size = (bcr>>16) * (bcr&0xffff) * 2  (in 16-bit words)
pub fn dma_spu(bus: &mut Bus, madr: u32, bcr: u32, chcr: u32) {
    let size = ((bcr >> 16) * (bcr & 0xFFFF) * 2) as usize; // 16-bit word count

    match chcr {
        0x01000201 => {
            // RAM → SPU
            let mut data = vec![0u16; size];
            let mut addr = madr;
            for word in data.iter_mut() {
                let phys = (addr & 0x1F_FFFF) as usize;
                *word = u16::from_le_bytes([bus.ram[phys], bus.ram[phys + 1]]);
                addr = addr.wrapping_add(2);
            }
            bus.spu.dma_write(&data);
            tracing::debug!("DMA4 RAM→SPU: {} halfwords from {:08X}", size, madr);
        }
        0x01000200 => {
            // SPU → RAM
            let mut data = vec![0u16; size];
            bus.spu.dma_read(&mut data);
            let mut addr = madr;
            for &word in &data {
                let phys = (addr & 0x1F_FFFF) as usize;
                let bytes = word.to_le_bytes();
                bus.ram[phys] = bytes[0];
                bus.ram[phys + 1] = bytes[1];
                addr = addr.wrapping_add(2);
            }
            tracing::debug!("DMA4 SPU→RAM: {} halfwords to {:08X}", size, madr);
        }
        _ => {
            tracing::warn!("DMA4 SPU unknown CHCR: {:08X}", chcr);
        }
    }

    // Post-transfer register updates — matching Redux dmaExec<4> template.
    // Mode 1 (block): advance MADR, clear block count in BCR.
    let mode = (chcr >> 9) & 3;
    let block_count = if bcr >> 16 == 0 { 0x10000u32 } else { bcr >> 16 };
    let block_size = bcr & 0xFFFF;
    let total_words = block_count * block_size;
    let new_madr = madr.wrapping_add(total_words * 4);
    bus.write_hw_reg32(0x10C0, new_madr & 0x00FF_FFFF); // DMA4 MADR
    if mode == 0 {
        bus.write_hw_reg32(0x10C4, bcr & 0xFFFF_0000); // keep count, clear size
    } else if mode == 1 {
        bus.write_hw_reg32(0x10C4, bcr & 0x0000_FFFF); // keep size, clear count
    }

    bus.dma_channel_done(4);
}

/// DMA Channel 6: OTC (Ordering Table Clear)
pub fn dma_otc(bus: &mut Bus, madr: u32, bcr: u32, _chcr: u32) {
    let count = if bcr == 0 { 0x10000u32 } else { bcr & 0xFFFF };
    let mut addr = madr & 0x1F_FFFC;

    for i in 0..count {
        let value = if i == count - 1 {
            0x00FF_FFFF
        } else {
            addr.wrapping_sub(4) & 0x1F_FFFF
        };
        let phys = (addr & 0x1F_FFFF) as usize;
        bus.ram[phys..phys+4].copy_from_slice(&value.to_le_bytes());
        addr = addr.wrapping_sub(4) & 0x1F_FFFC;
    }
    tracing::debug!("DMA6 OTC: {} entries from {:08X}", count, madr);
    bus.dma_otc_interrupt();
}
