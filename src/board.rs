//! The shared drawing surface: a fixed grid of color indices

// Board size in cells
pub const WIDTH: u16 = 80;
pub const HEIGHT: u16 = 24;

// Colour 0 is empty; 1..=PALETTE_LEN are the chalk colours
pub const PALETTE_LEN: u8 = 8;
pub const EMPTY: u8 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    cells: Vec<u8>,
}

impl Default for Board {
    fn default() -> Self {
        Self::new()
    }
}

impl Board {
    pub fn new() -> Self {
        Self {
            cells: vec![EMPTY; WIDTH as usize * HEIGHT as usize],
        }
    }

    pub fn in_bounds(x: u16, y: u16) -> bool {
        x < WIDTH && y < HEIGHT
    }

    pub fn get(&self, x: u16, y: u16) -> u8 {
        if Self::in_bounds(x, y) {
            self.cells[Self::index(x, y)]
        } else {
            EMPTY
        }
    }

    /// Paints the given points. Out-of-range points and colors are ignored, because they may come straight from the network
    pub fn paint(&mut self, color: u8, points: &[[u16; 2]]) {
        if color > PALETTE_LEN {
            return;
        }

        for &[x, y] in points {
            if Self::in_bounds(x, y) {
                self.cells[Self::index(x, y)] = color;
            }
        }
    }

    pub fn clear(&mut self) {
        self.cells.fill(EMPTY);
    }

    pub fn is_empty(&self) -> bool {
        self.cells.iter().all(|&c| c == EMPTY)
    }

    /// Run length encodes the board as `[color, run]` pairs, row-major
    pub fn to_runs(&self) -> Vec<[u16; 2]> {
        let mut runs: Vec<[u16; 2]> = Vec::new();
        for &c in &self.cells {
            match runs.last_mut() {
                Some([color, run]) if *color == c as u16 && *run < u16::MAX => *run += 1,
                _ => runs.push([c as u16, 1]),
            }
        }
        runs
    }

    /// Rebuilds a board from [`Board::to_runs`] output. Returns `None` unless the size matches ours and the runs cover the grid exactly
    pub fn from_runs(w: u16, h: u16, runs: &[[u16; 2]]) -> Option<Self> {
        if w != WIDTH || h != HEIGHT {
            return None;
        }

        let total = WIDTH as usize * HEIGHT as usize;
        let mut cells = Vec::with_capacity(total);
        for &[color, run] in runs {
            if color > PALETTE_LEN as u16 || run == 0 || cells.len() + run as usize > total {
                return None;
            }
            cells.extend(std::iter::repeat_n(color as u8, run as usize));
        }
        (cells.len() == total).then_some(Self { cells })
    }

    fn index(x: u16, y: u16) -> usize {
        y as usize * WIDTH as usize + x as usize
    }
}

/// Cells on the straight segment from `a` to `b`, both ends included, so a fast mouse drag still leaves a coninuous strike
pub fn line(a: (u16, u16), b: (u16, u16)) -> Vec<(u16, u16)> {
    let (mut x0, mut y0) = (a.0 as i32, a.1 as i32);
    let (x1, y1) = (b.0 as i32, b.1 as i32);
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut out = Vec::new();
    loop {
        out.push((x0 as u16, y0 as u16));
        if x0 == x1 && y0 == y1 {
            return out;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

/// Expands each center into a `size`x`size` square brush, clipped to the board and without duplicates
pub fn brush(centres: &[(u16, u16)], size: u16) -> Vec<[u16; 2]> {
    let size = size.max(1);
    let back = (size - 1) / 2;
    let mut out: Vec<[u16; 2]> = Vec::new();
    let mut seen = vec![false; WIDTH as usize * HEIGHT as usize];
    for &(cx, cy) in centres {
        for dy in 0..size {
            for dx in 0..size {
                let (x, y) = ((cx + dx).checked_sub(back), (cy + dy).checked_sub(back));
                let (Some(x), Some(y)) = (x, y) else { continue };
                if Board::in_bounds(x, y) && !std::mem::replace(&mut seen[Board::index(x, y)], true) {
                    out.push([x, y]);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_ignores_bad_input() {
        let mut b = Board::new();
        b.paint(2, &[[0, 0], [WIDTH, 0], [0, HEIGHT], [u16::MAX, u16::MAX]]);
        b.paint(PALETTE_LEN + 1, &[[1, 1]]);
        assert_eq!(b.get(0, 0), 2);
        assert_eq!(b.get(1, 1), EMPTY);
        assert_eq!(b.cells.iter().filter(|&&c| c != EMPTY).count(), 1);
    }

    #[test]
    fn erase_and_clear() {
        let mut b = Board::new();
        b.paint(1, &[[3, 3], [4, 4]]);
        b.paint(EMPTY, &[[3, 3]]);
        assert_eq!(b.get(3, 3), EMPTY);
        assert!(!b.is_empty());
        b.clear();
        assert!(b.is_empty());
    }

    #[test]
    fn runs_roundtrip() {
        let mut b = Board::new();
        b.paint(5, &[[0, 0], [1, 0], [79, 23], [40, 10]]);
        let runs = b.to_runs();
        assert_eq!(Board::from_runs(WIDTH, HEIGHT, &runs), Some(b));
        assert_eq!(Board::new().to_runs(), vec![[0, WIDTH * HEIGHT]]);
    }

    #[test]
    fn from_runs_rejects_bad_snapshot() {
        let full = [[0, WIDTH * HEIGHT]];
        assert!(Board::from_runs(WIDTH + 1, HEIGHT, &full).is_none());
        assert!(Board::from_runs(WIDTH, HEIGHT, &[[0, 10]]).is_none());
        assert!(Board::from_runs(WIDTH, HEIGHT, &[[0, WIDTH * HEIGHT], [1, 1]]).is_none());
        assert!(Board::from_runs(WIDTH, HEIGHT, &[[9, WIDTH * HEIGHT]]).is_none());
        assert!(Board::from_runs(WIDTH, HEIGHT, &[[0, 0], [0, WIDTH * HEIGHT]]).is_none());
    }

    #[test]
    fn line_is_continuous_both_ways() {
        let l = line((0, 0), (5, 2));
        assert_eq!(l.first(), Some(&(0, 0)));
        assert_eq!(l.last(), Some(&(5, 2)));
        for w in l.windows(2) {
            assert!(w[0].0.abs_diff(w[1].0) <= 1 && w[0].1.abs_diff(w[1].1) <= 1);
        }
        assert_eq!(line((5, 2), (0, 0)).len(), l.len());
        assert_eq!(line((3, 3), (3, 3)), vec![(3, 3)]);
    }

    #[test]
    fn brush_sizes_and_clipping() {
        assert_eq!(brush(&[(5, 5)], 1), vec![[5, 5]]);
        assert_eq!(brush(&[(5, 5)], 3).len(), 9);
        assert_eq!(brush(&[(0, 0)], 3).len(), 4);
        assert_eq!(brush(&[(5, 5), (5, 5)], 2).len(), 4);
    }
}
