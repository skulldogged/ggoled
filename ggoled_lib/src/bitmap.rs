#![allow(dead_code)]

pub use bit_vec::BitVec;

#[derive(PartialEq)]
pub struct Bitmap {
    pub w: usize,
    pub h: usize,
    pub data: BitVec,
}
impl Bitmap {
    pub fn new(w: usize, h: usize, on: bool) -> Self {
        let data = BitVec::from_elem(w * h, on);
        Bitmap { w, h, data }
    }

    /// Crop Bitmap to a new size. Out of bounds positions and sizes will panic.
    pub fn crop(&self, x: usize, y: usize, w: usize, h: usize) -> Self {
        assert!(x <= self.w && y <= self.h);
        assert!(w <= self.w - x && h <= self.h - y);
        let mut data = BitVec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                data.push(self.data[x + y * self.w]);
            }
        }
        Self { w, h, data }
    }

    /// Blit another Bitmap onto this one. Bounds will *not* be expanded.
    /// `opaque=true` means all pixels will be blitted. `opaque=false` means only set pixels will be blitted (i.e. unset pixels act as if transparent).
    pub fn blit(&mut self, other: &Bitmap, x: isize, y: isize, opaque: bool) {
        let src_x_start = (-x).max(0) as usize;
        let src_y_start = (-y).max(0) as usize;
        let dst_x_start = x.max(0) as usize;
        let dst_y_start = y.max(0) as usize;
        let overlap_w = other
            .w
            .saturating_sub(src_x_start)
            .min(self.w.saturating_sub(dst_x_start));
        let overlap_h = other
            .h
            .saturating_sub(src_y_start)
            .min(self.h.saturating_sub(dst_y_start));
        if overlap_w == 0 || overlap_h == 0 {
            return;
        }

        for oy in src_y_start..(src_y_start + overlap_h) {
            let sy = dst_y_start + (oy - src_y_start);
            for ox in src_x_start..(src_x_start + overlap_w) {
                let sx = dst_x_start + (ox - src_x_start);
                let si = sx + sy * self.w;
                let oi = ox + oy * other.w;
                if opaque {
                    self.data.set(si, other.data[oi]);
                } else {
                    self.data.set(si, self.data[si] | other.data[oi]);
                }
            }
        }
    }

    /// Inverts all pixels in the bitmap.
    pub fn invert(&mut self) {
        self.data.negate();
    }
}

#[cfg(test)]
mod tests {
    use super::Bitmap;

    fn bitmap_from_rows(rows: &[&str]) -> Bitmap {
        let h = rows.len();
        let w = rows.first().map_or(0, |r| r.len());
        let mut bitmap = Bitmap::new(w, h, false);
        for (y, row) in rows.iter().enumerate() {
            assert_eq!(row.len(), w);
            for (x, ch) in row.chars().enumerate() {
                bitmap.data.set(y * w + x, ch == '1');
            }
        }
        bitmap
    }

    fn rows_from_bitmap(bitmap: &Bitmap) -> Vec<String> {
        let mut rows = Vec::with_capacity(bitmap.h);
        for y in 0..bitmap.h {
            let mut row = String::with_capacity(bitmap.w);
            for x in 0..bitmap.w {
                row.push(if bitmap.data[y * bitmap.w + x] { '1' } else { '0' });
            }
            rows.push(row);
        }
        rows
    }

    #[test]
    fn blit_opaque_overwrites_destination() {
        let mut dst = bitmap_from_rows(&["111", "111", "111"]);
        let src = bitmap_from_rows(&["00", "10"]);
        dst.blit(&src, 1, 1, true);
        assert_eq!(rows_from_bitmap(&dst), vec!["111", "100", "110"]);
    }

    #[test]
    fn blit_transparent_ors_destination() {
        let mut dst = bitmap_from_rows(&["010", "000", "000"]);
        let src = bitmap_from_rows(&["10", "01"]);
        dst.blit(&src, 1, 1, false);
        assert_eq!(rows_from_bitmap(&dst), vec!["010", "010", "001"]);
    }

    #[test]
    fn blit_handles_negative_offset() {
        let mut dst = bitmap_from_rows(&["000", "000", "000"]);
        let src = bitmap_from_rows(&["111", "111", "111"]);
        dst.blit(&src, -1, -1, true);
        assert_eq!(rows_from_bitmap(&dst), vec!["110", "110", "000"]);
    }

    #[test]
    fn blit_offscreen_noop() {
        let mut dst = bitmap_from_rows(&["101", "010", "101"]);
        let before = rows_from_bitmap(&dst);
        let src = bitmap_from_rows(&["11", "11"]);
        dst.blit(&src, 10, 10, true);
        assert_eq!(rows_from_bitmap(&dst), before);
    }
}
