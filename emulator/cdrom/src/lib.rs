pub struct CdRom {
    pub index: u8,
    pub status: u8,
    pub interrupt_enable: u8,
    pub interrupt_flag: u8,
    pub param_fifo: Vec<u8>,
    pub response_fifo: Vec<u8>,
}

impl CdRom {
    pub fn new() -> Self {
        Self {
            index: 0,
            status: 0x18, // Parameter fifo empty, response fifo empty
            interrupt_enable: 0,
            interrupt_flag: 0,
            param_fifo: Vec::new(),
            response_fifo: Vec::new(),
        }
    }

    pub fn read(&self, port: u32) -> u8 {
        match port {
            0 => self.status,
            1 => {
                if let Some(&val) = self.response_fifo.first() {
                    val
                } else {
                    0
                }
            }
            3 => match self.index & 1 {
                0 => self.interrupt_enable,
                1 => self.interrupt_flag | 0xE0,
                _ => unreachable!(),
            },
            _ => {
                tracing::warn!("CDROM read port {} index {}", port, self.index);
                0
            }
        }
    }

    pub fn write(&mut self, port: u32, value: u8) {
        match port {
            0 => self.index = value & 3,
            1 => match self.index {
                0 => self.execute_command(value),
                _ => tracing::warn!("CDROM write port 1 index {}: {:02X}", self.index, value),
            },
            2 => match self.index {
                0 => self.param_fifo.push(value),
                1 => self.interrupt_enable = value,
                _ => tracing::warn!("CDROM write port 2 index {}: {:02X}", self.index, value),
            },
            3 => match self.index {
                1 => {
                    self.interrupt_flag &= !value;
                    if value & 0x40 != 0 {
                        self.param_fifo.clear();
                    }
                }
                _ => tracing::warn!("CDROM write port 3 index {}: {:02X}", self.index, value),
            },
            _ => tracing::warn!("CDROM write port {} index {}: {:02X}", port, self.index, value),
        }
    }

    fn execute_command(&mut self, cmd: u8) {
        tracing::debug!("CDROM command: {:02X}", cmd);
        self.response_fifo.clear();
        match cmd {
            0x01 => {
                // GetStat
                self.response_fifo.push(0x02); // motor on
                self.interrupt_flag = 3;
            }
            0x19 => {
                // Test
                let sub = self.param_fifo.first().copied().unwrap_or(0);
                self.param_fifo.clear();
                match sub {
                    0x20 => {
                        // Get CDROM BIOS date
                        self.response_fifo.extend_from_slice(&[0x94, 0x09, 0x19, 0xC0]);
                        self.interrupt_flag = 3;
                    }
                    _ => {
                        tracing::warn!("CDROM Test sub-command: {:02X}", sub);
                        self.interrupt_flag = 5; // error
                    }
                }
            }
            0x0A => {
                // Init
                self.response_fifo.push(0x02);
                self.interrupt_flag = 3;
            }
            _ => {
                tracing::warn!("CDROM unhandled command: {:02X}", cmd);
                self.response_fifo.push(0x02);
                self.interrupt_flag = 3;
            }
        }
        self.status = (self.status & !0x38) | if self.response_fifo.is_empty() { 0 } else { 0x20 };
    }
}
