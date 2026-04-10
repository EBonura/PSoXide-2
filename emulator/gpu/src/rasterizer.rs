/// Software rasterizer for PS1 GPU primitives.
///
/// Supports two modes:
///   - `scale = 1`: PSX-native (draws into 1024x512 VRAM directly)
///   - `scale = N`: screen-native (draws into Nx upscaled render target;
///     texture lookups still read from native VRAM)

use crate::vram::Vram;

/// Convert 24-bit RGB to 15-bit PS1 pixel
#[inline]
fn rgb24_to_15(r: u8, g: u8, b: u8) -> u16 {
    ((r as u16 >> 3) & 0x1F)
        | (((g as u16 >> 3) & 0x1F) << 5)
        | (((b as u16 >> 3) & 0x1F) << 10)
}

/// Extract RGB components from a GP0 color word
#[inline]
fn unpack_color(word: u32) -> (u8, u8, u8) {
    ((word & 0xFF) as u8, ((word >> 8) & 0xFF) as u8, ((word >> 16) & 0xFF) as u8)
}

/// Extract vertex position from a GP0 vertex word, applying draw offset
#[inline]
fn unpack_vertex(word: u32, ox: i16, oy: i16) -> (i32, i32) {
    let x = ((word & 0x7FF) as i16) << 5 >> 5; // sign-extend 11-bit
    let y = (((word >> 16) & 0x7FF) as i16) << 5 >> 5;
    (x as i32 + ox as i32, y as i32 + oy as i32)
}

pub struct Rasterizer {
    pub scale: u32, // 1 = native, N = upscaled
}

impl Rasterizer {
    pub fn new(scale: u32) -> Self {
        Self { scale: scale.max(1) }
    }

    /// Fill rectangle (GP0 0x02)
    pub fn fill_rect(&self, vram: &mut Vram, color: u32, xy: u32, wh: u32) {
        let (r, g, b) = unpack_color(color);
        let pixel = rgb24_to_15(r, g, b);
        let x = (xy & 0x3F0) as i32;
        let y = ((xy >> 16) & 0x1FF) as i32;
        let w = (((wh & 0x3FF) as i32 + 0xF) & !0xF).max(0);
        let h = ((wh >> 16) & 0x1FF) as i32;

        let s = self.scale as i32;
        for py in (y * s)..((y + h) * s) {
            for px in (x * s)..((x + w) * s) {
                vram.set_pixel_scaled(px, py, pixel, self.scale);
            }
        }
    }

    /// Flat-shaded opaque triangle
    pub fn flat_triangle(
        &self, vram: &mut Vram,
        color: u32, v0: u32, v1: u32, v2: u32,
        draw_area: &DrawArea, offset: (i16, i16),
    ) {
        let (r, g, b) = unpack_color(color);
        let pixel = rgb24_to_15(r, g, b);
        let p0 = unpack_vertex(v0, offset.0, offset.1);
        let p1 = unpack_vertex(v1, offset.0, offset.1);
        let p2 = unpack_vertex(v2, offset.0, offset.1);

        self.rasterize_triangle(vram, draw_area, p0, p1, p2, |_, _, _| pixel);
    }

    /// Flat-shaded opaque quad (two triangles)
    pub fn flat_quad(
        &self, vram: &mut Vram,
        color: u32, v0: u32, v1: u32, v2: u32, v3: u32,
        draw_area: &DrawArea, offset: (i16, i16),
    ) {
        self.flat_triangle(vram, color, v0, v1, v2, draw_area, offset);
        self.flat_triangle(vram, color, v1, v2, v3, draw_area, offset);
    }

    /// Gouraud-shaded triangle
    pub fn gouraud_triangle(
        &self, vram: &mut Vram,
        c0: u32, v0: u32, c1: u32, v1: u32, c2: u32, v2: u32,
        draw_area: &DrawArea, offset: (i16, i16),
    ) {
        let (r0, g0, b0) = unpack_color(c0);
        let (r1, g1, b1) = unpack_color(c1);
        let (r2, g2, b2) = unpack_color(c2);
        let p0 = unpack_vertex(v0, offset.0, offset.1);
        let p1 = unpack_vertex(v1, offset.0, offset.1);
        let p2 = unpack_vertex(v2, offset.0, offset.1);

        self.rasterize_triangle(vram, draw_area, p0, p1, p2, |w0, w1, w2| {
            let r = ((r0 as i32 * w0 + r1 as i32 * w1 + r2 as i32 * w2) >> 12).clamp(0, 255) as u8;
            let g = ((g0 as i32 * w0 + g1 as i32 * w1 + g2 as i32 * w2) >> 12).clamp(0, 255) as u8;
            let b = ((b0 as i32 * w0 + b1 as i32 * w1 + b2 as i32 * w2) >> 12).clamp(0, 255) as u8;
            rgb24_to_15(r, g, b)
        });
    }

    /// Gouraud-shaded quad
    pub fn gouraud_quad(
        &self, vram: &mut Vram,
        c0: u32, v0: u32, c1: u32, v1: u32, c2: u32, v2: u32, c3: u32, v3: u32,
        draw_area: &DrawArea, offset: (i16, i16),
    ) {
        self.gouraud_triangle(vram, c0, v0, c1, v1, c2, v2, draw_area, offset);
        self.gouraud_triangle(vram, c1, v1, c2, v2, c3, v3, draw_area, offset);
    }

    /// Flat-shaded rectangle / sprite (GP0 0x60-0x7F)
    pub fn flat_rect(
        &self, vram: &mut Vram,
        color: u32, vertex: u32, size_w: i32, size_h: i32,
        draw_area: &DrawArea, offset: (i16, i16),
    ) {
        let (r, g, b) = unpack_color(color);
        let pixel = rgb24_to_15(r, g, b);
        let (x0, y0) = unpack_vertex(vertex, offset.0, offset.1);
        let s = self.scale as i32;

        for dy in 0..size_h {
            for dx in 0..size_w {
                let px = x0 + dx;
                let py = y0 + dy;
                if draw_area.contains(px, py) {
                    for sy in 0..s {
                        for sx in 0..s {
                            vram.set_pixel_scaled(px * s + sx, py * s + sy, pixel, self.scale);
                        }
                    }
                }
            }
        }
    }

    /// Textured rectangle / sprite
    pub fn textured_rect(
        &self, vram: &mut Vram,
        _color: u32, vertex: u32, uv_clut: u32, size_w: i32, size_h: i32,
        texpage: u32, draw_area: &DrawArea, offset: (i16, i16),
    ) {
        let (x0, y0) = unpack_vertex(vertex, offset.0, offset.1);
        let u0 = (uv_clut & 0xFF) as i32;
        let v0 = ((uv_clut >> 8) & 0xFF) as i32;
        let clut_x = (((uv_clut >> 16) & 0x3F) * 16) as u16;
        let clut_y = ((uv_clut >> 22) & 0x1FF) as u16;
        let tp_x = ((texpage & 0xF) * 64) as u16;
        let tp_y = (((texpage >> 4) & 1) * 256) as u16;
        let depth = (texpage >> 7) & 3;
        let s = self.scale as i32;

        for dy in 0..size_h {
            for dx in 0..size_w {
                let px = x0 + dx;
                let py = y0 + dy;
                if !draw_area.contains(px, py) { continue; }

                let u = ((u0 + dx) & 0xFF) as u16;
                let v = ((v0 + dy) & 0xFF) as u16;
                let texel = sample_texture(vram, tp_x, tp_y, clut_x, clut_y, u, v, depth);
                if texel == 0 { continue; } // transparent

                for sy in 0..s {
                    for sx in 0..s {
                        vram.set_pixel_scaled(px * s + sx, py * s + sy, texel, self.scale);
                    }
                }
            }
        }
    }

    /// Barycentric triangle rasterizer
    fn rasterize_triangle(
        &self, vram: &mut Vram, draw_area: &DrawArea,
        p0: (i32, i32), p1: (i32, i32), p2: (i32, i32),
        shade: impl Fn(i32, i32, i32) -> u16,
    ) {
        // Bounding box
        let min_x = p0.0.min(p1.0).min(p2.0).max(draw_area.left);
        let max_x = p0.0.max(p1.0).max(p2.0).min(draw_area.right);
        let min_y = p0.1.min(p1.1).min(p2.1).max(draw_area.top);
        let max_y = p0.1.max(p1.1).max(p2.1).min(draw_area.bottom);

        // Reject degenerate or too-large triangles
        let area = edge(p0, p1, p2);
        if area == 0 { return; }
        if (max_x - min_x) > 1024 || (max_y - min_y) > 512 { return; }

        let s = self.scale as i32;

        // Ensure CCW winding
        let (p0, p1, p2, area) = if area < 0 {
            (p0, p2, p1, -area)
        } else {
            (p0, p1, p2, area)
        };

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let w0 = edge(p1, p2, (x, y));
                let w1 = edge(p2, p0, (x, y));
                let w2 = edge(p0, p1, (x, y));

                if w0 >= 0 && w1 >= 0 && w2 >= 0 {
                    // Normalize weights to 12-bit fixed point (4096 = 1.0)
                    let bw0 = (w0 * 4096) / area;
                    let bw1 = (w1 * 4096) / area;
                    let bw2 = 4096 - bw0 - bw1;

                    let pixel = shade(bw0, bw1, bw2);
                    for sy in 0..s {
                        for sx in 0..s {
                            vram.set_pixel_scaled(x * s + sx, y * s + sy, pixel, self.scale);
                        }
                    }
                }
            }
        }
    }
}

/// 2D cross product / edge function
#[inline]
fn edge(a: (i32, i32), b: (i32, i32), c: (i32, i32)) -> i32 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

/// Sample a texture from VRAM at native resolution
fn sample_texture(
    vram: &Vram, tp_x: u16, tp_y: u16,
    clut_x: u16, clut_y: u16,
    u: u16, v: u16, depth: u32,
) -> u16 {
    match depth {
        0 => {
            // 4-bit CLUT
            let tx = tp_x + u / 4;
            let raw = vram.get_pixel(tx, tp_y + v);
            let index = (raw >> ((u & 3) * 4)) & 0xF;
            vram.get_pixel(clut_x + index, clut_y)
        }
        1 => {
            // 8-bit CLUT
            let tx = tp_x + u / 2;
            let raw = vram.get_pixel(tx, tp_y + v);
            let index = (raw >> ((u & 1) * 8)) & 0xFF;
            vram.get_pixel(clut_x + index, clut_y)
        }
        2 | 3 => {
            // 15-bit direct
            vram.get_pixel(tp_x + u, tp_y + v)
        }
        _ => 0,
    }
}

pub struct DrawArea {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl DrawArea {
    #[inline]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x <= self.right && y >= self.top && y <= self.bottom
    }
}
