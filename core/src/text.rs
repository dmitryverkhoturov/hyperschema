//! Text: lazy per-label layout in em units + an R8 glyph atlas rasterised at
//! a few fixed pixel sizes. A glyph quad lives in world space and only
//! changes when the size bucket or the visible set changes, not per frame.

use crate::scene::{Align, Label};
use ab_glyph::{Font, FontArc, GlyphId, PxScale, ScaleFont};
use std::collections::HashMap;

pub const ATLAS: u32 = 2048;
pub const BUCKETS: [f32; 9] = [10.0, 14.0, 20.0, 28.0, 40.0, 56.0, 80.0, 112.0, 160.0];
pub const LINE_H: f32 = 1.25;

#[derive(Clone, Copy)]
pub struct AtlasGlyph {
    pub uv: [f32; 4],
    /// Pixel bounds relative to the pen position at bucket size.
    pub min: [f32; 2],
    pub size: [f32; 2],
}

pub struct Layout {
    /// (glyph, x, line) in em; x already includes alignment.
    pub glyphs: Vec<(GlyphId, f32, u16)>,
}

pub struct Text {
    font: FontArc,
    ascent: f32,
    descent: f32,
    pub pixels: Vec<u8>,
    /// Rows [y0, y1) changed since the last upload.
    pub dirty: Option<(u32, u32)>,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
    glyphs: HashMap<(GlyphId, u8), Option<AtlasGlyph>>,
    layouts: HashMap<u32, Layout>,
    /// UV of a solid white block, for text bars at tiny sizes.
    pub white_uv: [f32; 4],
}

impl Text {
    pub fn new(font_bytes: Vec<u8>) -> Self {
        let font = FontArc::try_from_vec(font_bytes).expect("bad font");
        let s = font.as_scaled(PxScale::from(1.0));
        let (ascent, descent) = (s.ascent(), s.descent());
        let mut t = Text {
            font,
            ascent,
            descent,
            pixels: vec![0; (ATLAS * ATLAS) as usize],
            dirty: None,
            shelf_x: 0,
            shelf_y: 0,
            shelf_h: 0,
            glyphs: HashMap::new(),
            layouts: HashMap::new(),
            white_uv: [0.0; 4],
        };
        t.reset_atlas();
        t
    }

    fn reset_atlas(&mut self) {
        self.pixels.fill(0);
        self.glyphs.clear();
        for y in 0..4 {
            for x in 0..4 {
                self.pixels[(y * ATLAS + x) as usize] = 255;
            }
        }
        let a = ATLAS as f32;
        self.white_uv = [1.0 / a, 1.0 / a, 3.0 / a, 3.0 / a];
        self.shelf_x = 6;
        self.shelf_y = 0;
        self.shelf_h = 6;
        self.dirty = Some((0, ATLAS));
    }

    /// Baseline offset from the top of a line, in em.
    pub fn baseline(&self) -> f32 {
        (LINE_H - (self.ascent - self.descent)) * 0.5 + self.ascent
    }

    pub fn layout(&mut self, idx: u32, label: &Label) -> &Layout {
        if self.layouts.len() > 50_000 {
            self.layouts.clear();
        }
        if !self.layouts.contains_key(&idx) {
            let l = self.make_layout(label);
            self.layouts.insert(idx, l);
        }
        &self.layouts[&idx]
    }

    /// Drops cached layouts (label indices change when the scene does).
    pub fn clear_layouts(&mut self) {
        self.layouts.clear();
    }

    /// Width of the widest line and the line count, in em.
    pub fn measure(&self, text: &str) -> (f32, usize) {
        let s = self.font.as_scaled(PxScale::from(1.0));
        let w = text.split('\n').map(|l| l.chars().map(|c| s.h_advance(self.font.glyph_id(c))).sum::<f32>()).fold(0.0, f32::max);
        (w, text.split('\n').count())
    }

    pub fn make_layout(&self, label: &Label) -> Layout {
        let s = self.font.as_scaled(PxScale::from(1.0));
        let wrap_em = if label.wrap && label.w > 0.0 { label.w / label.size } else { f32::MAX };
        let box_em = label.w / label.size;
        let mut glyphs = Vec::with_capacity(label.text.len());
        let mut line: u16 = 0;
        for para in label.text.split('\n') {
            // Greedy word wrap.
            let mut row: Vec<(GlyphId, f32)> = Vec::new();
            let mut x = 0f32;
            let flush = |row: &mut Vec<(GlyphId, f32)>, width: f32, line: u16, glyphs: &mut Vec<(GlyphId, f32, u16)>| {
                let off = match label.align {
                    Align::Left => 0.0,
                    Align::Center => (box_em - width) * 0.5,
                    Align::Right => box_em - width,
                };
                glyphs.extend(row.drain(..).map(|(g, gx)| (g, gx + off, line)));
            };
            for word in para.split_inclusive(' ') {
                let ids: Vec<GlyphId> = word.chars().map(|c| self.font.glyph_id(c)).collect();
                let w: f32 = ids.iter().map(|&g| s.h_advance(g)).sum();
                let trailing = if word.ends_with(' ') { s.h_advance(self.font.glyph_id(' ')) } else { 0.0 };
                if x > 0.0 && x + w - trailing > wrap_em {
                    flush(&mut row, x, line, &mut glyphs);
                    line += 1;
                    x = 0.0;
                }
                for g in ids {
                    row.push((g, x));
                    x += s.h_advance(g);
                }
            }
            let width = x - if para.ends_with(' ') { s.h_advance(self.font.glyph_id(' ')) } else { 0.0 };
            flush(&mut row, width, line, &mut glyphs);
            line += 1;
        }
        Layout { glyphs }
    }

    pub fn bucket_for(px: f32) -> u8 {
        BUCKETS.iter().position(|&b| b >= px * 0.95).unwrap_or(BUCKETS.len() - 1) as u8
    }

    /// Glyph in the atlas, rasterising it on first use. `Err` = atlas full.
    pub fn glyph(&mut self, g: GlyphId, bucket: u8) -> Result<Option<AtlasGlyph>, ()> {
        if let Some(e) = self.glyphs.get(&(g, bucket)) {
            return Ok(*e);
        }
        let px = BUCKETS[bucket as usize];
        let glyph = g.with_scale_and_position(PxScale::from(px), ab_glyph::point(0.0, 0.0));
        let Some(outline) = self.font.outline_glyph(glyph) else {
            self.glyphs.insert((g, bucket), None);
            return Ok(None);
        };
        let b = outline.px_bounds();
        let (w, h) = (b.width() as u32 + 2, b.height() as u32 + 2);
        if self.shelf_x + w > ATLAS {
            self.shelf_x = 0;
            self.shelf_y += self.shelf_h;
            self.shelf_h = 0;
        }
        if self.shelf_y + h > ATLAS {
            return Err(());
        }
        let (ox, oy) = (self.shelf_x + 1, self.shelf_y + 1);
        outline.draw(|x, y, c| {
            let i = ((oy + y) * ATLAS + ox + x) as usize;
            self.pixels[i] = (c.clamp(0.0, 1.0) * 255.0) as u8;
        });
        self.shelf_x += w;
        self.shelf_h = self.shelf_h.max(h);
        let d = self.dirty.get_or_insert((oy, oy + h));
        d.0 = d.0.min(oy - 1);
        d.1 = d.1.max(oy + h);
        let a = ATLAS as f32;
        let e = AtlasGlyph {
            uv: [ox as f32 / a, oy as f32 / a, (ox as f32 + b.width()) / a, (oy as f32 + b.height()) / a],
            min: [b.min.x, b.min.y],
            size: [b.width(), b.height()],
        };
        self.glyphs.insert((g, bucket), Some(e));
        Ok(Some(e))
    }

    pub fn clear_atlas(&mut self) {
        self.reset_atlas();
    }
}
