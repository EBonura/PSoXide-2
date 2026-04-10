/// CD-ROM controller — ported from PCSX-Redux cdrom.cc
///
/// Implements the PS1 CD-ROM register interface (4 ports, indexed),
/// command state machine with interrupt scheduling, and sector reading.

// CD-ROM interrupt types (m_stat)
const NO_INTR: u8 = 0;
const DATA_READY: u8 = 1;
const COMPLETE: u8 = 2;
const ACKNOWLEDGE: u8 = 3;
const DATA_END: u8 = 4;
const DISK_ERROR: u8 = 5;

// m_ctrl flags
const BUSYSTS: u8 = 0x80;
const DRQSTS: u8 = 0x40;
const RSLRRDY: u8 = 0x20;
const PRMWRDY: u8 = 0x10;
const PRMEMPT: u8 = 0x08;

// Status flags (m_statP)
const STATUS_PLAY: u8 = 0x80;
const STATUS_SEEK: u8 = 0x40;
const STATUS_READ: u8 = 0x20;
const STATUS_SHELLOPEN: u8 = 0x10;
const STATUS_ROTATING: u8 = 0x02;
const STATUS_ERROR: u8 = 0x01;

// Error codes
const ERROR_NOTREADY: u8 = 0x80;
const ERROR_INVALIDCMD: u8 = 0x40;

// Commands
const CDL_GETSTAT: u8 = 1;
const CDL_SETLOC: u8 = 2;
const CDL_READN: u8 = 6;
const CDL_STOP: u8 = 8;
const CDL_PAUSE: u8 = 9;
const CDL_RESET: u8 = 10;
const CDL_MUTE: u8 = 11;
const CDL_DEMUTE: u8 = 12;
const CDL_SETFILTER: u8 = 13;
const CDL_SETMODE: u8 = 14;
const CDL_GETPARAM: u8 = 15;
const CDL_GETLOCL: u8 = 16;
const CDL_GETLOCP: u8 = 17;
const CDL_GETTN: u8 = 19;
const CDL_GETTD: u8 = 20;
const CDL_SEEKL: u8 = 21;
const CDL_SEEKP: u8 = 22;
const CDL_TEST: u8 = 25;
const CDL_ID: u8 = 26;
const CDL_STANDBY: u8 = 7;
const CDL_READS: u8 = 27;
const CDL_INIT: u8 = 28;
const CDL_READTOC: u8 = 30;

const IRQ_RESCHEDULE: u32 = 0x100;
const TEST20: [u8; 4] = [0x98, 0x06, 0x10, 0xC3];

// Drive states
const DRIVESTATE_STANDBY: u8 = 0;
const DRIVESTATE_LID_OPEN: u8 = 1;
const DRIVESTATE_RESCAN_CD: u8 = 2;
const DRIVESTATE_PREPARE_CD: u8 = 3;
const DRIVESTATE_STOPPED: u8 = 4;

const CD_READ_TIME: u32 = 33_868_800 / 75;

/// Pending interrupt to schedule
pub struct CdInterrupt {
    pub irq_type: CdIrqType,
    pub delay: u32,
}

#[derive(Debug, Clone, Copy)]
pub enum CdIrqType {
    Command,
    Read,
    Lid,
}

pub struct CdRom {
    ctrl: u8,
    stat: u8,
    stat_p: u8,
    reg2: u8,
    cmd: u8,
    param: [u8; 8],
    param_c: u8,
    result: [u8; 16],
    result_c: u8,
    result_p: u8,
    result_ready: u8,
    irq: u16,
    irq_repeated: u8,
    e_cycle: u32,
    drive_state: u8,
    mode: u8,
    muted: bool,
    reading: bool,
    play: bool,
    read: bool,
    seeked: u8,
    transfer: [u8; 2352],
    transfer_index: usize,
    set_sector: [u8; 3],
    set_sector_play: [u8; 3],
    setloc_pending: bool,
    location_changed: bool,
    file: u8,
    channel: u8,
    has_disc: bool,
    pub pending_irqs: Vec<CdInterrupt>,
}

impl CdRom {
    pub fn new() -> Self {
        Self {
            ctrl: 0, stat: NO_INTR, stat_p: STATUS_ROTATING, reg2: 0x1F,
            cmd: 0, param: [0; 8], param_c: 0,
            result: [0; 16], result_c: 0, result_p: 0, result_ready: 0,
            irq: 0, irq_repeated: 0, e_cycle: 0,
            drive_state: DRIVESTATE_STANDBY, mode: 0, muted: false,
            reading: false, play: false, read: false, seeked: 1,
            transfer: [0; 2352], transfer_index: 0,
            set_sector: [0; 3], set_sector_play: [0; 3],
            setloc_pending: false, location_changed: false,
            file: 1, channel: 1, has_disc: false,
            pending_irqs: Vec::new(),
        }
    }

    fn set_result_size(&mut self, size: u8) {
        self.result_p = 0;
        self.result_c = size;
        self.result_ready = 1;
    }

    fn fire_irq(&mut self, set_irq_fn: &mut dyn FnMut(u32)) {
        if self.stat & self.reg2 != 0 {
            set_irq_fn(2);
        }
    }

    fn add_irq_queue(&mut self, irq: u16, ecycle: u32) {
        if self.irq != 0 {
            if irq == self.irq || irq == self.irq.wrapping_add(0x100) {
                self.irq_repeated = 1;
                self.pending_irqs.push(CdInterrupt { irq_type: CdIrqType::Command, delay: ecycle });
                return;
            }
        }
        self.irq = irq;
        self.e_cycle = ecycle;
        self.pending_irqs.push(CdInterrupt { irq_type: CdIrqType::Command, delay: ecycle });
    }

    fn stop_reading(&mut self) {
        self.reading = false;
        self.stat_p &= !(STATUS_READ | STATUS_SEEK);
    }

    fn stop_cdda(&mut self) {
        if self.play { self.stat_p &= !STATUS_PLAY; self.play = false; }
    }

    // ======== Register reads ========

    pub fn read(&mut self, port: u32) -> u8 {
        match port {
            0 => {
                if self.result_ready != 0 { self.ctrl |= RSLRRDY; } else { self.ctrl &= !RSLRRDY; }
                self.ctrl |= PRMEMPT | PRMWRDY;
                self.ctrl
            }
            1 => {
                let ret = if (self.result_p & 0xF) < self.result_c {
                    self.result[(self.result_p & 0xF) as usize]
                } else { 0 };
                self.result_p += 1;
                if self.result_p == self.result_c { self.result_ready = 0; }
                ret
            }
            2 => {
                if !self.read { self.ctrl &= !DRQSTS; 0 }
                else {
                    let ret = self.transfer[self.transfer_index];
                    self.transfer_index += 1;
                    self.adjust_transfer_index();
                    ret
                }
            }
            3 => {
                if self.ctrl & 1 != 0 { self.stat | 0xE0 } else { self.reg2 | 0xE0 }
            }
            _ => 0,
        }
    }

    // ======== Register writes ========

    pub fn write(&mut self, port: u32, value: u8, set_irq_fn: &mut dyn FnMut(u32)) {
        match port {
            0 => self.ctrl = (value & 3) | (self.ctrl & !3),
            1 => {
                if self.ctrl & 3 != 0 { return; }
                self.cmd = value;
                self.result_ready = 0;
                self.ctrl |= BUSYSTS;
                self.add_irq_queue(self.cmd as u16, 0x800);
                match value {
                    CDL_READN | CDL_READS | CDL_PAUSE => { self.stop_cdda(); self.stop_reading(); }
                    CDL_INIT | CDL_RESET => { self.seeked = 1; self.stop_cdda(); self.stop_reading(); }
                    CDL_SETMODE => { self.mode = self.param[0]; }
                    CDL_SETLOC if self.param_c >= 3 => {
                        self.set_sector = [self.param[0], self.param[1], self.param[2]];
                        self.setloc_pending = true;
                    }
                    _ => {}
                }
            }
            2 => match self.ctrl & 3 {
                0 => { if self.param_c < 8 { self.param[self.param_c as usize] = value; self.param_c += 1; } }
                1 => { self.reg2 = value; self.fire_irq(set_irq_fn); }
                _ => {}
            }
            3 => match self.ctrl & 3 {
                0 => {
                    if value & 0x80 != 0 && !self.read {
                        self.read = true;
                        self.transfer_index = match self.mode & 0x30 { 0x20 => 0, _ => 12 };
                    }
                }
                1 => {
                    self.stat &= !value;
                    if value & 0x40 != 0 { self.param_c = 0; }
                }
                _ => {}
            }
            _ => {}
        }
    }

    fn adjust_transfer_index(&mut self) {
        let sz = match self.mode & 0x30 { 0x20 => 2340, 0x10 => 12 + 2328, _ => 12 + 2048 };
        if self.transfer_index >= sz { self.transfer_index -= sz; }
        if self.transfer_index == 0 { self.ctrl &= !DRQSTS; self.read = false; }
    }

    // ======== Command interrupt ========

    pub fn interrupt(&mut self, set_irq_fn: &mut dyn FnMut(u32)) {
        let irq = self.irq;
        if self.stat != 0 {
            self.pending_irqs.push(CdInterrupt { irq_type: CdIrqType::Command, delay: IRQ_RESCHEDULE });
            return;
        }
        self.ctrl &= !BUSYSTS;
        self.set_result_size(1);
        self.result[0] = self.stat_p;
        self.stat = ACKNOWLEDGE;

        if self.irq_repeated != 0 {
            self.irq_repeated = 0;
            self.pending_irqs.push(CdInterrupt { irq_type: CdIrqType::Command, delay: self.e_cycle });
            self.fire_irq(set_irq_fn);
            self.param_c = 0;
            return;
        }
        self.irq = 0;

        let mut no_busy_error = false;
        let mut start_rotating = false;

        // Match on full u16 irq — the 0x100 offset distinguishes first-phase
        // responses from delayed (second-phase) responses.
        // Constants are u8, so cast to u16 for matching.
        let cmd = irq;
        match cmd {
            // ======== First-phase responses (irq < 0x100) ========
            x if x == CDL_GETSTAT as u16 => {
                if self.drive_state != DRIVESTATE_LID_OPEN { self.stat_p &= !STATUS_SHELLOPEN; }
                no_busy_error = true;
            }
            x if x == CDL_SETLOC as u16 || x == CDL_SETFILTER as u16
                || x == CDL_MUTE as u16 || x == CDL_DEMUTE as u16 => {}
            x if x == CDL_SETMODE as u16 => { no_busy_error = true; }
            x if x == CDL_GETPARAM as u16 => {
                self.set_result_size(5);
                self.result[1] = self.mode; self.result[2] = 0;
                self.result[3] = self.file; self.result[4] = self.channel;
                no_busy_error = true;
            }
            x if x == CDL_GETLOCL as u16 => { self.set_result_size(8); self.result[..8].copy_from_slice(&self.transfer[..8]); }
            x if x == CDL_GETLOCP as u16 => { self.set_result_size(8); self.result[..8].fill(0); }
            x if x == CDL_GETTN as u16 => {
                if !self.has_disc { self.stat = DISK_ERROR; self.result[0] |= STATUS_ERROR; }
                else { self.set_result_size(3); self.result[1] = 1; self.result[2] = 1; }
            }
            x if x == CDL_GETTD as u16 => {
                if !self.has_disc { self.stat = DISK_ERROR; self.result[0] |= STATUS_ERROR; }
                else { self.set_result_size(4); self.result[0] = self.stat_p; self.result[1] = 0; self.result[2] = 2; }
            }
            x if x == CDL_TEST as u16 => {
                if self.param[0] == 0x20 { self.set_result_size(4); self.result[..4].copy_from_slice(&TEST20); }
                no_busy_error = true;
            }
            x if x == CDL_ID as u16 => { self.add_irq_queue(CDL_ID as u16 + 0x100, 20480); }
            x if x == CDL_INIT as u16 => {
                self.stat_p |= STATUS_SHELLOPEN;
                self.drive_state = DRIVESTATE_RESCAN_CD;
                self.pending_irqs.push(CdInterrupt { irq_type: CdIrqType::Lid, delay: 20480 });
                no_busy_error = true; start_rotating = true;
            }
            x if x == CDL_RESET as u16 => {
                self.muted = false; self.mode = 0x20;
                self.add_irq_queue(CDL_RESET as u16 + 0x100, 4100000);
                no_busy_error = true; start_rotating = true;
            }
            x if x == CDL_STOP as u16 => {
                self.stop_cdda(); self.stop_reading();
                self.drive_state = DRIVESTATE_STOPPED;
                self.add_irq_queue(CDL_STOP as u16 + 0x100, 0x800);
            }
            x if x == CDL_PAUSE as u16 => {
                let d = if self.drive_state == DRIVESTATE_STANDBY { 7000 } else { 1000000 };
                self.add_irq_queue(CDL_PAUSE as u16 + 0x100, d);
                self.ctrl |= BUSYSTS;
            }
            x if x == CDL_STANDBY as u16 => {
                self.add_irq_queue(CDL_STANDBY as u16 + 0x100, CD_READ_TIME * 125 / 2);
                start_rotating = true;
            }
            x if x == CDL_SEEKL as u16 || x == CDL_SEEKP as u16 => {
                self.stop_cdda(); self.stop_reading();
                self.stat_p |= STATUS_SEEK; self.seeked = 0;
                start_rotating = true;
            }
            x if x == CDL_READN as u16 || x == CDL_READS as u16 => {
                if self.setloc_pending {
                    self.set_sector_play = self.set_sector;
                    self.setloc_pending = false; self.location_changed = true;
                }
                self.reading = true; self.stat_p |= STATUS_READ; self.stat_p &= !STATUS_SEEK;
                self.result[0] = self.stat_p; start_rotating = true;
                let delay = if self.mode & 0x80 != 0 { CD_READ_TIME } else { CD_READ_TIME * 2 };
                self.pending_irqs.push(CdInterrupt { irq_type: CdIrqType::Read, delay });
            }
            x if x == CDL_READTOC as u16 => {
                self.add_irq_queue(CDL_READTOC as u16 + 0x100, CD_READ_TIME * 180 / 4);
                no_busy_error = true; start_rotating = true;
            }

            // ======== Delayed (second-phase) responses (irq >= 0x100) ========
            x if x == CDL_ID as u16 + 0x100 => {
                self.set_result_size(8);
                if !self.has_disc {
                    self.result[0] = 0x08; self.result[1] = 0x40; self.result[2..8].fill(0);
                    self.stat = DISK_ERROR;
                } else {
                    self.result[0] = self.stat_p; self.result[1] = 0; self.result[2] = 0; self.result[3] = 0;
                    self.result[4..8].copy_from_slice(b"PCSX");
                    self.stat = COMPLETE;
                }
            }
            x if x == CDL_RESET as u16 + 0x100 => {
                self.stat = COMPLETE;
            }
            x if x == CDL_STOP as u16 + 0x100 => {
                self.stat_p &= !STATUS_ROTATING; self.result[0] = self.stat_p;
                self.stat = COMPLETE;
            }
            x if x == CDL_PAUSE as u16 + 0x100 => {
                self.stat_p &= !STATUS_READ; self.result[0] = self.stat_p;
                self.stat = COMPLETE;
            }
            x if x == CDL_STANDBY as u16 + 0x100 => {
                self.stat = COMPLETE;
            }
            x if x == CDL_READTOC as u16 + 0x100 => {
                self.stat = COMPLETE;
                no_busy_error = true;
            }

            // ======== Unknown command ========
            _ => {
                self.set_result_size(2);
                self.result[0] = self.stat_p | STATUS_ERROR;
                self.result[1] = ERROR_INVALIDCMD;
                self.stat = DISK_ERROR;
            }
        }

        if self.drive_state == DRIVESTATE_STOPPED && start_rotating {
            self.drive_state = DRIVESTATE_STANDBY;
            self.stat_p |= STATUS_ROTATING;
        }
        if !no_busy_error {
            match self.drive_state {
                DRIVESTATE_LID_OPEN | DRIVESTATE_RESCAN_CD | DRIVESTATE_PREPARE_CD => {
                    self.set_result_size(2);
                    self.result[0] = self.stat_p | STATUS_ERROR;
                    self.result[1] = ERROR_NOTREADY;
                    self.stat = DISK_ERROR;
                }
                _ => {}
            }
        }
        self.fire_irq(set_irq_fn);
        self.param_c = 0;
    }

    /// Read sector interrupt
    pub fn read_interrupt(&mut self, set_irq_fn: &mut dyn FnMut(u32)) {
        if !self.reading { return; }
        if self.irq != 0 || self.stat != 0 {
            self.pending_irqs.push(CdInterrupt { irq_type: CdIrqType::Read, delay: IRQ_RESCHEDULE });
            return;
        }
        self.set_result_size(1);
        self.stat_p |= STATUS_READ | STATUS_ROTATING;
        self.stat_p &= !STATUS_SEEK;
        self.result[0] = self.stat_p;
        self.seeked = 1;
        self.transfer.fill(0);
        self.ctrl |= DRQSTS;
        self.read = false;
        let delay = if self.mode & 0x80 != 0 { CD_READ_TIME / 2 } else { CD_READ_TIME };
        self.pending_irqs.push(CdInterrupt {
            irq_type: CdIrqType::Read,
            delay: if self.location_changed { self.location_changed = false; delay * 30 } else { delay },
        });
        self.stat = DATA_READY;
        self.fire_irq(set_irq_fn);
    }

    /// Lid/seek state machine
    pub fn lid_seek_interrupt(&mut self) {
        match self.drive_state {
            DRIVESTATE_STANDBY => { self.stat_p &= !STATUS_SEEK; }
            DRIVESTATE_RESCAN_CD => {
                self.stat_p |= STATUS_ROTATING;
                self.drive_state = DRIVESTATE_PREPARE_CD;
                self.pending_irqs.push(CdInterrupt { irq_type: CdIrqType::Lid, delay: CD_READ_TIME * 150 });
            }
            DRIVESTATE_PREPARE_CD => {
                self.stat_p |= STATUS_SEEK;
                self.drive_state = DRIVESTATE_STANDBY;
                self.pending_irqs.push(CdInterrupt { irq_type: CdIrqType::Lid, delay: CD_READ_TIME * 26 });
            }
            _ => {}
        }
    }

    pub fn read_ctrl_drq(&self) -> bool {
        self.ctrl & DRQSTS != 0
    }

    pub fn dma_read(&mut self, dest: &mut [u8]) {
        if !self.read { return; }
        for byte in dest.iter_mut() {
            *byte = self.transfer[self.transfer_index];
            self.transfer_index += 1;
            self.adjust_transfer_index();
        }
    }
}
