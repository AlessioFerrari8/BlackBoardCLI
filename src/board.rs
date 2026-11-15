//! The shared drawing surface: a fixed grid of color indices, where a cell can also hold a letter

use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthChar;

// Board size in cells
pub const WIDTH: u16 = 80;
pub const HEIGHT: u16 = 24;

// Colour 0 is empty; 1..=PALETTE_LEN are the chalk colours
pub const PALETTE_LEN: u8 = 8;
pub const EMPTY: u8 = 0;

/// What one cell shows: a chalk color, and a letter written in that color instead of a full block
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cell {
    pub color: u8,
    pub glyph: Option<char>,
}

/// A letter on the board, as it travels in snapshots and save files
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    pub x: u16,
    pub y: u16,
    pub c: char,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    cells: Vec<u8>,
    glyphs: Vec<Option<char>>,
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
            glyphs: vec![None; WIDTH as usize * HEIGHT as usize],
        }
    }

    pub fn in_bounds(x: u16, y: u16) -> bool {
        x < WIDTH && y < HEIGHT
    }

    /// Cells connected to (x, y) that have its same color, for the bucket tool. Letters count as walls
    pub fn fill(&self, x: u16, y: u16) -> Vec<[u16; 2]> {
        if !Self::in_bounds(x, y) {
            return Vec::new();
        }
        let target = self.cell(x, y);
        let mut seen = vec![false; WIDTH as usize * HEIGHT as usize];
        let mut stack = vec![(x, y)];
        let mut out = Vec::new();
        while let Some((cx, cy)) = stack.pop() {
            let i = Self::index(cx, cy);
            if seen[i] || self.cell(cx, cy) != target {
                continue;
            }
            seen[i] = true;
            out.push([cx, cy]);
            if cx > 0 {
                stack.push((cx - 1, cy));
            }
            if cx + 1 < WIDTH {
                stack.push((cx + 1, cy));
            }
            if cy > 0 {
                stack.push((cx, cy - 1));
            }
            if cy + 1 < HEIGHT {
                stack.push((cx, cy + 1));
            }
        }
        out
    }

    pub fn get(&self, x: u16, y: u16) -> u8 {
        if Self::in_bounds(x, y) {
            self.cells[Self::index(x, y)]
        } else {
            EMPTY
        }
    }

    pub fn cell(&self, x: u16, y: u16) -> Cell {
        if !Self::in_bounds(x, y) {
            return Cell::default();
        }
        let i = Self::index(x, y);
        Cell {
            color: self.cells[i],
            glyph: self.glyphs[i],
        }
    }

    /// Puts `cell` back as it is. Used by undo, so it takes the same checks as the network input
    pub fn set(&mut self, x: u16, y: u16, cell: Cell) {
        if !Self::in_bounds(x, y) || cell.color > PALETTE_LEN {
            return;
        }
        let i = Self::index(x, y);
        self.cells[i] = cell.color;
        self.glyphs[i] = cell.glyph.filter(|&c| cell.color != EMPTY && is_glyph(c));
    }

    /// Paints the given points. Out-of-range points and colors are ignored, because they may come straight from the network
    /// A painted cell loses its letter
    pub fn paint(&mut self, color: u8, points: &[[u16; 2]]) {
        if color > PALETTE_LEN {
            return;
        }

        for &[x, y] in points {
            if Self::in_bounds(x, y) {
                let i = Self::index(x, y);
                self.cells[i] = color;
                self.glyphs[i] = None;
            }
        }
    }

    /// Writes `text` from `(x, y)` to the right, one letter per cell, and stops at the edge. Letters that do not fit in one cell
    /// are skipped. Returns how many cells were written
    pub fn write(&mut self, color: u8, x: u16, y: u16, text: &str) -> u16 {
        if color == EMPTY || color > PALETTE_LEN || !Self::in_bounds(x, y) {
            return 0;
        }
        let mut n = 0;
        for c in text.chars().filter(|&c| is_glyph(c)) {
            if x + n >= WIDTH {
                break;
            }
            let i = Self::index(x + n, y);
            self.cells[i] = color;
            self.glyphs[i] = Some(c);
            n += 1;
        }
        n
    }

    pub fn clear(&mut self) {
        self.cells.fill(EMPTY);
        self.glyphs.fill(None);
    }

    /// Every letter on the board, row-major
    pub fn glyph_list(&self) -> Vec<Glyph> {
        self.glyphs
            .iter()
            .enumerate()
            .filter_map(|(i, g)| {
                g.map(|c| Glyph {
                    x: (i % WIDTH as usize) as u16,
                    y: (i / WIDTH as usize) as u16,
                    c,
                })
            })
            .collect()
    }

    /// Puts letters from a snapshot back. Letters on empty cells, outside the board or wider than one cell are dropped
    pub fn add_glyphs(&mut self, glyphs: &[Glyph]) {
        for g in glyphs {
            if Self::in_bounds(g.x, g.y) && is_glyph(g.c) {
                let i = Self::index(g.x, g.y);
                if self.cells[i] != EMPTY {
                    self.glyphs[i] = Some(g.c);
                }
            }
        }
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
        (cells.len() == total).then_some(Self {
            cells,
            glyphs: vec![None; total],
        })
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

/// Outline of the rectangle with corners `a` and `b`, each cell once
pub fn rect(a: (u16, u16), b: (u16, u16)) -> Vec<(u16, u16)> {
    let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
    let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
    let mut out = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            if y == y0 || y == y1 || x == x0 || x == x1 {
                out.push((x, y));
            }
        }
    }
    out
}

/// True for a printable letter one cell wide: what [`Board::write`] accepts
pub fn is_glyph(c: char) -> bool {
    !c.is_control() && c != ' ' && c.width() == Some(1)
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
    fn write_puts_letters_and_paint_removes_them() {
        let mut b = Board::new();
        assert_eq!(b.write(3, 77, 1, "ciao"), 3);
        assert_eq!(
            b.cell(77, 1),
            Cell {
                color: 3,
                glyph: Some('c')
            }
        );
        assert_eq!(b.cell(79, 1).glyph, Some('a'));
        assert_eq!(b.write(EMPTY, 0, 0, "x"), 0);
        assert_eq!(b.write(2, 0, 0, "a\x1b一b"), 2, "escape and wide letters are skipped");
        assert_eq!(b.cell(1, 0).glyph, Some('b'));
        b.paint(5, &[[77, 1]]);
        assert_eq!(b.cell(77, 1), Cell { color: 5, glyph: None });
        b.clear();
        assert!(b.glyph_list().is_empty());
    }

    #[test]
    fn glyphs_travel_with_the_runs() {
        let mut b = Board::new();
        b.write(4, 10, 5, "hi");
        let mut copy = Board::from_runs(WIDTH, HEIGHT, &b.to_runs()).unwrap();
        copy.add_glyphs(&b.glyph_list());
        assert_eq!(copy, b);
        // A letter on an empty cell or off the board is dropped
        copy.add_glyphs(&[Glyph { x: 0, y: 0, c: 'z' }, Glyph { x: WIDTH, y: 0, c: 'z' }]);
        assert_eq!(copy, b);
    }

    #[test]
    fn set_restores_a_cell() {
        let mut b = Board::new();
        b.set(
            1,
            1,
            Cell {
                color: 2,
                glyph: Some('k'),
            },
        );
        assert_eq!(b.cell(1, 1).glyph, Some('k'));
        b.set(
            1,
            1,
            Cell {
                color: EMPTY,
                glyph: Some('k'),
            },
        );
        assert_eq!(b.cell(1, 1), Cell::default(), "an empty cell holds no letter");
    }

    #[test]
    fn rect_is_an_outline() {
        assert_eq!(rect((2, 2), (2, 2)), vec![(2, 2)]);
        assert_eq!(rect((0, 0), (3, 2)).len(), 10);
        assert_eq!(rect((3, 2), (0, 0)), rect((0, 0), (3, 2)));
    }

    #[test]
    fn brush_sizes_and_clipping() {
        assert_eq!(brush(&[(5, 5)], 1), vec![[5, 5]]);
        assert_eq!(brush(&[(5, 5)], 3).len(), 9);
        assert_eq!(brush(&[(0, 0)], 3).len(), 4);
        assert_eq!(brush(&[(5, 5), (5, 5)], 2).len(), 4);
    }
}
