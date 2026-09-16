//! Tests for the tiling algorithm.
//!
//! The cases named after the specification come first: they encode the shape
//! the grid must take at each pane count. The property tests that follow hold
//! for every count and every area.

use super::*;

/// A 100x40 area, chosen so every split in the specified cases divides evenly
/// and the expected rectangles can be written out exactly.
const AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 100,
    height: 40,
};

fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[test]
fn one_pane_fills_the_area() {
    assert_eq!(tile(1, AREA), vec![AREA]);
}

#[test]
fn two_panes_cut_down_the_middle() {
    assert_eq!(
        tile(2, AREA),
        vec![rect(0, 0, 50, 40), rect(50, 0, 50, 40)],
        "two panes should be side-by-side columns"
    );
}

#[test]
fn three_panes_put_a_full_width_pane_along_the_bottom() {
    assert_eq!(
        tile(3, AREA),
        vec![
            rect(0, 0, 50, 20),
            rect(50, 0, 50, 20),
            rect(0, 20, 100, 20),
        ],
        "the third pane should span the whole bottom half"
    );
}

#[test]
fn four_panes_are_equal_squares() {
    assert_eq!(
        tile(4, AREA),
        vec![
            rect(0, 0, 50, 20),
            rect(50, 0, 50, 20),
            rect(0, 20, 50, 20),
            rect(50, 20, 50, 20),
        ],
        "the bottom pane should be cut in two, giving four equal cells"
    );
}

#[test]
fn five_panes_put_three_on_top_and_two_below() {
    let rects = tile(
        5,
        Rect {
            x: 0,
            y: 0,
            width: 90,
            height: 40,
        },
    );
    assert_eq!(
        rects,
        vec![
            rect(0, 0, 30, 20),
            rect(30, 0, 30, 20),
            rect(60, 0, 30, 20),
            rect(0, 20, 45, 20),
            rect(45, 20, 45, 20),
        ],
        "the bottom row should stretch to fill the width"
    );
}

#[test]
fn nine_panes_form_a_three_by_three_grid() {
    let rects = tile(
        9,
        Rect {
            x: 0,
            y: 0,
            width: 90,
            height: 30,
        },
    );
    assert_eq!(rects.len(), 9);
    for (i, r) in rects.iter().enumerate() {
        assert_eq!(r.width, 30, "pane {i} should be a third of the width");
        assert_eq!(r.height, 10, "pane {i} should be a third of the height");
    }
}

#[test]
fn growing_from_one_to_four_matches_the_specified_sequence() {
    // Column counts along the way: 1, 2, 2, 2.
    assert_eq!(tile(1, AREA).len(), 1);
    assert_eq!(
        tile(2, AREA)[0].height,
        AREA.height,
        "two panes are columns"
    );
    assert_eq!(
        tile(3, AREA)[2].width,
        AREA.width,
        "three panes: bottom spans"
    );
    assert_eq!(
        tile(4, AREA)[2].width,
        AREA.width / 2,
        "four panes: bottom splits"
    );
}

#[test]
fn no_panes_produce_no_rectangles() {
    assert!(tile(0, AREA).is_empty());
}

#[test]
fn a_degenerate_area_produces_no_rectangles() {
    assert!(tile(4, rect(0, 0, 0, 40)).is_empty());
    assert!(tile(4, rect(0, 0, 100, 0)).is_empty());
}

#[test]
fn the_area_offset_is_respected() {
    let offset = rect(7, 3, 100, 40);
    let rects = tile(4, offset);
    assert_eq!(rects[0], rect(7, 3, 50, 20));
    assert_eq!(rects[3], rect(57, 23, 50, 20));
}

#[test]
fn a_zoomed_pane_takes_the_whole_area() {
    assert_eq!(tile_zoomed(AREA), AREA);
}

/// Every pane count from 1 to 16, at several awkward sizes, must produce
/// rectangles that tile the area exactly: no gaps, no overlaps, nothing
/// outside. The odd dimensions are the point -- they force the remainder
/// distribution to be exercised.
#[test]
fn the_grid_always_covers_the_area_exactly() {
    let areas = [
        rect(0, 0, 100, 40),
        rect(0, 0, 80, 24),
        rect(3, 5, 97, 41),
        rect(0, 0, 7, 3),
        rect(0, 0, 1, 1),
        rect(0, 0, 173, 61),
    ];

    for area in areas {
        for count in 1..=16usize {
            let rects = tile(count, area);
            assert_eq!(rects.len(), count, "{count} panes in {area:?}");

            // Coverage: every cell of the area belongs to exactly one pane.
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    let owners = rects
                        .iter()
                        .filter(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
                        .count();
                    assert_eq!(
                        owners, 1,
                        "cell ({x},{y}) has {owners} owners with {count} panes in {area:?}"
                    );
                }
            }

            // Containment: nothing spills outside the area.
            for r in &rects {
                assert!(
                    r.x >= area.x
                        && r.y >= area.y
                        && r.x + r.width <= area.x + area.width
                        && r.y + r.height <= area.y + area.height,
                    "{r:?} escapes {area:?} with {count} panes"
                );
            }
        }
    }
}

/// Panes should be as close to equal as integer arithmetic allows.
///
/// Stated per row, because the last row is deliberately stretched: with five
/// panes the bottom two are wider than the top three, and that is the rule
/// working, not a bug. Within a row, and across row heights, the spread must
/// never exceed the one cell that integer division can leave over.
#[test]
fn panes_within_a_row_differ_by_at_most_one_cell() {
    for count in 1..=16usize {
        let rects = tile(count, rect(0, 0, 173, 61));

        // Group by y: each group is one row of the grid.
        let mut rows: std::collections::BTreeMap<u16, Vec<u16>> = std::collections::BTreeMap::new();
        for r in &rects {
            rows.entry(r.y).or_default().push(r.width);
        }

        for (y, widths) in &rows {
            let min = widths.iter().copied().min().expect("a row has panes");
            let max = widths.iter().copied().max().expect("a row has panes");
            assert!(
                max - min <= 1,
                "row at y={y} has widths from {min} to {max} with {count} panes"
            );
        }

        let heights: Vec<_> = rows
            .keys()
            .map(|y| {
                rects
                    .iter()
                    .find(|r| r.y == *y)
                    .expect("the row exists")
                    .height
            })
            .collect();
        let min = heights.iter().copied().min().expect("at least one row");
        let max = heights.iter().copied().max().expect("at least one row");
        assert!(
            max - min <= 1,
            "row heights vary from {min} to {max} with {count} panes"
        );
    }
}

/// An ASCII picture of each grid, so a reviewer can see the shapes rather
/// than read coordinates.
#[test]
fn grid_shapes() {
    let mut rendered = String::new();

    for count in 1..=9usize {
        let area = rect(0, 0, 24, 9);
        let rects = tile(count, area);

        rendered.push_str(&format!("n = {count}\n"));
        for y in 0..area.height {
            for x in 0..area.width {
                let owner = rects
                    .iter()
                    .position(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height);
                // Panes are labelled 1..=9 so the picture reads in spawn order.
                rendered.push(match owner {
                    Some(i) => char::from_digit(i as u32 + 1, 10).unwrap_or('?'),
                    None => '.',
                });
            }
            rendered.push('\n');
        }
        rendered.push('\n');
    }

    insta::assert_snapshot!(rendered);
}
