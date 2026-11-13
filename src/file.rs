//! Saving a board to disk and opening it again. A save writes two files side by side: `board-N.board`, which `blackbaord
//! host --open` reads back, and `board-N.txt`, the same drawing as plain text, to paste anywhere you like

use std::{fs::{self, OpenOptions}, io::{self, Write}, path::{Path, PathBuf}};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::board::{Board, EMPTY, Glyph, HEIGHT, WIDTH};

const FORMAT: &str = "blackbaord";
const FORMAT_VERSION: u32 = 1;
/// A full board with a letter in every cell is about 60 KiB
const MAX_FILE: u64 = 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct SavedBoard {
    format: String,
    version: u32,
    w: u16, 
    h: u16,
    runs: Vec<[u16; 2]>,
    #[serde(default)]
    glyphs: Vec<Glyph>,
}

/// Saved `board` in `dir` under the first free `board-N` name. Returns the two paths written
pub fn save(board: &Board, dir: &Path) -> Result<(PathBuf, PathBuf)> {
    let saved = SavedBoard {
        format: FORMAT.into(),
        version: FORMAT_VERSION,
        w: WIDTH,
        h: HEIGHT,
        runs: board.to_runs(),
        glyphs: board.glyph_list(),
    };
    let json = serde_json::to_vec(&saved)?;
    for n in 1..10_000 {
        let path = dir.join(format!("board-{n}.board"));
        // `create_new` fails when the name is already taken, so two saves at once never overwrite each other
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("cannot write {}", path.display())),
        };
        file.write_all(&json)?;
        let text = path.with_extension("txt");
        fs::write(&text, to_text(board)).with_context(|| format!("cannot write {}", text.display()))?;
        return Ok((path, text));
    }
    bail!("too many saved board in {}", dir.display())
}

/// Opens a file writte by [`save`]
pub fn load(path: &Path) -> Result<Board> {
    let size = fs::metadata(path).with_context(|| format!("cannot open {}", path.display()))?.len();
    if size > MAX_FILE {
        bail!("{} is too big to be a board", path.display());
    }
    let data = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let saved: SavedBoard = serde_json::from_slice(&data).with_context(|| format!("{} is not a board file", path.display()))?;
    if saved.format != FORMAT || saved.version != FORMAT_VERSION {
        bail!("{} is not a board file this version can open", path.display());
    }
    let mut board = Board::from_runs(saved.w, saved.h, &saved.runs).with_context(|| format!("{} holds a board 
        of the wrong size or a broken one", path.display()))?;
    board.add_glyphs(&saved.glyphs);
    Ok(board)
}


/// The board as text: letters as they are, chalk as `█`, nothing as a space. Trailing blanks are cut
pub fn to_text(board: &Board) -> String {
    let mut rows: Vec<String> = (0..HEIGHT) 
        .map(|y| {
            let row: String = (0..WIDTH)
                .map(|x| {
                    let c = board.cell(x, y);
                    match c.glyph {
                        Some(g) => g,
                        None if c.color == EMPTY => ' ',
                        None => '█',
                    }
                })
                .collect();
            row.trim_end().to_string()
        })
        .collect();
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    let mut text = rows.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {

}