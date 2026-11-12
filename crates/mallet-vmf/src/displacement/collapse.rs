//! The vertices VBSP leaves out where a displacement meets a neighbour of a lower power,
//! as `SetupAllowedVerts` in the Source SDK's `disp_common.cpp` decides them. A vertex
//! on a shared edge that the neighbour does not have is removed, with every vertex that
//! depends on it in the displacement's tree of nodes, and the search repeats until no
//! displacement loses another vertex; the game then draws the displacement without them,
//! so no cracks open along the edge. Hammer's Displacement Edge Collapse button shows
//! them as pink cubes.
//!
//! Positions follow VBSP's displacement space: `x` runs along a row and `y` from row to
//! row, so vertex `[x, y]` is the one at row `y`, column `x`. Edges are numbered left
//! (`x` = 0), top (`y` last), right (`x` last) and bottom (`y` = 0), each running from
//! corner `edge` to corner `edge + 1` of the displacement's corners.

use std::collections::{BTreeSet, HashMap};

use glam::DVec3;

use super::Surface;

/// How close, along every axis, two corners must be to count as one, as VBSP's
/// `FindEdge` tests them.
const TOLERANCE: f64 = 0.01;
const RIGHT: usize = 2;
const BOTTOM: usize = 3;

/// A vertex's place in displacement space: column, then row.
type Vert = [i32; 2];

/// How much of an edge a neighbour covers, as VBSP's `NeighborSpan`, in terms of an
/// edge running up or to the right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Span {
    CornerToCorner,
    CornerToMidpoint,
    MidpointToCorner,
}

/// The neighbour along part of an edge.
#[derive(Clone, Copy, Debug)]
struct SubNeighbour {
    /// The neighbour's place in the list of displacements.
    neighbour: usize,
    /// The neighbour's edge that touches this one.
    edge: usize,
    /// How much of this edge the neighbour covers.
    span: Span,
    /// How much of the neighbour's edge this displacement covers.
    neighbour_span: Span,
}

/// The vertices to each side of a node, at the node's step.
const SIDE_STEPS: [Vert; 4] = [[1, 0], [0, 1], [-1, 0], [0, -1]];
/// Where a node's four children are, at half the node's step.
const CHILD_STEPS: [Vert; 4] = [[1, 1], [-1, 1], [-1, -1], [1, -1]];
/// The two side vertices of its parent each child depends on.
const CHILD_DEPENDENCIES: [[Vert; 2]; 4] = [
    [[1, 0], [0, 1]],
    [[0, 1], [-1, 0]],
    [[-1, 0], [0, -1]],
    [[0, -1], [1, 0]],
];

/// VBSP's `CPowerInfo` for one power: the tree of nodes a displacement is tessellated
/// by, and which vertices depend on which.
#[derive(Debug)]
struct PowerInfo {
    power: i32,
    size: i32,
    root: Vert,
    side_verts: Vec<[Vert; 4]>,
    child_verts: Vec<[Vert; 4]>,
    /// For each vertex, the vertices that cannot exist without it.
    dependents: Vec<Vec<Vert>>,
}

impl PowerInfo {
    fn new(power: i32) -> Self {
        let size = (1 << power) + 1;
        let count = (size * size) as usize;
        let mut info = Self {
            power,
            size,
            root: [size / 2, size / 2],
            side_verts: vec![[[0; 2]; 4]; count],
            child_verts: vec![[[0; 2]; 4]; count],
            dependents: vec![Vec::new(); count],
        };
        info.add_node(info.root, [size - 1, size - 1], [0, 0], 0);
        info
    }

    fn index(&self, vert: Vert) -> usize {
        (vert[1] * self.size + vert[0]) as usize
    }

    /// Adds a node, which depends on two vertices, with its side vertices, which
    /// depend on it, and its children, as VBSP's `InitPowerInfo_R` does.
    fn add_node(&mut self, node: Vert, first: Vert, second: Vert, level: i32) {
        let at = self.index(node);
        for dependency in [first, second] {
            let index = self.index(dependency);
            self.dependents[index].push(node);
        }
        let step = 1 << (self.power - level - 1);
        for (side, offset) in SIDE_STEPS.iter().enumerate() {
            let vert = [node[0] + offset[0] * step, node[1] + offset[1] * step];
            self.side_verts[at][side] = vert;
            self.dependents[at].push(vert);
        }
        let child_step = step >> 1;
        if child_step == 0 {
            return;
        }
        for (child, offset) in CHILD_STEPS.iter().enumerate() {
            let vert = [node[0] + offset[0] * child_step, node[1] + offset[1] * child_step];
            self.child_verts[at][child] = vert;
            let [a, b] = CHILD_DEPENDENCIES[child];
            self.add_node(
                vert,
                [node[0] + a[0] * step, node[1] + a[1] * step],
                [node[0] + b[0] * step, node[1] + b[1] * step],
                level + 1,
            );
        }
    }
}

/// A displacement taking part: its corners, its power's node tree, and its neighbours
/// along each edge, two halves at most.
struct Disp<'a> {
    corners: [DVec3; 4],
    info: &'a PowerInfo,
    edges: [[Option<SubNeighbour>; 2]; 4],
}

/// For each surface, the indices of the vertices VBSP will leave out of it, sorted.
/// Displacements of powers other than 2, 3 and 4, which the compilers do not take,
/// lose nothing and are nobody's neighbour.
pub fn collapsed_vertices(surfaces: &[&Surface]) -> Vec<Vec<usize>> {
    let infos: HashMap<i32, PowerInfo> = (2..=4).map(|power| (power, PowerInfo::new(power))).collect();
    let taking_part: Vec<usize> = (0..surfaces.len())
        .filter(|&at| power_of(surfaces[at].size).is_some())
        .collect();
    let mut disps: Vec<Disp> = taking_part
        .iter()
        .map(|&at| Disp {
            corners: surfaces[at].corners,
            info: &infos[&power_of(surfaces[at].size).unwrap_or(2)],
            edges: [[None; 2]; 4],
        })
        .collect();
    for (main, other) in touching_pairs(&disps) {
        setup_edge_neighbours(&mut disps, main, other);
    }
    let mut allowed: Vec<Vec<bool>> = disps
        .iter()
        .map(|disp| vec![true; (disp.info.size * disp.info.size) as usize])
        .collect();
    loop {
        let mut any = false;
        for index in 0..disps.len() {
            let mut count = 0;
            disable_unallowed(&disps, &mut allowed, index, disps[index].info.root, 0, &mut count);
            any |= count > 0;
        }
        if !any {
            break;
        }
    }
    let mut result = vec![Vec::new(); surfaces.len()];
    for (disp, &at) in taking_part.iter().enumerate() {
        result[at] = (0..allowed[disp].len())
            .filter(|&vertex| !allowed[disp][vertex])
            .collect();
    }
    result
}

fn power_of(size: usize) -> Option<i32> {
    match size {
        5 => Some(2),
        9 => Some(3),
        17 => Some(4),
        _ => None,
    }
}

/// A corner's key in the lookup of corners, fine enough that corners within
/// [`TOLERANCE`] of each other almost always share it.
fn corner_key(point: DVec3) -> [i64; 3] {
    (point * 8.0).round().as_i64vec3().to_array()
}

/// The pairs of displacements that could be neighbours, the smaller index first, in
/// order: those where a corner or an edge's midpoint of one is a corner of the other.
/// VBSP tests every pair whose boxes touch, which finds the same neighbours.
fn touching_pairs(disps: &[Disp]) -> Vec<(usize, usize)> {
    let mut by_corner: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
    for (index, disp) in disps.iter().enumerate() {
        for corner in disp.corners {
            by_corner.entry(corner_key(corner)).or_default().push(index);
        }
    }
    let mut pairs = BTreeSet::new();
    for (index, disp) in disps.iter().enumerate() {
        let midpoints = (0..4).map(|edge| (disp.corners[edge] + disp.corners[(edge + 1) % 4]) * 0.5);
        for point in disp.corners.into_iter().chain(midpoints) {
            for &other in by_corner.get(&corner_key(point)).into_iter().flatten() {
                if other != index {
                    pairs.insert((index.min(other), index.max(other)));
                }
            }
        }
    }
    pairs.into_iter().collect()
}

fn same_point(a: DVec3, b: DVec3) -> bool {
    (a - b).abs().max_element() <= TOLERANCE
}

/// The edge of `disp` that runs from `from` to `to`, as VBSP's `FindEdge`.
fn find_edge(disp: &Disp, from: DVec3, to: DVec3) -> Option<usize> {
    (0..4).find(|&edge| same_point(disp.corners[edge], from) && same_point(disp.corners[(edge + 1) % 4], to))
}

/// Finds how `other` borders each edge of `main`, as VBSP's `SetupEdgeNeighbors`: the
/// whole edge, half of a longer edge, or one of two shorter edges meeting at its middle.
fn setup_edge_neighbours(disps: &mut [Disp], main: usize, other: usize) {
    for edge in 0..4 {
        let from = disps[main].corners[edge];
        let to = disps[main].corners[(edge + 1) % 4];
        let middle = (from + to) * 0.5;
        let other_disp = &disps[other];
        if let Some(found) = find_edge(other_disp, to, from) {
            add_neighbour(
                disps,
                (main, edge, 0, Span::CornerToCorner),
                (other, found, Span::CornerToCorner),
            );
        } else if let Some(found) = find_edge(other_disp, to, from * 2.0 - to) {
            add_neighbour(
                disps,
                (main, edge, 0, Span::CornerToCorner),
                (other, found, Span::CornerToMidpoint),
            );
        } else if let Some(found) = find_edge(other_disp, to * 2.0 - from, from) {
            add_neighbour(
                disps,
                (main, edge, 0, Span::CornerToCorner),
                (other, found, Span::MidpointToCorner),
            );
        } else {
            let flipped = usize::from(flips(edge));
            if let Some(found) = find_edge(&disps[other], middle, from) {
                add_neighbour(
                    disps,
                    (main, edge, flipped, Span::CornerToMidpoint),
                    (other, found, Span::CornerToCorner),
                );
            }
            if let Some(found) = find_edge(&disps[other], to, middle) {
                add_neighbour(
                    disps,
                    (main, edge, 1 - flipped, Span::MidpointToCorner),
                    (other, found, Span::CornerToCorner),
                );
            }
        }
    }
}

/// Whether an edge runs down or to the left, against the way spans are named.
fn flips(edge: usize) -> bool {
    edge == RIGHT || edge == BOTTOM
}

/// A span named along an edge's own direction, renamed to run up or to the right.
fn flip(edge: usize, span: Span) -> Span {
    match (flips(edge), span) {
        (true, Span::CornerToMidpoint) => Span::MidpointToCorner,
        (true, Span::MidpointToCorner) => Span::CornerToMidpoint,
        _ => span,
    }
}

/// Records that two displacements border each other, as VBSP's `AddNeighbor`, unless
/// either part of an edge is already taken.
fn add_neighbour(
    disps: &mut [Disp],
    (main, edge, sub, span): (usize, usize, usize, Span),
    (other, other_edge, other_span): (usize, usize, Span),
) {
    let span = flip(edge, span);
    let other_span = flip(other_edge, other_span);
    let other_sub = usize::from(other_span == Span::MidpointToCorner);
    if disps[main].edges[edge][sub].is_some() || disps[other].edges[other_edge][other_sub].is_some() {
        return;
    }
    disps[main].edges[edge][sub] = Some(SubNeighbour {
        neighbour: other,
        edge: other_edge,
        span,
        neighbour_span: other_span,
    });
    disps[other].edges[other_edge][other_sub] = Some(SubNeighbour {
        neighbour: main,
        edge,
        span: other_span,
        neighbour_span: span,
    });
}

/// The axis an edge keeps fixed: x for the left and right edges, y for the others.
fn edge_axis(edge: usize) -> usize {
    edge % 2
}

/// Which edge a vertex lies on, testing them in order, or `None` inside.
fn edge_of(vert: Vert, size: i32) -> Option<usize> {
    let last = size - 1;
    if vert[0] == 0 {
        Some(0)
    } else if vert[1] == last {
        Some(1)
    } else if vert[0] == last {
        Some(2)
    } else if vert[1] == 0 {
        Some(3)
    } else {
        None
    }
}

fn is_corner(vert: Vert, size: i32) -> bool {
    let last = size - 1;
    (vert[0] == 0 || vert[0] == last) && (vert[1] == 0 || vert[1] == last)
}

/// How much more finely, in powers of two, a neighbour's edge is divided than its
/// power says, where it covers this edge: VBSP's `g_ShiftInfos`.
fn power_shift(span: Span, neighbour_span: Span) -> Option<i32> {
    match (span, neighbour_span) {
        (Span::CornerToCorner, Span::CornerToCorner) => Some(0),
        (Span::CornerToCorner, _) => Some(-1),
        (_, Span::CornerToCorner) => Some(1),
        _ => None,
    }
}

/// The part of an edge a span covers, as its first and last vertex along the edge.
fn span_ends(size: i32, edge: usize, span: Span) -> (Vert, Vert) {
    let last = size - 1;
    let corners = [[0, 0], [0, last], [last, last], [last, 0]];
    let mut start = corners[edge];
    let mut end = corners[(edge + 1) % 4];
    let free = 1 - edge_axis(edge);
    let middle = size / 2;
    match (flips(edge), span) {
        (true, Span::CornerToMidpoint) | (false, Span::MidpointToCorner) => start[free] = middle,
        (true, Span::MidpointToCorner) | (false, Span::CornerToMidpoint) => end[free] = middle,
        _ => {}
    }
    (start, end)
}

/// Which of an edge's (up to two) neighbours a vertex on it reaches, as VBSP's
/// `GetSubNeighborIndex`. The middle of an edge where two neighbours meet is their
/// corner, which is always kept, so it reaches none.
fn sub_neighbour(disp: &Disp, edge: usize, vert: Vert) -> Option<usize> {
    let middle = disp.info.size / 2;
    let along = vert[1 - edge_axis(edge)];
    let sides = &disp.edges[edge];
    let mut sub = 0;
    if along == middle {
        if sides[0].is_some_and(|first| first.span != Span::CornerToCorner) {
            return None;
        }
    } else if along > middle {
        sub = 1;
    }
    if sides[sub].is_some() {
        return Some(sub);
    }
    let whole = sides[0].is_some_and(|first| first.span == Span::CornerToCorner);
    (sub == 1 && whole).then_some(0)
}

/// The neighbour a vertex on an edge reaches and the same vertex in the neighbour, as
/// VBSP's `TransformIntoSubNeighbor`.
fn into_neighbour(disps: &[Disp], disp: usize, edge: usize, sub: usize, vert: Vert) -> Option<(usize, Vert)> {
    let side = disps[disp].edges[edge][sub]?;
    let neighbour = &disps[side.neighbour];
    let (from_start, from_end) = span_ends(disps[disp].info.size, edge, side.span);
    let (to_end, to_start) = span_ends(neighbour.info.size, side.edge, side.neighbour_span);
    let free = 1 - edge_axis(edge);
    let fixed = (i64::from(vert[free] - from_start[free]) << 16) / i64::from(from_end[free] - from_start[free]);
    let axis = edge_axis(side.edge);
    let mut out = [0; 2];
    out[axis] = to_start[axis];
    out[1 - axis] = to_start[1 - axis] + (i64::from(to_end[1 - axis] - to_start[1 - axis]) * fixed / 65536) as i32;
    Some((side.neighbour, out))
}

/// Whether a side vertex of a node at `level` may exist, as VBSP's `IsVertAllowed`:
/// a vertex on an edge must also exist, and be kept, in the neighbour there.
fn is_allowed(disps: &[Disp], allowed: &[Vec<bool>], disp: usize, vert: Vert, level: i32) -> bool {
    let info = disps[disp].info;
    if is_corner(vert, info.size) {
        return true;
    }
    let Some(edge) = edge_of(vert, info.size) else {
        return true;
    };
    let Some(sub) = sub_neighbour(&disps[disp], edge, vert) else {
        return true;
    };
    let Some(side) = disps[disp].edges[edge][sub] else {
        return true;
    };
    let Some(shift) = power_shift(side.span, side.neighbour_span) else {
        return true;
    };
    if disps[side.neighbour].info.power + shift < level + 1 {
        return false;
    }
    let Some((neighbour, there)) = into_neighbour(disps, disp, edge, sub, vert) else {
        return true;
    };
    allowed[neighbour][disps[neighbour].info.index(there)]
}

/// Removes a vertex and every vertex that depends on it, as VBSP's `UnallowVerts_R`.
fn unallow(disps: &[Disp], allowed: &mut [Vec<bool>], disp: usize, vert: Vert, count: &mut usize) {
    let info = disps[disp].info;
    let index = info.index(vert);
    if !allowed[disp][index] {
        return;
    }
    allowed[disp][index] = false;
    *count += 1;
    for &dependent in &info.dependents[index] {
        unallow(disps, allowed, disp, dependent, count);
    }
}

/// Removes the side vertices of a node and its children that may not exist, as VBSP's
/// `DisableUnallowedVerts_R`.
fn disable_unallowed(
    disps: &[Disp],
    allowed: &mut [Vec<bool>],
    disp: usize,
    node: Vert,
    level: i32,
    count: &mut usize,
) {
    let info = disps[disp].info;
    let at = info.index(node);
    for side in info.side_verts[at] {
        if !is_allowed(disps, allowed, disp, side, level) {
            unallow(disps, allowed, disp, side, count);
        }
    }
    if level + 1 < info.power {
        for child in info.child_verts[at] {
            disable_unallowed(disps, allowed, disp, child, level + 1, count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::displacement::Displacement;

    /// A flat displacement of `power` over the square from `min` to `max` at height 0,
    /// its rows running along y and its columns along x.
    fn flat(min: [f64; 2], max: [f64; 2], power: u32) -> Surface {
        let polygon = vec![
            DVec3::new(max[0], max[1], 0.0),
            DVec3::new(max[0], min[1], 0.0),
            DVec3::new(min[0], min[1], 0.0),
            DVec3::new(min[0], max[1], 0.0),
        ];
        let displacement = Displacement::create(&polygon, DVec3::Z, power).unwrap();
        Surface::new(&displacement, &polygon, DVec3::Z).unwrap()
    }

    #[test]
    fn neighbours_of_one_power_keep_every_vertex() {
        let (a, b) = (flat([0.0, 0.0], [64.0, 64.0], 3), flat([64.0, 0.0], [128.0, 64.0], 3));
        assert_eq!(collapsed_vertices(&[&a, &b]), vec![Vec::<usize>::new(), Vec::new()]);
    }

    #[test]
    fn a_finer_edge_loses_the_vertices_its_coarser_neighbour_lacks() {
        let (fine, coarse) = (flat([0.0, 0.0], [64.0, 64.0], 3), flat([64.0, 0.0], [128.0, 64.0], 2));
        let collapsed = collapsed_vertices(&[&fine, &coarse]);
        assert_eq!(collapsed[0], vec![17, 35, 53, 71]);
        assert!(collapsed[1].is_empty());
        let lonely = collapsed_vertices(&[&fine]);
        assert!(lonely[0].is_empty());
    }

    #[test]
    fn two_powers_apart_also_removes_vertices_inside() {
        let (fine, coarse) = (flat([0.0, 0.0], [64.0, 64.0], 4), flat([64.0, 0.0], [128.0, 64.0], 2));
        let collapsed = collapsed_vertices(&[&fine, &coarse]);
        let on_edge: Vec<usize> = collapsed[0].iter().copied().filter(|index| index % 17 == 16).collect();
        let rows: Vec<usize> = on_edge.iter().map(|index| index / 17).collect();
        assert_eq!(rows, vec![1, 2, 3, 5, 6, 7, 9, 10, 11, 13, 14, 15]);
        assert!(collapsed[0].iter().any(|index| index % 17 != 16));
        assert!(collapsed[0].iter().all(|index| index % 17 >= 13));
    }

    #[test]
    fn two_halves_along_a_longer_edge_match_it() {
        let big = flat([0.0, 0.0], [128.0, 128.0], 3);
        let lower = flat([128.0, 0.0], [192.0, 64.0], 2);
        let upper = flat([128.0, 64.0], [192.0, 128.0], 2);
        let collapsed = collapsed_vertices(&[&big, &lower, &upper]);
        assert!(collapsed.iter().all(Vec::is_empty), "{collapsed:?}");
        let finer = flat([0.0, 0.0], [128.0, 128.0], 4);
        let collapsed = collapsed_vertices(&[&finer, &lower, &upper]);
        let rows: Vec<usize> = collapsed[0]
            .iter()
            .filter(|index| *index % 17 == 16)
            .map(|index| index / 17)
            .collect();
        assert_eq!(rows, vec![1, 3, 5, 7, 9, 11, 13, 15]);
        assert!(collapsed[1].is_empty() && collapsed[2].is_empty());
    }

    #[test]
    fn node_trees_match_vbsp() {
        let info = PowerInfo::new(3);
        assert_eq!(info.root, [4, 4]);
        assert_eq!(info.side_verts[info.index([4, 4])], [[8, 4], [4, 8], [0, 4], [4, 0]]);
        assert_eq!(info.child_verts[info.index([4, 4])], [[6, 6], [2, 6], [2, 2], [6, 2]]);
        assert!(info.dependents[info.index([8, 4])].contains(&[6, 6]));
        assert!(info.dependents[info.index([8, 4])].contains(&[6, 2]));
        assert!(info.dependents[info.index([6, 6])].contains(&[8, 6]));
    }
}
