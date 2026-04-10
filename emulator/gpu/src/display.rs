pub struct DisplayConfig {
    pub x_start: u16,
    pub y_start: u16,
    pub x_end: u16,
    pub y_end: u16,
    pub display_area_x: u16,
    pub display_area_y: u16,
    pub horizontal_resolution: u16,
    pub vertical_resolution: u16,
    pub is_pal: bool,
    pub is_24bit: bool,
    pub interlaced: bool,
}

impl DisplayConfig {
    pub fn new() -> Self {
        Self {
            x_start: 0x200,
            y_start: 0x010,
            x_end: 0xC00,
            y_end: 0x100,
            display_area_x: 0,
            display_area_y: 0,
            horizontal_resolution: 320,
            vertical_resolution: 240,
            is_pal: false,
            is_24bit: false,
            interlaced: false,
        }
    }

    pub fn width(&self) -> u16 {
        self.horizontal_resolution
    }

    pub fn height(&self) -> u16 {
        self.vertical_resolution
    }
}
