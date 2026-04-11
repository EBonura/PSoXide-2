/// SIO (Serial I/O) controller — ported from PCSX-Redux sio.cc/pad.cc
///
/// Handles controller polling and memory card communication.
/// The PS1 SIO is a serial port at 0x1F801040-0x1F80104E.

use std::collections::VecDeque;

// Status flags
const TX_DATACLEAR: u32 = 0x0001;
const RX_FIFONOTEMPTY: u32 = 0x0002;
const TX_FINISHED: u32 = 0x0004;
const RX_PARITYERR: u32 = 0x0008;
const ACK_INPUT: u32 = 0x0080;
const IRQ_FLAG: u32 = 0x0200;

// Control flags
const TX_ENABLE: u16 = 0x0001;
const SELECT_ENABLE: u16 = 0x0002;
const RESET_ERR: u16 = 0x0010;
const RESET: u16 = 0x0040;
const TX_IRQEN: u16 = 0x0400;
const RX_IRQEN: u16 = 0x0800;
const ACK_IRQEN: u16 = 0x1000;
const WHICH_PORT: u16 = 0x2000;

// Pad states
const PAD_STATE_IDLE: u32 = 0;
const PAD_STATE_READ_COMMAND: u32 = 1;
const PAD_STATE_READ_DATA: u32 = 2;

// Device types
const DEVICE_NONE: u8 = 0;
const DEVICE_PAD: u8 = 0x01;
const DEVICE_MEMCARD: u8 = 0x81;
const DEVICE_IGNORE: u8 = 0xFF;

/// Digital pad button state (active-low: 0 = pressed, 1 = released)
#[derive(Clone, Copy)]
pub struct PadButtons(pub u16);

impl PadButtons {
    pub fn new() -> Self { Self(0xFFFF) } // all released

    pub fn set_pressed(&mut self, btn: PadButton, pressed: bool) {
        if pressed {
            self.0 &= !(1 << btn as u32);
        } else {
            self.0 |= 1 << btn as u32;
        }
    }
}

#[repr(u32)]
#[derive(Clone, Copy)]
pub enum PadButton {
    Select = 0,
    L3 = 1,
    R3 = 2,
    Start = 3,
    Up = 4,
    Right = 5,
    Down = 6,
    Left = 7,
    L2 = 8,
    R2 = 9,
    L1 = 10,
    R1 = 11,
    Triangle = 12,
    Circle = 13,
    Cross = 14,
    Square = 15,
}

pub struct Sio {
    // Registers
    pub status: u32,
    pub mode: u16,
    pub control: u16,
    pub baud: u16,
    data: u8,

    // FIFO
    rx_fifo: VecDeque<u8>,

    // Pad state machine
    pad_state: u32,
    current_device: u8,
    buffer: [u8; 256],
    buffer_index: u32,
    max_buffer_index: u32,

    // Connected pads
    pub pad1: PadButtons,
    pub pad2: PadButtons,
    pub pad1_connected: bool,
    pub pad2_connected: bool,

    // Pending interrupt
    pub pending_irq: bool,
    pub pending_irq_delay: u32,
}

impl Sio {
    pub fn new() -> Self {
        Self {
            status: TX_DATACLEAR | TX_FINISHED,
            mode: 0,
            control: 0,
            baud: 0,
            data: 0,
            rx_fifo: VecDeque::new(),
            pad_state: PAD_STATE_IDLE,
            current_device: DEVICE_NONE,
            buffer: [0; 256],
            buffer_index: 0,
            max_buffer_index: 0,
            pad1: PadButtons::new(),
            pad2: PadButtons::new(),
            pad1_connected: true,
            pad2_connected: false,
            pending_irq: false,
            pending_irq_delay: 0,
        }
    }

    pub fn reset(&mut self) {
        self.rx_fifo.clear();
        self.pad_state = PAD_STATE_IDLE;
        self.status = TX_DATACLEAR | TX_FINISHED;
        self.mode = 0;
        self.control = 0;
        self.baud = 0;
        self.buffer_index = 0;
        self.current_device = DEVICE_NONE;
    }

    fn sio_cycles(&self) -> u32 {
        (self.baud as u32).max(1) * 8
    }

    fn acknowledge(&mut self) {
        if self.control & TX_ENABLE != 0 && self.control & ACK_IRQEN != 0 {
            self.pending_irq = true;
            self.pending_irq_delay = self.sio_cycles();
        }
    }

    fn is_transmit_ready(&self) -> bool {
        let tx_enabled = self.control & TX_ENABLE != 0;
        let tx_finished = self.status & TX_FINISHED != 0;
        let tx_data_pending = self.status & TX_DATACLEAR == 0;
        tx_enabled && tx_finished && tx_data_pending
    }

    /// pcsx-redux isReceiveIRQReady: checks RX_IRQEN and FIFO fill level
    /// against the RX IRQ mode (ctrl bits 9-8).
    fn is_receive_irq_ready(&self) -> bool {
        if self.control & RX_IRQEN == 0 {
            return false;
        }
        let mode = (self.control >> 8) & 3;
        let threshold = match mode {
            0 => 1,
            1 => 2,
            2 => 4,
            3 => 8,
            _ => unreachable!(),
        };
        self.rx_fifo.len() >= threshold
    }

    fn update_fifo_status(&mut self) {
        if self.rx_fifo.is_empty() {
            self.status &= !RX_FIFONOTEMPTY;
        } else {
            self.status |= RX_FIFONOTEMPTY;
        }
    }

    fn write_pad(&mut self, value: u8) {
        let port2 = self.control & WHICH_PORT != 0;
        let connected = if port2 { self.pad2_connected } else { self.pad1_connected };
        let buttons = if port2 { self.pad2 } else { self.pad1 };

        match self.pad_state {
            PAD_STATE_IDLE => {
                self.status |= RX_FIFONOTEMPTY;
                if !connected {
                    self.buffer[0] = 0xFF;
                    return;
                }
                self.buffer[0] = 0xFF; // initial response (will be overwritten)
                self.max_buffer_index = 2;
                self.buffer_index = 0;
                self.pad_state = PAD_STATE_READ_COMMAND;
            }
            PAD_STATE_READ_COMMAND => {
                self.pad_state = PAD_STATE_READ_DATA;
                self.buffer_index = 1;
                // Respond with pad ID based on command
                match value {
                    0x42 => {
                        // Read pad
                        self.buffer[1] = 0x41; // Digital pad ID (upper nibble=4, lower=1 halfword)
                        self.max_buffer_index = 2 + 2; // 2 bytes of button data
                        // Fill button data
                        self.buffer[2] = 0x5A; // always 0x5A
                        self.buffer[3] = (buttons.0 & 0xFF) as u8;
                        self.buffer[4] = (buttons.0 >> 8) as u8;
                    }
                    _ => {
                        self.buffer[1] = 0xFF; // unknown command
                        self.pad_state = PAD_STATE_IDLE;
                        self.current_device = DEVICE_IGNORE;
                        return;
                    }
                }
            }
            PAD_STATE_READ_DATA => {
                self.buffer_index += 1;
                if self.buffer_index >= self.max_buffer_index {
                    self.pad_state = PAD_STATE_IDLE;
                    self.current_device = DEVICE_IGNORE;
                    return;
                }
            }
            _ => return,
        }

        self.acknowledge();
    }

    fn transmit_data(&mut self) {
        self.status &= !TX_FINISHED;

        let mut rx_byte = 0xFF_u8;

        if self.current_device == DEVICE_NONE {
            self.current_device = self.data;
        }

        match self.current_device {
            DEVICE_PAD => {
                self.write_pad(self.data);
                rx_byte = self.buffer[self.buffer_index as usize];
            }
            DEVICE_MEMCARD => {
                // Memory card not implemented — return 0xFF (no card)
                self.current_device = DEVICE_IGNORE;
            }
            DEVICE_IGNORE => {}
            _ => {
                self.current_device = DEVICE_NONE;
                self.pad_state = PAD_STATE_IDLE;
            }
        }

        self.rx_fifo.push_back(rx_byte);
        self.update_fifo_status();
        self.data = rx_byte;

        // pcsx-redux: fire receive IRQ if FIFO threshold met and IRQ not already set
        if self.is_receive_irq_ready() && self.status & IRQ_FLAG == 0 {
            self.pending_irq = true;
            self.pending_irq_delay = self.sio_cycles();
        }

        self.status |= TX_DATACLEAR | TX_FINISHED;
    }

    // ======== Register interface ========

    pub fn read8(&mut self) -> u8 {
        let ret = if self.status & RX_FIFONOTEMPTY != 0 && !self.rx_fifo.is_empty() {
            self.rx_fifo.pop_front().unwrap_or(0xFF)
        } else {
            0xFF
        };
        self.update_fifo_status();
        ret
    }

    pub fn read_status16(&self) -> u16 {
        self.status as u16
    }

    pub fn read_status32(&self) -> u32 {
        self.status
    }

    pub fn read_mode16(&self) -> u16 {
        self.mode
    }

    pub fn read_ctrl16(&self) -> u16 {
        self.control
    }

    pub fn read_baud16(&self) -> u16 {
        self.baud
    }

    pub fn write8(&mut self, value: u8) {
        self.data = value;
        self.status &= !TX_DATACLEAR;

        let ready = self.is_transmit_ready();
        {
            use std::sync::atomic::{AtomicU32, Ordering};
            static NS: AtomicU32 = AtomicU32::new(0);
            let n = NS.fetch_add(1, Ordering::Relaxed);
            if n < 20 {
                eprintln!("SIO_W8 #{}: val={:02X} ctrl={:04X} stat={:04X} tx_ready={} dev={:02X} pad_st={}",
                    n, value, self.control, self.status, ready, self.current_device, self.pad_state);
            }
        }
        if ready {
            self.transmit_data();
        }
    }

    pub fn write_mode16(&mut self, value: u16) {
        self.mode = value;
    }

    pub fn write_ctrl16(&mut self, value: u16) {
        let deselected = self.control & SELECT_ENABLE != 0 && value & SELECT_ENABLE == 0;
        let selected = self.control & SELECT_ENABLE == 0 && value & SELECT_ENABLE != 0;
        // pcsx-redux: port changed = was port2, now port1
        let port_changed = self.control & WHICH_PORT != 0 && value & WHICH_PORT == 0;
        let was_ready = self.is_transmit_ready();

        self.control = value;

        if selected && self.control & TX_IRQEN != 0 && self.status & IRQ_FLAG == 0 {
            self.pending_irq = true;
            self.pending_irq_delay = self.sio_cycles();
        }

        // pcsx-redux: deselected || portChanged triggers full state reset
        if deselected || port_changed {
            self.current_device = DEVICE_NONE;
            self.pad_state = PAD_STATE_IDLE;
            self.buffer_index = 0;
        }

        if self.control & RESET_ERR != 0 {
            self.status &= !(RX_PARITYERR | IRQ_FLAG);
            self.control &= !RESET_ERR;
            // pcsx-redux: re-check if receive IRQ should fire after clearing flags
            if self.is_receive_irq_ready() {
                self.status |= IRQ_FLAG;
            }
        }

        if self.control & RESET != 0 {
            self.rx_fifo.clear();
            self.pad_state = PAD_STATE_IDLE;
            self.buffer_index = 0;
            self.status = TX_DATACLEAR | TX_FINISHED;
            self.current_device = DEVICE_NONE;
            self.pending_irq = false;
        }

        self.update_fifo_status();

        if self.control & TX_IRQEN != 0 {
            self.status |= IRQ_FLAG;
        }

        if !was_ready && self.is_transmit_ready() {
            self.transmit_data();
        }
    }

    pub fn write_baud16(&mut self, value: u16) {
        self.baud = value;
    }

    /// Called when the SIO interrupt fires (from scheduler)
    pub fn interrupt(&mut self) {
        self.status |= IRQ_FLAG;
        // The bus should set IRQ bit 7 (SIO0)
    }

    /// Debug dump for diagnostics
    pub fn debug_dump(&self) -> String {
        format!("status={:04X} ctrl={:04X} mode={:04X} baud={:04X} data={:02X} fifo_len={} pad_state={} device={:02X} pending_irq={} pending_delay={}",
            self.status, self.control, self.mode, self.baud, self.data,
            self.rx_fifo.len(), self.pad_state, self.current_device,
            self.pending_irq, self.pending_irq_delay)
    }
}
