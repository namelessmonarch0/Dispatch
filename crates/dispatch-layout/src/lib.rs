//! The pane tiling algorithm.
//!
//! Pure functions over rectangles: no state, no I/O, no rendering. This is the
//! most precisely specified part of Dispatch, so it lives on its own and is
//! tested exhaustively.
//!
//! # The rule
//!
//! Panes fill a balanced grid, row-major, with the last row stretched across
//! the full width:
//!
//! ```text
//! cols = ceil(sqrt(n))
//! rows = ceil(n / cols)
//! ```
//!
//! Which produces, for a new pane at each step:
//!
//! | n | shape                                                    |
//! |---|----------------------------------------------------------|
//! | 1 | one full pane                                            |
//! | 2 | a vertical cut down the middle: two columns              |
//! | 3 | two columns on top, one full-width pane on the bottom    |
//! | 4 | the bottom pane cut in two: four equal squares           |
//! | 5 | three on top, two stretched across the bottom           |
//! | 9 | a 3x3 grid                                               |

use ratatui::layout::Rect;

/// Number of columns used for `count` panes.
///
/// `ceil(sqrt(count))`, computed in integer arithmetic so it cannot drift on
/// a value like 9 where the float square root may land just under.
fn columns_for(count: usize) -> usize {
    let mut cols = 1;
    while cols * cols < count {
        cols += 1;
    }
    cols
}

/// Splits `total` into `parts` sizes that sum to exactly `total`.
///
/// The remainder goes to the earliest parts, one cell each, so the grid always
/// fills its area with no gap column or row.
fn split(total: u16, parts: usize) -> Vec<u16> {
    if parts == 0 {
        return Vec::new();
    }

    let parts_u16 = u16::try_from(parts).unwrap_or(u16::MAX);
    let base = total / parts_u16;
    let remainder = usize::from(total % parts_u16);

    (0..parts)
        .map(|i| base + u16::from(i < remainder))
        .collect()
}

/// Lays `count` panes out across `area`.
///
/// Returns one rectangle per pane, in the same order the panes were given.
/// The rectangles never overlap and together cover `area` exactly.
#[must_use]
pub fn tile(count: usize, area: Rect) -> Vec<Rect> {
    if count == 0 || area.width == 0 || area.height == 0 {
        return Vec::new();
    }

    let cols = columns_for(count);
    let rows = count.div_ceil(cols);
    let row_heights = split(area.height, rows);

    let mut rects = Vec::with_capacity(count);
    let mut y = area.y;

    for (row, height) in row_heights.iter().enumerate() {
        // The last row holds whatever is left, which is why three panes give a
        // full-width pane along the bottom rather than a gap beside it.
        let in_this_row = (count - row * cols).min(cols);
        let mut x = area.x;

        for width in split(area.width, in_this_row) {
            rects.push(Rect {
                x,
                y,
                width,
                height: *height,
            });
            x += width;
        }

        y += height;
    }

    rects
}

/// The rectangle a zoomed pane occupies: the whole area.
///
/// Its siblings are not rendered while a pane is zoomed.
#[must_use]
pub fn tile_zoomed(area: Rect) -> Rect {
    area
}

#[cfg(test)]
mod tests;
