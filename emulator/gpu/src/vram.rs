pub const VRAM_WIDTH: usize = 1024;
pub const VRAM_HEIGHT: usize = 512;

pub struct Vram {
    pub data: Box<[u16; VRAM_WIDTH * VRAM_HEIGHT]>,
}

impl Vram {
    pub fn new() -> Self {
        Self {
            data: vec![0u16; VRAM_WIDTH * VRAM_HEIGHT]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
        }
    }

    pub fn clear(&mut self) {
        self.data.fill(0);
    }

    #[inline]
    pub fn get_pixel(&self, x: u16, y: u16) -> u16 {
        let x = (x as usize) & (VRAM_WIDTH - 1);
        let y = (y as usize) & (VRAM_HEIGHT - 1);
        self.data[y * VRAM_WIDTH + x]
    }

    #[inline]
    pub fn set_pixel(&mut self, x: u16, y: u16, color: u16) {
        let x = (x as usize) & (VRAM_WIDTH - 1);
        let y = (y as usize) & (VRAM_HEIGHT - 1);
        self.data[y * VRAM_WIDTH + x] = color;
    }

    /// Write a pixel at scaled coordinates. scale=1 writes to native VRAM.
    /// scale>1 writes to native VRAM at (x/scale, y/scale) — the upscaled
    /// render target is handled externally; this always writes native VRAM.
    #[inline]
    pub fn set_pixel_scaled(&mut self, x: i32, y: i32, color: u16, scale: u32) {
        let nx = (x / scale as i32) as usize & (VRAM_WIDTH - 1);
        let ny = (y / scale as i32) as usize & (VRAM_HEIGHT - 1);
        self.data[ny * VRAM_WIDTH + nx] = color;
    }

    /// Convert VRAM to RGBA8 for display
    pub fn to_rgba8(&self, x_start: u16, y_start: u16, width: u16, height: u16) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for y in y_start..y_start + height {
            for x in x_start..x_start + width {
                let pixel = self.get_pixel(x, y);
                let r = ((pixel & 0x1F) << 3) as u8;
                let g = (((pixel >> 5) & 0x1F) << 3) as u8;
                let b = (((pixel >> 10) & 0x1F) << 3) as u8;
                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                rgba.push(0xFF);
            }
        }
        rgba
    }
}
