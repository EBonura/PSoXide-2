use crate::display::DisplayConfig;
use crate::status::GpuStatus;
use crate::vram::Vram;

#[derive(Debug, Clone, Copy, PartialEq)]
enum GpuState {
    Idle,
    ReceivingCommand { cmd: u8, words_remaining: u32 },
    CpuToVram { x: u16, y: u16, w: u16, h: u16, current_x: u16, current_y: u16 },
    VramToCpu { x: u16, y: u16, w: u16, h: u16, current_x: u16, current_y: u16 },
}

pub struct CommandProcessor {
    state: GpuState,
    command_buffer: Vec<u32>,
    // Drawing environment
    pub texpage: u32,
    pub draw_area_left: i16,
    pub draw_area_top: i16,
    pub draw_area_right: i16,
    pub draw_area_bottom: i16,
    pub draw_offset_x: i16,
    pub draw_offset_y: i16,
    pub mask_set: bool,
    pub mask_check: bool,
    pub texture_window: u32,
}

impl CommandProcessor {
    pub fn new() -> Self {
        Self {
            state: GpuState::Idle,
            command_buffer: Vec::with_capacity(16),
            texpage: 0,
            draw_area_left: 0,
            draw_area_top: 0,
            draw_area_right: 0,
            draw_area_bottom: 0,
            draw_offset_x: 0,
            draw_offset_y: 0,
            mask_set: false,
            mask_check: false,
            texture_window: 0,
        }
    }

    pub fn gp0_write(&mut self, vram: &mut Vram, status: &mut GpuStatus, display: &mut DisplayConfig, data: u32) {
        match self.state {
            GpuState::Idle => self.gp0_command(vram, status, display, data),
            GpuState::ReceivingCommand { cmd, words_remaining } => {
                self.command_buffer.push(data);
                let remaining = words_remaining - 1;
                if remaining == 0 {
                    self.state = GpuState::Idle;
                    self.execute_command(vram, cmd);
                } else {
                    self.state = GpuState::ReceivingCommand { cmd, words_remaining: remaining };
                }
            }
            GpuState::CpuToVram { x, y, w, h, mut current_x, mut current_y } => {
                // Write 2 pixels per word (16-bit each)
                for i in 0..2u32 {
                    let pixel = if i == 0 { data as u16 } else { (data >> 16) as u16 };
                    let px = x.wrapping_add(current_x);
                    let py = y.wrapping_add(current_y);
                    vram.set_pixel(px, py, pixel);

                    current_x += 1;
                    if current_x >= w {
                        current_x = 0;
                        current_y += 1;
                        if current_y >= h {
                            self.state = GpuState::Idle;
                            return;
                        }
                    }
                }
                self.state = GpuState::CpuToVram { x, y, w, h, current_x, current_y };
            }
            GpuState::VramToCpu { .. } => {
                // Shouldn't receive writes in this state
            }
        }
    }

    fn gp0_command(&mut self, vram: &mut Vram, status: &mut GpuStatus, _display: &mut DisplayConfig, data: u32) {
        let cmd = (data >> 24) as u8;
        match cmd {
            0x00 => {} // NOP

            // Fill rectangle in VRAM
            0x02 => {
                self.command_buffer.clear();
                self.command_buffer.push(data);
                self.state = GpuState::ReceivingCommand { cmd, words_remaining: 2 };
            }

            // Polygons: flat/gouraud, 3/4 vertex, textured/untextured, semi-trans
            0x20..=0x3F => {
                let words = polygon_word_count(cmd);
                self.command_buffer.clear();
                self.command_buffer.push(data);
                if words > 1 {
                    self.state = GpuState::ReceivingCommand { cmd, words_remaining: words - 1 };
                } else {
                    self.execute_command(vram, cmd);
                }
            }

            // Lines
            0x40..=0x5F => {
                let words = line_word_count(cmd);
                self.command_buffer.clear();
                self.command_buffer.push(data);
                if words > 1 {
                    self.state = GpuState::ReceivingCommand { cmd, words_remaining: words - 1 };
                } else {
                    self.execute_command(vram, cmd);
                }
            }

            // Rectangles/Sprites
            0x60..=0x7F => {
                let words = rect_word_count(cmd);
                self.command_buffer.clear();
                self.command_buffer.push(data);
                if words > 1 {
                    self.state = GpuState::ReceivingCommand { cmd, words_remaining: words - 1 };
                } else {
                    self.execute_command(vram, cmd);
                }
            }

            // Copy rectangle (CPU -> VRAM)
            0xA0 => {
                self.command_buffer.clear();
                self.command_buffer.push(data);
                self.state = GpuState::ReceivingCommand { cmd, words_remaining: 2 };
            }

            // Copy rectangle (VRAM -> CPU)
            0xC0 => {
                self.command_buffer.clear();
                self.command_buffer.push(data);
                self.state = GpuState::ReceivingCommand { cmd, words_remaining: 2 };
            }

            // Copy rectangle (VRAM -> VRAM)
            0x80 => {
                self.command_buffer.clear();
                self.command_buffer.push(data);
                self.state = GpuState::ReceivingCommand { cmd, words_remaining: 3 };
            }

            // Environment commands
            0xE1 => {
                // Draw mode (texpage)
                self.texpage = data;
                // Mirror bits into GPUSTAT
                status.raw = (status.raw & !0x7FF) | (data & 0x7FF);
            }
            0xE2 => {
                // Texture window
                self.texture_window = data;
            }
            0xE3 => {
                // Draw area top-left
                self.draw_area_left = (data & 0x3FF) as i16;
                self.draw_area_top = ((data >> 10) & 0x1FF) as i16;
            }
            0xE4 => {
                // Draw area bottom-right
                self.draw_area_right = (data & 0x3FF) as i16;
                self.draw_area_bottom = ((data >> 10) & 0x1FF) as i16;
            }
            0xE5 => {
                // Draw offset
                self.draw_offset_x = ((data & 0x7FF) as i16) << 5 >> 5; // sign-extend 11-bit
                self.draw_offset_y = (((data >> 11) & 0x7FF) as i16) << 5 >> 5;
            }
            0xE6 => {
                // Mask bit setting
                self.mask_set = data & 1 != 0;
                self.mask_check = data & 2 != 0;
                status.set_bit(11, self.mask_set);
                status.set_bit(12, self.mask_check);
            }

            _ => {
                tracing::trace!("GP0 unhandled command: {:02X} (data={:08X})", cmd, data);
            }
        }
    }

    fn execute_command(&mut self, vram: &mut Vram, cmd: u8) {
        let buf = &self.command_buffer;
        match cmd {
            // Fill rectangle
            0x02 => {
                let color = buf[0] & 0xFF_FFFF;
                let xy = buf[1];
                let wh = buf[2];
                let x = (xy & 0x3F0) as u16; // aligned to 16 pixels
                let y = ((xy >> 16) & 0x1FF) as u16;
                let w = (((wh & 0x3FF) + 0xF) & !0xF) as u16; // rounded to 16
                let h = ((wh >> 16) & 0x1FF) as u16;

                let r = (color & 0xFF) as u8;
                let g = ((color >> 8) & 0xFF) as u8;
                let b = ((color >> 16) & 0xFF) as u8;
                let pixel = ((r as u16 >> 3) & 0x1F)
                    | (((g as u16 >> 3) & 0x1F) << 5)
                    | (((b as u16 >> 3) & 0x1F) << 10);

                for py in y..y + h {
                    for px in x..x + w {
                        vram.set_pixel(px, py, pixel);
                    }
                }
                tracing::trace!("GP0 fill rect: ({},{}) {}x{} color={:06X}", x, y, w, h, color);
            }

            // CPU -> VRAM
            0xA0 => {
                let xy = buf[1];
                let wh = buf[2];
                let x = (xy & 0x3FF) as u16;
                let y = ((xy >> 16) & 0x1FF) as u16;
                let w = (wh & 0xFFFF) as u16;
                let h = ((wh >> 16) & 0xFFFF) as u16;
                let w = if w == 0 { 1024 } else { w };
                let h = if h == 0 { 512 } else { h };

                tracing::trace!("GP0 CPU->VRAM: ({},{}) {}x{}", x, y, w, h);
                self.state = GpuState::CpuToVram { x, y, w, h, current_x: 0, current_y: 0 };
            }

            // VRAM -> CPU
            0xC0 => {
                let xy = buf[1];
                let wh = buf[2];
                let x = (xy & 0x3FF) as u16;
                let y = ((xy >> 16) & 0x1FF) as u16;
                let w = (wh & 0xFFFF) as u16;
                let h = ((wh >> 16) & 0xFFFF) as u16;
                let w = if w == 0 { 1024 } else { w };
                let h = if h == 0 { 512 } else { h };

                tracing::trace!("GP0 VRAM->CPU: ({},{}) {}x{}", x, y, w, h);
                self.state = GpuState::VramToCpu { x, y, w, h, current_x: 0, current_y: 0 };
            }

            // VRAM -> VRAM
            0x80 => {
                let src_xy = buf[1];
                let dst_xy = buf[2];
                let wh = buf[3];
                let sx = (src_xy & 0x3FF) as u16;
                let sy = ((src_xy >> 16) & 0x1FF) as u16;
                let dx = (dst_xy & 0x3FF) as u16;
                let dy = ((dst_xy >> 16) & 0x1FF) as u16;
                let w = (wh & 0xFFFF) as u16;
                let h = ((wh >> 16) & 0xFFFF) as u16;

                for py in 0..h {
                    for px in 0..w {
                        let pixel = vram.get_pixel(sx + px, sy + py);
                        vram.set_pixel(dx + px, dy + py, pixel);
                    }
                }
                tracing::trace!("GP0 VRAM->VRAM: ({},{})->({},{}) {}x{}", sx, sy, dx, dy, w, h);
            }

            // Polygons — stub: just log for now (rasterizer comes in Phase 2)
            0x20..=0x3F => {
                tracing::trace!("GP0 polygon cmd {:02X} ({} words)", cmd, buf.len());
            }

            // Lines — stub
            0x40..=0x5F => {
                tracing::trace!("GP0 line cmd {:02X}", cmd);
            }

            // Rectangles — stub
            0x60..=0x7F => {
                tracing::trace!("GP0 rect cmd {:02X}", cmd);
            }

            _ => {
                tracing::trace!("GP0 execute unhandled cmd {:02X}", cmd);
            }
        }
    }

    pub fn gp1_write(&mut self, vram: &mut Vram, status: &mut GpuStatus, display: &mut DisplayConfig, data: u32) {
        let cmd = (data >> 24) as u8;
        match cmd {
            0x00 => {
                // Reset GPU
                *status = GpuStatus::new();
                *display = DisplayConfig::new();
                self.state = GpuState::Idle;
                self.command_buffer.clear();
                self.texpage = 0;
                self.draw_area_left = 0;
                self.draw_area_top = 0;
                self.draw_area_right = 0;
                self.draw_area_bottom = 0;
                self.draw_offset_x = 0;
                self.draw_offset_y = 0;
                tracing::debug!("GP1 reset");
            }
            0x01 => {
                // Reset command buffer
                self.state = GpuState::Idle;
                self.command_buffer.clear();
            }
            0x02 => {
                // Acknowledge IRQ1
            }
            0x03 => {
                // Display enable
                let disabled = data & 1 != 0;
                status.set_display_disabled(disabled);
            }
            0x04 => {
                // DMA direction
                let dir = data & 3;
                status.set_dma_direction(dir);
            }
            0x05 => {
                // Start of display area in VRAM
                display.display_area_x = (data & 0x3FF) as u16;
                display.display_area_y = ((data >> 10) & 0x1FF) as u16;
            }
            0x06 => {
                // Horizontal display range
                display.x_start = (data & 0xFFF) as u16;
                display.x_end = ((data >> 12) & 0xFFF) as u16;
            }
            0x07 => {
                // Vertical display range
                display.y_start = (data & 0x3FF) as u16;
                display.y_end = ((data >> 10) & 0x3FF) as u16;
            }
            0x08 => {
                // Display mode
                let hr = match data & 3 {
                    0 => 256,
                    1 => 320,
                    2 => 512,
                    3 => 640,
                    _ => 320,
                };
                display.horizontal_resolution = if data & 0x40 != 0 { 368 } else { hr };
                display.vertical_resolution = if data & 4 != 0 { 480 } else { 240 };
                display.is_pal = data & 8 != 0;
                display.is_24bit = data & 0x10 != 0;
                display.interlaced = data & 0x20 != 0;

                // Update status bits
                status.raw = (status.raw & !0x7F4000) | ((data & 0x3F) << 17) | ((data & 0x40) << 10);
            }
            0x10 => {
                // Get GPU info
                tracing::trace!("GP1 get info: {:02X}", data & 0xF);
            }
            _ => {
                tracing::trace!("GP1 unhandled command: {:02X} data={:08X}", cmd, data);
            }
        }
    }

    pub fn read_data(&mut self, vram: &mut Vram) -> u32 {
        match &mut self.state {
            GpuState::VramToCpu { x, y, w, h, current_x, current_y } => {
                let mut result = 0u32;
                for i in 0..2u32 {
                    let px = *x + *current_x;
                    let py = *y + *current_y;
                    let pixel = vram.get_pixel(px, py) as u32;
                    result |= pixel << (i * 16);

                    *current_x += 1;
                    if *current_x >= *w {
                        *current_x = 0;
                        *current_y += 1;
                        if *current_y >= *h {
                            self.state = GpuState::Idle;
                            return result;
                        }
                    }
                }
                result
            }
            _ => 0,
        }
    }
}

/// Calculate number of words for a polygon command
fn polygon_word_count(cmd: u8) -> u32 {
    let is_quad = cmd & 0x08 != 0;
    let is_textured = cmd & 0x04 != 0;
    let is_gouraud = cmd & 0x10 != 0;

    let vertices = if is_quad { 4u32 } else { 3 };
    let mut words = 1u32; // command + color

    if is_gouraud {
        // color per vertex (first included in command word)
        words += vertices - 1; // extra color words
    }

    words += vertices; // vertex positions

    if is_textured {
        words += vertices; // UV + CLUT/TPage per vertex
    }

    words
}

/// Calculate number of words for a line command
fn line_word_count(cmd: u8) -> u32 {
    let is_gouraud = cmd & 0x10 != 0;
    let is_poly = cmd & 0x08 != 0;

    if is_poly {
        // Polyline — terminated by 0x5555_5555 or 0x5000_5000
        // Can't know exact count; use a reasonable max
        // We'll handle termination in the state machine
        8 // reasonable initial estimate
    } else if is_gouraud {
        4 // color1 + vertex1 + color2 + vertex2
    } else {
        3 // color + vertex1 + vertex2
    }
}

/// Calculate number of words for a rectangle command
fn rect_word_count(cmd: u8) -> u32 {
    let is_textured = cmd & 0x04 != 0;
    let size = (cmd >> 3) & 3;

    let mut words = 1u32; // command + color
    words += 1; // vertex

    if is_textured {
        words += 1; // UV + CLUT
    }

    if size == 0 {
        words += 1; // variable size
    }
    // size 1=1x1, 2=8x8, 3=16x16 — no extra word needed

    words
}
