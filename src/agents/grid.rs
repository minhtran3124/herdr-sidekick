//! Grid for the agent transcript panes: how many columns and rows fit the agents' area, and
//! where each pane goes. Pure, so the layout rules are tested without herdr.

/// Smallest pane worth opening: narrower or shorter and a transcript is unreadable.
pub const MIN_W: u16 = 30;
pub const MIN_H: u16 = 8;
/// Terminal cells are about twice as tall as wide.
const CELL_ASPECT: f64 = 2.2;

/// Columns and rows for `n` panes in a `w`×`h` area: the grid whose smallest pane is largest,
/// width and height weighed alike after the cell aspect. None when even the best grid has a
/// pane below MIN_W×MIN_H.
pub fn choose(n: usize, w: u16, h: u16) -> Option<(usize, usize)> {
    if n == 0 {
        return Some((0, 0));
    }
    (1..=n)
        .map(|cols| (cols, n.div_ceil(cols)))
        .filter(|&(c, r)| w as usize / c >= MIN_W as usize && h as usize / r >= MIN_H as usize)
        .max_by(|&(c1, r1), &(c2, r2)| score(w, h, c1, r1).total_cmp(&score(w, h, c2, r2)).then(c2.cmp(&c1)))
}

fn score(w: u16, h: u16, c: usize, r: usize) -> f64 {
    let (cw, ch) = (w as f64 / c as f64, h as f64 / r as f64);
    (cw / CELL_ASPECT).min(ch)
}

/// Panes per column, row-major order: pane `i` sits at row `i / cols`, column `i % cols`, so the
/// first `n % cols` columns get the extra pane.
pub fn columns(n: usize, cols: usize) -> Vec<Vec<usize>> {
    (0..cols).map(|c| (c..n).step_by(cols).collect()).collect()
}

/// The share a pane keeps when the next one is split off it: splitting the `i`-th of `k` equal
/// parts off what is left (1-based), the target keeps 1/(k-i+1) of its current size.
pub fn keep_ratio(k: usize, i: usize) -> f64 {
    1.0 / (k - i + 1) as f64
}

/// Width and height of the box around `rects` ([x, y, w, h] each).
pub fn bounds(rects: &[[u16; 4]]) -> (u16, u16) {
    let x0 = rects.iter().map(|r| r[0]).min().unwrap_or(0);
    let y0 = rects.iter().map(|r| r[1]).min().unwrap_or(0);
    let x1 = rects.iter().map(|r| r[0] + r[2]).max().unwrap_or(0);
    let y1 = rects.iter().map(|r| r[1] + r[3]).max().unwrap_or(0);
    (x1 - x0, y1 - y0)
}

/// The box's size when `rects` tile it, None when something else takes part of it. Pane
/// borders and rounded splits can leave a few cells uncounted, hence the 5% slack.
pub fn tiled(rects: &[[u16; 4]]) -> Option<(u16, u16)> {
    let (w, h) = bounds(rects);
    let covered: usize = rects.iter().map(|r| r[2] as usize * r[3] as usize).sum();
    (covered * 100 >= w as usize * h as usize * 95).then_some((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_agents_in_a_wide_area_make_three_columns_of_two() {
        assert_eq!(choose(6, 213, 61), Some((3, 2)));
        assert_eq!(choose(6, 162, 61), Some((3, 2)));
    }

    #[test]
    fn a_tall_narrow_area_stacks_instead() {
        assert_eq!(choose(3, 80, 61), Some((1, 3)));
        assert_eq!(choose(1, 80, 61), Some((1, 1)));
    }

    #[test]
    fn no_grid_when_every_pane_would_be_unreadable() {
        assert_eq!(choose(40, 160, 40), None, "40 panes cannot fit 30x8 each in 160x40");
        assert!(choose(20, 160, 40).is_some());
    }

    #[test]
    fn row_major_fill_puts_the_extra_pane_in_the_first_columns() {
        assert_eq!(columns(5, 3), vec![vec![0, 3], vec![1, 4], vec![2]]);
        assert_eq!(columns(6, 3), vec![vec![0, 3], vec![1, 4], vec![2, 5]]);
    }

    #[test]
    fn keep_ratios_split_into_equal_parts() {
        // Three columns from one: keep 1/3, then the remaining 2/3 keeps 1/2.
        let w = 300.0;
        let first = w * keep_ratio(3, 1);
        let second = (w - first) * keep_ratio(3, 2);
        assert_eq!((first, second, w - first - second), (100.0, 100.0, 100.0));
    }

    #[test]
    fn main_and_agents_tile_their_box_unless_a_panel_sits_among_them() {
        let main = [0, 0, 100, 60];
        let agents = [[100, 0, 100, 30], [100, 30, 100, 30]];
        assert_eq!(tiled(&[main, agents[0], agents[1]]), Some((200, 60)));
        // A side panel below the first agent: main and the agents leave a hole.
        assert_eq!(tiled(&[main, agents[0]]), None);
        assert_eq!(bounds(&agents), (100, 60));
    }
}
