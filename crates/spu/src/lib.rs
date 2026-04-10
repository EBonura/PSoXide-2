pub struct Spu {
    pub ram: Box<[u8; 0x8_0000]>, // 512KB
    pub spucnt: u16,
    pub spustat: u16,
    pub main_volume_left: i16,
    pub main_volume_right: i16,
}

impl Spu {
    pub fn new() -> Self {
        Self {
            ram: vec![0u8; 0x8_0000].into_boxed_slice().try_into().unwrap(),
            spucnt: 0,
            spustat: 0,
            main_volume_left: 0,
            main_volume_right: 0,
        }
    }

    pub fn read16(&self, offset: u32) -> u16 {
        match offset {
            0x1AA => self.spucnt,
            0x1AE => self.spustat,
            _ => {
                tracing::warn!("SPU read16 unhandled offset: {:03X}", offset);
                0
            }
        }
    }

    pub fn write16(&mut self, offset: u32, value: u16) {
        match offset {
            0x1AA => {
                self.spucnt = value;
                // Mirror enable bit into SPUSTAT
                self.spustat = (self.spustat & !0x3F) | (value & 0x3F);
            }
            0x1AC => { /* SPU transfer control */ }
            _ => {
                tracing::trace!("SPU write16 offset: {:03X} = {:04X}", offset, value);
            }
        }
    }
}
