use super::Bus;

pub struct DmaController;

impl DmaController {
    pub fn new() -> Self {
        Self
    }
}

/// DMA Channel 2: GPU
pub fn dma_gpu(bus: &mut Bus, madr: u32, bcr: u32, chcr: u32) {
    let direction = chcr & 1; // 0 = to RAM, 1 = from RAM (to device)
    let mode = (chcr >> 9) & 3;

    match mode {
        1 => {
            // Block mode
            let block_size = (bcr & 0xFFFF) as u32;
            let block_count = (bcr >> 16) as u32;
            let total_words = if block_count == 0 { 0x10000 } else { block_count } *
                              if block_size == 0 { 0x10000 } else { block_size };

            if direction == 1 {
                // RAM -> GPU (send data to GPU)
                let mut addr = madr;
                for _ in 0..total_words {
                    let word = bus.read32(0x8000_0000 | addr);
                    bus.gpu.gp0_write(word);
                    addr = addr.wrapping_add(4) & 0x1F_FFFC;
                }
            } else {
                // GPU -> RAM (read from GPU)
                let mut addr = madr;
                for _ in 0..total_words {
                    let word = bus.gpu.read_data();
                    let phys = addr & 0x1F_FFFF;
                    bus.ram[phys as usize..phys as usize + 4]
                        .copy_from_slice(&word.to_le_bytes());
                    addr = addr.wrapping_add(4) & 0x1F_FFFC;
                }
            }

            tracing::debug!("DMA2 block: {} words, dir={}", total_words, direction);
        }
        2 => {
            // Linked-list mode (GPU command lists)
            if direction != 1 {
                tracing::warn!("DMA2 linked-list in wrong direction");
                return;
            }

            let mut addr = madr & 0x1F_FFFC;
            let mut count = 0u32;

            loop {
                let header = {
                    let phys = addr & 0x1F_FFFF;
                    u32::from_le_bytes([
                        bus.ram[phys as usize],
                        bus.ram[phys as usize + 1],
                        bus.ram[phys as usize + 2],
                        bus.ram[phys as usize + 3],
                    ])
                };

                let num_words = (header >> 24) as u32;

                for i in 1..=num_words {
                    let word_addr = addr.wrapping_add(i * 4) & 0x1F_FFFC;
                    let phys = word_addr & 0x1F_FFFF;
                    let word = u32::from_le_bytes([
                        bus.ram[phys as usize],
                        bus.ram[phys as usize + 1],
                        bus.ram[phys as usize + 2],
                        bus.ram[phys as usize + 3],
                    ]);
                    bus.gpu.gp0_write(word);
                }

                count += 1;
                if count > 0x20_0000 {
                    tracing::error!("DMA2 linked-list infinite loop detected");
                    break;
                }

                if header & 0x00FF_FFFF == 0x00FF_FFFF {
                    break; // Terminator
                }
                addr = header & 0x1F_FFFC;
            }

            tracing::debug!("DMA2 linked-list: {} nodes", count);
        }
        _ => {
            tracing::warn!("DMA2 unhandled mode {}", mode);
        }
    }

    // Signal completion
    bus.dma_gpu_interrupt();
}

/// DMA Channel 6: OTC (Ordering Table Clear)
pub fn dma_otc(bus: &mut Bus, madr: u32, bcr: u32, _chcr: u32) {
    let count = if bcr == 0 { 0x10000 } else { bcr & 0xFFFF };
    let mut addr = madr & 0x1F_FFFC;

    tracing::debug!("DMA6 OTC: MADR={:08X}, count={}", madr, count);

    for i in 0..count {
        let value = if i == count - 1 {
            0x00FF_FFFF // Terminator
        } else {
            addr.wrapping_sub(4) & 0x1F_FFFF
        };

        let phys = (addr & 0x1F_FFFF) as usize;
        bus.ram[phys..phys + 4].copy_from_slice(&value.to_le_bytes());
        addr = addr.wrapping_sub(4) & 0x1F_FFFC;
    }

    bus.dma_otc_interrupt();
}
