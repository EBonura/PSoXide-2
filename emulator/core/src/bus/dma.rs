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
pub fn dma_cdrom(bus: &mut Bus, madr: u32, bcr: u32, chcr: u32) {
    match chcr {
        0x11000000 | 0x11400100 => {
            if !bus.cdrom_has_data() {
                tracing::debug!("DMA3 CD-ROM: not ready");
                bus.dma_channel_done(3);
                return;
            }
            let cdsize = ((bcr & 0xFFFF) * 4) as usize;
            let cdsize = if cdsize == 0 { 2048 } else { cdsize };

            let mut buf = vec![0u8; cdsize];
            bus.cdrom.dma_read(&mut buf);

            let mut addr = madr & 0x1F_FFFC;
            for chunk in buf.chunks(4) {
                let phys = (addr & 0x1F_FFFF) as usize;
                let len = chunk.len().min(bus.ram.len() - phys);
                bus.ram[phys..phys+len].copy_from_slice(&chunk[..len]);
                addr = addr.wrapping_add(4) & 0x1F_FFFC;
            }
            tracing::debug!("DMA3 CD-ROM: {} bytes to {:08X}", cdsize, madr);
        }
        _ => tracing::warn!("DMA3 unknown chcr: {:08X}", chcr),
    }
    bus.dma_channel_done(3);
}

/// DMA Channel 4: SPU
pub fn dma_spu(bus: &mut Bus, madr: u32, bcr: u32, chcr: u32) {
    let direction = chcr & 1;
    let block_size = if bcr & 0xFFFF == 0 { 0x10000u32 } else { bcr & 0xFFFF };
    let block_count = if bcr >> 16 == 0 { 0x10000u32 } else { bcr >> 16 };
    let total_words = block_count * block_size;
    let total_bytes = (total_words * 4) as usize;

    if direction == 0 {
        // SPU -> RAM (read from SPU RAM)
        tracing::debug!("DMA4 SPU->RAM: {} bytes from SPU to {:08X}", total_bytes, madr);
        // Stub: fill with zeros
        let mut addr = madr;
        for _ in 0..total_words {
            let phys = (addr & 0x1F_FFFF) as usize;
            bus.ram[phys..phys+4].fill(0);
            addr = addr.wrapping_add(4) & 0x1F_FFFC;
        }
    } else {
        // RAM -> SPU (write to SPU RAM)
        tracing::debug!("DMA4 RAM->SPU: {} bytes from {:08X}", total_bytes, madr);
        // Stub: ignore the data (SPU RAM not modeled for playback yet)
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
