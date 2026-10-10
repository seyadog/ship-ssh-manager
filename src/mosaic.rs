//! The mosaic: a tree of divisions. Every division is side by side or stacked and holds two parts, each a
//! terminal or another division, so any arrangement can be built.

use ratatui::layout::Rect;

/// Where a terminal docks against another one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tree {
    /// A terminal (tab id).
    Leaf(u64),
    /// `side`: the parts are side by side (else one on top of the other). `pm`: size of the first, in thousandths.
    Split { side: bool, pm: u16, a: Box<Tree>, b: Box<Tree> },
}

/// A division as drawn: where the pointer can grab it.
#[derive(Clone, Debug)]
pub struct SplitInfo {
    /// Which way to go from the root to reach it (false: first part, true: second).
    pub path: Vec<bool>,
    /// The area both parts share.
    pub whole: Rect,
    pub side: bool,
    /// Column (side by side) or row (stacked) where the second part starts.
    pub at: u16,
}

impl Tree {
    /// A tidy arrangement of these terminals: halves, alternating side by side and stacked.
    #[cfg(test)]
    pub fn balanced(ids: &[u64], side: bool) -> Tree {
        match ids {
            [one] => Tree::Leaf(*one),
            _ => {
                let mid = ids.len() / 2;
                Tree::Split { side, pm: 500, a: Box::new(Tree::balanced(&ids[..mid], !side)), b: Box::new(Tree::balanced(&ids[mid..], !side)) }
            }
        }
    }

    pub fn leaves(&self) -> Vec<u64> {
        match self {
            Tree::Leaf(id) => vec![*id],
            Tree::Split { a, b, .. } => {
                let mut v = a.leaves();
                v.extend(b.leaves());
                v
            }
        }
    }

    pub fn contains(&self, id: u64) -> bool {
        self.leaves().contains(&id)
    }

    /// Takes a terminal out; its sibling fills the place of the division.
    pub fn remove(self, id: u64) -> Option<Tree> {
        match self {
            Tree::Leaf(x) if x == id => None,
            Tree::Leaf(x) => Some(Tree::Leaf(x)),
            Tree::Split { side, pm, a, b } => match ((*a).remove(id), (*b).remove(id)) {
                (Some(a), Some(b)) => Some(Tree::Split { side, pm, a: Box::new(a), b: Box::new(b) }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }

    /// Puts `new` where `old` is.
    pub fn replace(&mut self, old: u64, new: u64) {
        match self {
            Tree::Leaf(x) if *x == old => *x = new,
            Tree::Leaf(_) => {}
            Tree::Split { a, b, .. } => {
                a.replace(old, new);
                b.replace(old, new);
            }
        }
    }

    /// Two terminals trade places.
    pub fn swap(&mut self, p: u64, q: u64) {
        match self {
            Tree::Leaf(x) if *x == p => *x = q,
            Tree::Leaf(x) if *x == q => *x = p,
            Tree::Leaf(_) => {}
            Tree::Split { a, b, .. } => {
                a.swap(p, q);
                b.swap(p, q);
            }
        }
    }

    /// `new` (not in the tree yet) takes half of the terminal `target`, on that side of it.
    pub fn dock(&mut self, target: u64, new: u64, side: Side) -> bool {
        match self {
            Tree::Leaf(x) if *x == target => {
                let (by_side, new_first) = match side {
                    Side::Left => (true, true),
                    Side::Right => (true, false),
                    Side::Top => (false, true),
                    Side::Bottom => (false, false),
                };
                let (t, n) = (Tree::Leaf(target), Tree::Leaf(new));
                let (a, b) = if new_first { (n, t) } else { (t, n) };
                *self = Tree::Split { side: by_side, pm: 500, a: Box::new(a), b: Box::new(b) };
                true
            }
            Tree::Leaf(_) => false,
            Tree::Split { a, b, .. } => a.dock(target, new, side) || b.dock(target, new, side),
        }
    }

    /// The boxes of the terminals and the divisions inside `area`.
    pub fn layout(&self, area: Rect) -> (Vec<(u64, Rect)>, Vec<SplitInfo>) {
        let (mut boxes, mut splits) = (vec![], vec![]);
        self.layout_into(area, &mut vec![], &mut boxes, &mut splits);
        (boxes, splits)
    }

    fn layout_into(&self, area: Rect, path: &mut Vec<bool>, boxes: &mut Vec<(u64, Rect)>, splits: &mut Vec<SplitInfo>) {
        match self {
            Tree::Leaf(id) => boxes.push((*id, area)),
            Tree::Split { side, pm, a, b } => {
                let (ra, rb, at) = if *side {
                    let w = ((area.width as u32 * *pm as u32 / 1000) as u16).max(1).min(area.width.saturating_sub(1));
                    (Rect::new(area.x, area.y, w, area.height), Rect::new(area.x + w, area.y, area.width - w, area.height), area.x + w)
                } else {
                    let h = ((area.height as u32 * *pm as u32 / 1000) as u16).max(1).min(area.height.saturating_sub(1));
                    (Rect::new(area.x, area.y, area.width, h), Rect::new(area.x, area.y + h, area.width, area.height - h), area.y + h)
                };
                splits.push(SplitInfo { path: path.clone(), whole: area, side: *side, at });
                path.push(false);
                a.layout_into(ra, path, boxes, splits);
                path.pop();
                path.push(true);
                b.layout_into(rb, path, boxes, splits);
                path.pop();
            }
        }
    }

    /// Sets the size of the division at `path`.
    pub fn set_pm(&mut self, path: &[bool], value: u16) {
        if let Tree::Split { pm, a, b, .. } = self {
            match path.split_first() {
                None => *pm = value,
                Some((false, rest)) => a.set_pm(rest, value),
                Some((true, rest)) => b.set_pm(rest, value),
            }
        }
    }
}

/// Where a new terminal goes when it splits the box `r` by itself: beside it when the box is wide (a cell is about
/// twice as tall as wide), below it when it is tall, and never leaving a box too small to use if the other way fits.
pub fn auto_side(r: Rect) -> Side {
    let wide = r.width >= r.height * 2;
    let fits_beside = r.width / 2 >= 20;
    let fits_below = r.height / 2 >= 6;
    if wide && (fits_beside || !fits_below) || !wide && !fits_below && fits_beside {
        Side::Right
    } else {
        Side::Bottom
    }
}

/// Like Hyprland's default layout: each terminal splits the one before it, beside or below by the shape of its box.
pub fn dwindle(ids: &[u64], area: Rect) -> Tree {
    let mut tree = Tree::Leaf(ids[0]);
    for pair in ids.windows(2) {
        let r = tree.layout(area).0.iter().find(|(i, _)| *i == pair[0]).map(|&(_, r)| r).unwrap_or(area);
        tree.dock(pair[0], pair[1], auto_side(r));
    }
    tree
}

/// The side of `r` a pointer at (x, y) is closest to, when it is near the edge; `None` in the middle.
pub fn dock_side(r: Rect, x: u16, y: u16) -> Option<Side> {
    let fx = (x.saturating_sub(r.x)) as f32 / r.width.max(1) as f32;
    let fy = (y.saturating_sub(r.y)) as f32 / r.height.max(1) as f32;
    let edges = [(fx, Side::Left), (1.0 - fx, Side::Right), (fy, Side::Top), (1.0 - fy, Side::Bottom)];
    let (d, side) = edges.into_iter().min_by(|p, q| p.0.total_cmp(&q.0)).unwrap();
    (d < 0.3).then_some(side)
}

/// The half of `r` a terminal would take when docked on `side`; the whole box when `None`.
pub fn dock_zone(r: Rect, side: Option<Side>) -> Rect {
    let (hw, hh) = (r.width / 2, r.height / 2);
    match side {
        Some(Side::Left) => Rect::new(r.x, r.y, hw, r.height),
        Some(Side::Right) => Rect::new(r.x + hw, r.y, r.width - hw, r.height),
        Some(Side::Top) => Rect::new(r.x, r.y, r.width, hh),
        Some(Side::Bottom) => Rect::new(r.x, r.y + hh, r.width, r.height - hh),
        None => r,
    }
}

/// The box next to box `s` in direction (dx, dy), by where the boxes really are.
pub fn neighbour(rects: &[Rect], s: usize, dx: i32, dy: i32) -> Option<usize> {
    let cur = *rects.get(s)?;
    let overlap = |a0: u16, a1: u16, b0: u16, b1: u16| (a1.min(b1) as i32 - a0.max(b0) as i32).max(0);
    rects
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != s)
        .filter_map(|(i, r)| {
            let (gap, side) = match (dx, dy) {
                (-1, 0) => (cur.x as i32 - (r.x + r.width) as i32, overlap(cur.y, cur.y + cur.height, r.y, r.y + r.height)),
                (1, 0) => (r.x as i32 - (cur.x + cur.width) as i32, overlap(cur.y, cur.y + cur.height, r.y, r.y + r.height)),
                (0, -1) => (cur.y as i32 - (r.y + r.height) as i32, overlap(cur.x, cur.x + cur.width, r.x, r.x + r.width)),
                _ => (r.y as i32 - (cur.y + cur.height) as i32, overlap(cur.x, cur.x + cur.width, r.x, r.x + r.width)),
            };
            (gap >= 0 && side > 0).then_some((i, gap, side))
        })
        .min_by_key(|&(_, gap, side)| (gap, -side))
        .map(|(i, _, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { x: 0, y: 0, width: 80, height: 40 };

    #[test]
    fn four_terminals_make_a_grid_and_tile_the_area() {
        let t = Tree::balanced(&[1, 2, 3, 4], true);
        let (boxes, splits) = t.layout(AREA);
        assert_eq!(boxes.len(), 4);
        assert_eq!(splits.len(), 3);
        let cells: u32 = boxes.iter().map(|(_, r)| r.width as u32 * r.height as u32).sum();
        assert_eq!(cells, 80 * 40);
        assert_eq!(boxes[0].1, Rect::new(0, 0, 40, 20));
        assert_eq!(boxes[3].1, Rect::new(40, 20, 40, 20));
    }

    #[test]
    fn docking_splits_a_terminal_on_any_side_again_and_again() {
        let mut t = Tree::Leaf(1);
        assert!(t.dock(1, 2, Side::Right));
        assert!(t.dock(2, 3, Side::Bottom));
        assert!(t.dock(1, 4, Side::Top));
        assert!(t.dock(3, 5, Side::Left));
        assert_eq!(t.leaves().len(), 5);
        let (boxes, _) = t.layout(AREA);
        let cells: u32 = boxes.iter().map(|(_, r)| r.width as u32 * r.height as u32).sum();
        assert_eq!(cells, 80 * 40, "no gaps, no overlaps in area");
        // 4 is above 1, 2 is to the right of 1, 3 is below 2, 5 is left of 3
        let at = |id| boxes.iter().find(|(i, _)| *i == id).unwrap().1;
        assert!(at(4).y < at(1).y && at(2).x > at(1).x && at(3).y > at(2).y && at(5).x < at(3).x);
    }

    #[test]
    fn terminals_open_by_themselves_like_hyprland() {
        // a wide screen: the second goes beside, the third below the second, the fourth beside it...
        let t = dwindle(&[1, 2, 3, 4], Rect::new(0, 0, 160, 50));
        let (boxes, _) = t.layout(Rect::new(0, 0, 160, 50));
        assert_eq!(boxes.len(), 4);
        let at = |id| boxes.iter().find(|(i, _)| *i == id).unwrap().1;
        assert!(at(2).x > at(1).x);
        assert!(at(3).y > at(2).y);
        assert!(boxes.iter().all(|(_, r)| r.width >= 20 && r.height >= 6), "{boxes:?}");
        assert_eq!(auto_side(Rect::new(0, 0, 100, 20)), Side::Right);
        assert_eq!(auto_side(Rect::new(0, 0, 50, 40)), Side::Bottom);
        assert_eq!(auto_side(Rect::new(0, 0, 30, 40)), Side::Bottom);
        assert_eq!(auto_side(Rect::new(0, 0, 60, 10)), Side::Right, "too short to split below");
    }

    #[test]
    fn removing_a_terminal_gives_its_room_to_the_neighbour() {
        let t = Tree::balanced(&[1, 2, 3], true).remove(1).unwrap();
        assert_eq!(t.leaves(), vec![2, 3]);
        let (boxes, _) = t.layout(AREA);
        assert_eq!(boxes.iter().map(|(_, r)| r.width as u32 * r.height as u32).sum::<u32>(), 80 * 40);
        assert_eq!(Tree::Leaf(1).remove(1), None);
    }

    #[test]
    fn swap_replace_and_resize() {
        let mut t = Tree::balanced(&[1, 2], true);
        t.swap(1, 2);
        assert_eq!(t.leaves(), vec![2, 1]);
        t.replace(2, 9);
        assert_eq!(t.leaves(), vec![9, 1]);
        t.set_pm(&[], 250);
        assert_eq!(t.layout(AREA).0[0].1.width, 20);
    }

    #[test]
    fn neighbours_follow_the_real_boxes() {
        let mut t = Tree::Leaf(1);
        t.dock(1, 2, Side::Right);
        t.dock(2, 3, Side::Bottom); // 1 on the left; 2 over 3 on the right
        let rects: Vec<Rect> = t.layout(AREA).0.iter().map(|(_, r)| *r).collect();
        let order = t.leaves(); // [1, 2, 3]
        assert_eq!(order, vec![1, 2, 3]);
        assert_eq!(neighbour(&rects, 0, 1, 0), Some(1), "right of 1: the upper one");
        assert_eq!(neighbour(&rects, 2, -1, 0), Some(0));
        assert_eq!(neighbour(&rects, 1, 0, 1), Some(2));
        assert_eq!(neighbour(&rects, 0, -1, 0), None);
    }

    #[test]
    fn the_edge_under_the_pointer_says_where_to_dock() {
        let r = Rect::new(10, 10, 40, 20);
        assert_eq!(dock_side(r, 11, 20), Some(Side::Left));
        assert_eq!(dock_side(r, 49, 20), Some(Side::Right));
        assert_eq!(dock_side(r, 30, 10), Some(Side::Top));
        assert_eq!(dock_side(r, 30, 29), Some(Side::Bottom));
        assert_eq!(dock_side(r, 30, 20), None);
        assert_eq!(dock_zone(r, Some(Side::Left)), Rect::new(10, 10, 20, 20));
    }
}
