//! Hammer's displacements: the `dispinfo` block a brush face keeps, the surface it makes,
//! and what Hammer's Displacement tools do to it.
//!
//! A displacement turns a four-sided face into a grid of `2^power + 1` vertices a side.
//! Each vertex starts on the flat face and moves along the face normal by the surface's
//! elevation, by its own normal times its distance, and by its offset from Subdivide, as
//! VBSP's `CCoreDispInfo::GenerateDispSurf` builds it. Rows run from the corner nearest
//! `startposition` towards the next corner of the face's winding, and each row runs
//! towards the corner before the start, so vertex `row * size + column` is the one a
//! VMF's `rowN` lists at place `column`.

use std::collections::HashMap;

use glam::DVec3;

use crate::geometry::Bounds;
use crate::kv::{Block, Item, Pair};
use crate::transform::clean;
use crate::{parse_numbers, parse_vec3, Side};

/// The name of the block a displacement face keeps its data in.
pub const BLOCK: &str = "dispinfo";
/// The smallest power Hammer's Create offers.
pub const MIN_POWER: u32 = 2;
/// The largest power the compilers accept.
pub const MAX_POWER: u32 = 4;
/// A triangle tag bit: the triangle is flat enough to walk on.
pub const TAG_WALKABLE: u32 = 1;
/// A triangle tag bit: the triangle is flat enough to build on.
pub const TAG_BUILDABLE: u32 = 8;
/// The lowest height of a triangle's normal that Hammer tags walkable.
const WALKABLE_NORMAL: f64 = 0.7;
/// The lowest height of a triangle's normal that Hammer tags buildable.
const BUILDABLE_NORMAL: f64 = 0.8;
/// Corners closer than this count as one corner when sewing and subdividing.
const WELD: f64 = 0.1;
/// What Hammer writes in `allowed_verts`: every vertex is allowed.
const ALLOWED_VERTS: &str = "-1 -1 -1 -1 -1 -1 -1 -1 -1 -1";
/// The keys and blocks of a `dispinfo`, in the order Hammer writes them.
const FIELDS: [&str; 12] = [
    "power",
    "startposition",
    "flags",
    "elevation",
    "subdiv",
    "normals",
    "distances",
    "offsets",
    "offset_normals",
    "alphas",
    "triangle_tags",
    "allowed_verts",
];
/// How many of [`FIELDS`] are keys; the rest are blocks.
const KEY_FIELDS: usize = 5;

/// The number of vertices a side of a displacement of this power has.
pub fn side_size(power: u32) -> usize {
    (1usize << power.min(MAX_POWER)) + 1
}

/// What a `dispinfo` block holds, with one entry per vertex, row by row.
#[derive(Debug, Clone, PartialEq)]
pub struct Displacement {
    /// The grid has `2^power + 1` vertices a side.
    pub power: u32,
    /// The corner of the face the first row starts at, or a point nearest it.
    pub start: DVec3,
    /// Hammer's collision flags: 2 for no physics, 4 for no hull and 8 for no ray
    /// collision.
    pub flags: u32,
    /// How far the whole surface sits off its face, along the face's normal.
    pub elevation: f64,
    /// Whether Subdivide made the offsets.
    pub subdivided: bool,
    /// The direction each vertex moves in.
    pub normals: Vec<DVec3>,
    /// How far each vertex moves along its normal; never negative.
    pub distances: Vec<f64>,
    /// How far Subdivide moved each vertex off the flat face.
    pub offsets: Vec<DVec3>,
    /// The surface normal Subdivide gave each vertex.
    pub offset_normals: Vec<DVec3>,
    /// How much of a blended material's second texture shows at each vertex, from 0 to
    /// 255.
    pub alphas: Vec<f64>,
    /// Each triangle's [`TAG_WALKABLE`] and [`TAG_BUILDABLE`] bits, and the bits that
    /// force them either way.
    pub triangle_tags: Vec<u32>,
    /// The pairs of the `allowed_verts` block, kept as written.
    pub allowed_verts: Vec<Item>,
    /// Keys and blocks Mallet does not know, each with how many known fields came before
    /// it, so they are written back where they were.
    pub extra: Vec<(usize, Item)>,
}

impl Displacement {
    /// A flat displacement, as Hammer's Create makes one: every vertex on the face and
    /// moving along `normal`, with no alpha. The power is kept between 0 and
    /// [`MAX_POWER`].
    pub fn new(power: u32, start: DVec3, normal: DVec3) -> Self {
        let power = power.min(MAX_POWER);
        let size = side_size(power);
        let count = size * size;
        let triangles = 2 * (size - 1) * (size - 1);
        Self {
            power,
            start,
            flags: 0,
            elevation: 0.0,
            subdivided: false,
            normals: vec![normal; count],
            distances: vec![0.0; count],
            offsets: vec![DVec3::ZERO; count],
            offset_normals: vec![normal; count],
            alphas: vec![0.0; count],
            triangle_tags: vec![tag_for(normal); triangles],
            allowed_verts: vec![Item::Pair(Pair::new("10", ALLOWED_VERTS))],
            extra: Vec::new(),
        }
    }

    /// Hammer's Create on a face with this polygon and outward normal: a flat
    /// displacement starting at the corner nearest the polygon's smallest corner, as
    /// Hammer picks it. `None` unless the polygon has four corners.
    pub fn create(polygon: &[DVec3], normal: DVec3, power: u32) -> Option<Self> {
        if polygon.len() != 4 {
            return None;
        }
        let mut bounds = Bounds::EMPTY;
        for corner in polygon {
            bounds.add_point(*corner);
        }
        let start = polygon.iter().copied().min_by(|a, b| {
            a.distance_squared(bounds.min)
                .total_cmp(&b.distance_squared(bounds.min))
        })?;
        Some(Self::new(power, start, normal))
    }

    /// Reads a `dispinfo` block. Rows that are missing or short read as zero. `None`
    /// when the power is missing or above [`MAX_POWER`].
    pub fn read(block: &Block) -> Option<Self> {
        let power: u32 = block.get("power")?.trim().parse().ok()?;
        if power > MAX_POWER {
            return None;
        }
        let size = side_size(power);
        let count = size * size;
        let number = |key: &str| block.get(key).and_then(|value| value.trim().parse::<f64>().ok());
        let vectors = |name: &str| {
            let values = rows(block, name, size, size * 3);
            (0..count)
                .map(|index| DVec3::new(values[index * 3], values[index * 3 + 1], values[index * 3 + 2]))
                .collect::<Vec<_>>()
        };
        let mut seen = [false; FIELDS.len()];
        let mut known = 0;
        let mut extra = Vec::new();
        for item in &block.items {
            let field = match item {
                Item::Pair(pair) => FIELDS[..KEY_FIELDS]
                    .iter()
                    .position(|name| pair.key.eq_ignore_ascii_case(name)),
                Item::Block(child) => FIELDS[KEY_FIELDS..]
                    .iter()
                    .position(|name| child.name.eq_ignore_ascii_case(name))
                    .map(|index| index + KEY_FIELDS),
            };
            match field {
                Some(field) if !seen[field] => {
                    seen[field] = true;
                    known += 1;
                }
                _ => extra.push((known, item.clone())),
            }
        }
        let tags = rows(block, "triangle_tags", size - 1, 2 * (size - 1));
        Some(Self {
            power,
            start: block.get("startposition").and_then(parse_vec3).unwrap_or_default(),
            flags: number("flags").map_or(0, |flags| flags.max(0.0) as u32),
            elevation: number("elevation").unwrap_or(0.0),
            subdivided: number("subdiv").is_some_and(|value| value != 0.0),
            normals: vectors("normals"),
            distances: rows(block, "distances", size, size),
            offsets: vectors("offsets"),
            offset_normals: vectors("offset_normals"),
            alphas: rows(block, "alphas", size, size),
            triangle_tags: tags.into_iter().map(|tag| tag.max(0.0) as u32).collect(),
            allowed_verts: block.blocks_named("allowed_verts").next().map_or_else(
                || vec![Item::Pair(Pair::new("10", ALLOWED_VERTS))],
                |child| child.items.clone(),
            ),
            extra,
        })
    }

    /// The `dispinfo` block, laid out as Hammer writes it, with the keys and blocks
    /// Mallet does not know back where they were.
    pub fn to_block(&self) -> Block {
        let size = self.size();
        let vectors = |name: &str, vectors: &[DVec3]| {
            let values: Vec<f64> = vectors.iter().flat_map(|vector| vector.to_array()).collect();
            Item::Block(rows_block(name, &values, size * 3))
        };
        let tags: Vec<f64> = self.triangle_tags.iter().map(|tag| f64::from(*tag)).collect();
        let fields = [
            Item::Pair(Pair::new("power", self.power.to_string())),
            Item::Pair(Pair::new(
                "startposition",
                format!(
                    "[{} {} {}]",
                    format_number(self.start.x),
                    format_number(self.start.y),
                    format_number(self.start.z)
                ),
            )),
            Item::Pair(Pair::new("flags", self.flags.to_string())),
            Item::Pair(Pair::new("elevation", format_number(self.elevation))),
            Item::Pair(Pair::new("subdiv", u8::from(self.subdivided).to_string())),
            vectors("normals", &self.normals),
            Item::Block(rows_block("distances", &self.distances, size)),
            vectors("offsets", &self.offsets),
            vectors("offset_normals", &self.offset_normals),
            Item::Block(rows_block("alphas", &self.alphas, size)),
            Item::Block(rows_block("triangle_tags", &tags, 2 * (size - 1))),
            Item::Block(Block {
                name: "allowed_verts".to_owned(),
                items: self.allowed_verts.clone(),
            }),
        ];
        let mut block = Block::new(BLOCK);
        let extras_at = |slot: &dyn Fn(usize) -> bool| {
            self.extra
                .iter()
                .filter(|(at, _)| slot(*at))
                .map(|(_, item)| item.clone())
                .collect::<Vec<_>>()
        };
        for (position, field) in fields.into_iter().enumerate() {
            block.items.extend(extras_at(&|at| at == position));
            block.items.push(field);
        }
        block.items.extend(extras_at(&|at| at >= FIELDS.len()));
        block
    }

    /// Vertices a side.
    pub fn size(&self) -> usize {
        side_size(self.power)
    }

    /// How far vertex `index` moves, besides the elevation and its offset: its normal
    /// times its distance.
    pub fn vector(&self, index: usize) -> DVec3 {
        self.normals[index] * self.distances[index]
    }

    /// Sets how far vertex `index` moves, as a direction and a distance that is never
    /// negative. A vertex that does not move keeps its direction, or takes `fallback`
    /// when it has none.
    pub fn set_vector(&mut self, index: usize, vector: DVec3, fallback: DVec3) {
        let distance = vector.length();
        if distance < 1e-6 {
            if self.normals[index].length_squared() < 0.5 {
                self.normals[index] = fallback;
            }
            self.distances[index] = 0.0;
        } else {
            self.normals[index] = vector / distance;
            self.distances[index] = distance;
        }
    }

    /// Moves vertex `index` of `surface` to `position` by changing its vector.
    pub fn move_vertex(&mut self, surface: &Surface, index: usize, position: DVec3) {
        let base = surface.flat[index] + surface.normal * self.elevation + self.offsets[index];
        self.set_vector(index, position - base, surface.normal);
    }

    /// Hammer's Invert Alpha: what showed the first texture shows the second, and the
    /// other way round.
    pub fn invert_alpha(&mut self) {
        for alpha in &mut self.alphas {
            *alpha = (255.0 - *alpha).clamp(0.0, 255.0);
        }
    }

    /// Tags each triangle walkable and buildable by how upright it is, as Hammer does
    /// after every edit, keeping the bits that force a tag either way.
    pub fn update_tags(&mut self, surface: &Surface) {
        let triangles = surface.triangles();
        self.triangle_tags.resize(triangles.len(), 0);
        for (tag, [a, b, c]) in self.triangle_tags.iter_mut().zip(triangles) {
            let [a, b, c] = [a, b, c].map(|index| surface.positions[index]);
            let normal = (b - a).cross(c - a).normalize_or_zero();
            *tag = (*tag & !(TAG_WALKABLE | TAG_BUILDABLE)) | tag_for(normal);
        }
    }
}

/// The tag bits of a triangle facing this way.
fn tag_for(normal: DVec3) -> u32 {
    let mut tag = 0;
    if normal.z >= WALKABLE_NORMAL {
        tag |= TAG_WALKABLE;
    }
    if normal.z >= BUILDABLE_NORMAL {
        tag |= TAG_BUILDABLE;
    }
    tag
}

/// Writes a number as Hammer would read it back: six decimals at most, no `-0`.
fn format_number(value: f64) -> String {
    clean(value).to_string()
}

/// The numbers of a block of `rowN` keys, `count` rows of `length` numbers, with what
/// is missing or does not parse read as zero.
fn rows(block: &Block, name: &str, count: usize, length: usize) -> Vec<f64> {
    let mut values = vec![0.0; count * length];
    let Some(child) = block.blocks_named(name).next() else {
        return values;
    };
    for pair in child.pairs() {
        let row = pair
            .key
            .get(..3)
            .filter(|prefix| prefix.eq_ignore_ascii_case("row"))
            .and_then(|_| pair.key[3..].trim().parse::<usize>().ok());
        let Some(row) = row.filter(|row| *row < count) else {
            continue;
        };
        let numbers = parse_numbers(&pair.value).unwrap_or_default();
        for (at, number) in numbers.into_iter().take(length).enumerate() {
            values[row * length + at] = number;
        }
    }
    values
}

/// A block of `rowN` keys holding `values`, `length` of them a row.
fn rows_block(name: &str, values: &[f64], length: usize) -> Block {
    let mut block = Block::new(name);
    for (row, chunk) in values.chunks(length.max(1)).enumerate() {
        let text = chunk
            .iter()
            .map(|value| format_number(*value))
            .collect::<Vec<_>>()
            .join(" ");
        block.push_pair(format!("row{row}"), text);
    }
    block
}

impl Side {
    /// The face's displacement, read from its `dispinfo` block.
    pub fn displacement(&self) -> Option<Displacement> {
        Displacement::read(self.dispinfo()?)
    }

    /// Makes the face a displacement, or a plain face again with `None`. A new
    /// `dispinfo` goes after the typed fields, where Hammer writes it.
    pub fn set_displacement(&mut self, displacement: Option<&Displacement>) {
        let at = self
            .extra
            .iter()
            .position(|item| matches!(item, Item::Block(block) if block.name.eq_ignore_ascii_case(BLOCK)));
        match (at, displacement) {
            (Some(at), Some(displacement)) => self.extra[at] = Item::Block(displacement.to_block()),
            (Some(at), None) => {
                self.extra.remove(at);
                if at < self.extra_slots.len() {
                    self.extra_slots.remove(at);
                }
            }
            (None, Some(displacement)) => {
                self.extra_slots.truncate(self.extra.len());
                self.extra.push(Item::Block(displacement.to_block()));
            }
            (None, None) => {}
        }
    }
}

/// The surface a displacement makes on its face, vertex by vertex and row by row.
#[derive(Debug, Clone, PartialEq)]
pub struct Surface {
    /// Vertices a side.
    pub size: usize,
    /// The face's corners in VBSP's winding, clockwise seen from in front, starting at
    /// the one nearest the displacement's start position. Rows run from the first
    /// towards the second; each row runs towards the fourth.
    pub corners: [DVec3; 4],
    /// The face's outward normal.
    pub normal: DVec3,
    /// Each vertex on the flat face, where its texture coordinates come from.
    pub flat: Vec<DVec3>,
    /// Each vertex where the displacement puts it.
    pub positions: Vec<DVec3>,
    /// Each vertex's normal, averaged from the triangles around it, for shading.
    pub normals: Vec<DVec3>,
    /// Each vertex's alpha, from 0 to 255.
    pub alphas: Vec<f64>,
    /// The box around the positions.
    pub bounds: Bounds,
}

impl Surface {
    /// The surface `displacement` makes on a face with this polygon and outward normal.
    /// `None` unless the polygon has four corners, as VBSP requires.
    pub fn new(displacement: &Displacement, polygon: &[DVec3], normal: DVec3) -> Option<Self> {
        let [first, mut second, third, mut fourth]: [DVec3; 4] = polygon.try_into().ok()?;
        if (second - first).cross(fourth - first).dot(normal) > 0.0 {
            std::mem::swap(&mut second, &mut fourth);
        }
        let winding = [first, second, third, fourth];
        let start = (0..4)
            .min_by(|&a, &b| {
                winding[a]
                    .distance_squared(displacement.start)
                    .total_cmp(&winding[b].distance_squared(displacement.start))
            })
            .unwrap_or(0);
        let corners = [0, 1, 2, 3].map(|step| winding[(start + step) % 4]);
        let size = displacement.size();
        let steps = (size - 1) as f64;
        let mut flat = Vec::with_capacity(size * size);
        for row in 0..size {
            let along = row as f64 / steps;
            let left = corners[0].lerp(corners[1], along);
            let right = corners[3].lerp(corners[2], along);
            for column in 0..size {
                flat.push(left.lerp(right, column as f64 / steps));
            }
        }
        let mut surface = Self {
            size,
            corners,
            normal,
            flat,
            positions: Vec::new(),
            normals: Vec::new(),
            alphas: Vec::new(),
            bounds: Bounds::EMPTY,
        };
        surface.refresh(displacement);
        Some(surface)
    }

    /// Moves the vertices to where `displacement` puts them now, as after an edit.
    pub fn refresh(&mut self, displacement: &Displacement) {
        let lift = self.normal * displacement.elevation;
        self.positions = self
            .flat
            .iter()
            .enumerate()
            .map(|(index, flat)| *flat + lift + displacement.vector(index) + displacement.offsets[index])
            .collect();
        self.alphas = displacement.alphas.clone();
        self.alphas.resize(self.flat.len(), 0.0);
        self.bounds = Bounds::EMPTY;
        for position in &self.positions {
            self.bounds.add_point(*position);
        }
        self.normals = self.vertex_normals();
    }

    /// The index of the vertex at a row and column.
    pub fn index(&self, row: usize, column: usize) -> usize {
        row * self.size + column
    }

    /// The vertex at corner `corner` of [`Surface::corners`].
    pub fn corner_vertex(&self, corner: usize) -> usize {
        let last = self.size - 1;
        match corner % 4 {
            0 => self.index(0, 0),
            1 => self.index(last, 0),
            2 => self.index(last, last),
            _ => self.index(0, last),
        }
    }

    /// The vertices along edge `edge`, from corner `edge` to the corner after it.
    pub fn edge_vertices(&self, edge: usize) -> Vec<usize> {
        let last = self.size - 1;
        match edge % 4 {
            0 => (0..=last).map(|row| self.index(row, 0)).collect(),
            1 => (0..=last).map(|column| self.index(last, column)).collect(),
            2 => (0..=last).rev().map(|row| self.index(row, last)).collect(),
            _ => (0..=last).rev().map(|column| self.index(0, column)).collect(),
        }
    }

    /// The surface's triangles as vertex indices, two a grid square in the order VBSP
    /// makes them, each wound counter-clockwise seen from in front. The squares'
    /// diagonals alternate, so the triangles form Source's diamond pattern.
    pub fn triangles(&self) -> Vec<[usize; 3]> {
        let size = self.size;
        let mut triangles = Vec::with_capacity(2 * (size - 1) * (size - 1));
        for row in 0..size - 1 {
            for column in 0..size - 1 {
                let corner = row * size + column;
                let (across, up) = (corner + 1, corner + size);
                let diagonal = up + 1;
                if corner % 2 == 1 {
                    triangles.push([corner, across, up]);
                    triangles.push([across, diagonal, up]);
                } else {
                    triangles.push([corner, diagonal, up]);
                    triangles.push([corner, across, diagonal]);
                }
            }
        }
        triangles
    }

    /// Every edge of the triangles once: the wireframe the 2D views draw.
    pub fn edges(&self) -> Vec<[DVec3; 2]> {
        let size = self.size;
        let at = |row: usize, column: usize| self.positions[row * size + column];
        let mut edges = Vec::with_capacity(3 * size * size);
        for row in 0..size {
            for column in 0..size - 1 {
                edges.push([at(row, column), at(row, column + 1)]);
            }
        }
        for row in 0..size - 1 {
            for column in 0..size {
                edges.push([at(row, column), at(row + 1, column)]);
            }
            for column in 0..size - 1 {
                if (row * size + column) % 2 == 1 {
                    edges.push([at(row, column + 1), at(row + 1, column)]);
                } else {
                    edges.push([at(row, column), at(row + 1, column + 1)]);
                }
            }
        }
        edges
    }

    /// The edges around the surface.
    pub fn outline(&self) -> Vec<[DVec3; 2]> {
        (0..4)
            .flat_map(|edge| {
                let vertices = self.edge_vertices(edge);
                vertices
                    .windows(2)
                    .map(|pair| [self.positions[pair[0]], self.positions[pair[1]]])
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// How far along a ray it first meets the surface, from either side.
    pub fn ray_hit(&self, origin: DVec3, direction: DVec3) -> Option<f64> {
        if !ray_meets_box(origin, direction, &self.bounds) {
            return None;
        }
        self.triangles()
            .into_iter()
            .filter_map(|[a, b, c]| {
                ray_triangle(
                    origin,
                    direction,
                    self.positions[a],
                    self.positions[b],
                    self.positions[c],
                )
            })
            .min_by(f64::total_cmp)
    }

    /// The vertex nearest a point.
    pub fn nearest_vertex(&self, point: DVec3) -> Option<usize> {
        (0..self.positions.len()).min_by(|&a, &b| {
            self.positions[a]
                .distance_squared(point)
                .total_cmp(&self.positions[b].distance_squared(point))
        })
    }

    fn vertex_normals(&self) -> Vec<DVec3> {
        let mut sums = vec![DVec3::ZERO; self.positions.len()];
        for [a, b, c] in self.triangles() {
            let [pa, pb, pc] = [a, b, c].map(|index| self.positions[index]);
            let weighted = (pb - pa).cross(pc - pa);
            for index in [a, b, c] {
                sums[index] += weighted;
            }
        }
        sums.into_iter().map(|sum| sum.normalize_or(self.normal)).collect()
    }
}

/// Whether a ray passes through a box, grown a little so rays along a flat surface count.
fn ray_meets_box(origin: DVec3, direction: DVec3, bounds: &Bounds) -> bool {
    if bounds.is_empty() {
        return false;
    }
    let margin = DVec3::splat(0.01);
    let (min, max) = (bounds.min - margin, bounds.max + margin);
    let mut enter = f64::NEG_INFINITY;
    let mut exit = f64::INFINITY;
    for axis in 0..3 {
        if direction[axis].abs() < 1e-12 {
            if origin[axis] < min[axis] || origin[axis] > max[axis] {
                return false;
            }
            continue;
        }
        let a = (min[axis] - origin[axis]) / direction[axis];
        let b = (max[axis] - origin[axis]) / direction[axis];
        enter = enter.max(a.min(b));
        exit = exit.min(a.max(b));
    }
    exit >= enter.max(0.0)
}

/// Where a ray meets a triangle, from either side, by the Möller–Trumbore test.
fn ray_triangle(origin: DVec3, direction: DVec3, a: DVec3, b: DVec3, c: DVec3) -> Option<f64> {
    let (first, second) = (b - a, c - a);
    let p = direction.cross(second);
    let determinant = first.dot(p);
    if determinant.abs() < 1e-12 {
        return None;
    }
    let inverse = 1.0 / determinant;
    let offset = origin - a;
    let u = offset.dot(p) * inverse;
    if !(-1e-9..=1.0 + 1e-9).contains(&u) {
        return None;
    }
    let q = offset.cross(first);
    let v = direction.dot(q) * inverse;
    if v < -1e-9 || u + v > 1.0 + 1e-9 {
        return None;
    }
    let distance = second.dot(q) * inverse;
    (distance >= 0.0).then_some(distance)
}

/// A displacement face being edited: its side's id, its data and its surface.
#[derive(Debug, Clone, PartialEq)]
pub struct Patch {
    pub side: u32,
    pub displacement: Displacement,
    pub surface: Surface,
}

impl Patch {
    /// The displacement of a face with this polygon and outward normal; `None` when the
    /// face is not a displacement or has other than four corners.
    pub fn new(side: &Side, polygon: &[DVec3], normal: DVec3) -> Option<Self> {
        let displacement = side.displacement()?;
        let surface = Surface::new(&displacement, polygon, normal)?;
        Some(Self {
            side: side.id,
            displacement,
            surface,
        })
    }

    /// Moves the surface to the data and tags the triangles again, after an edit.
    pub fn refresh(&mut self) {
        self.surface.refresh(&self.displacement);
        self.displacement.update_tags(&self.surface);
    }

    /// Moves vertex `index` to `position`. Call [`Patch::refresh`] once all are moved.
    pub fn move_vertex(&mut self, index: usize, position: DVec3) {
        self.displacement.move_vertex(&self.surface, index, position);
    }
}

/// How a brush's strength falls off towards its edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Falloff {
    /// Full strength at the centre, easing to nothing at the edge: Hammer's soft edge.
    #[default]
    Soft,
    /// Full strength out to the edge: Hammer's hard edge.
    Hard,
}

/// Which way Paint Geometry moves vertices.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum PaintAxis {
    /// Along each face's normal.
    #[default]
    FaceNormal,
    /// Along one direction for every face, such as the world's X, Y or Z axis.
    Direction(DVec3),
}

/// What a brush does to the vertices it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Effect {
    /// Adds the amount: raises, or lowers when the amount is below zero.
    #[default]
    Raise,
    /// Moves towards the amount, measured from the flat face: Hammer's Raise To.
    RaiseTo,
    /// Moves towards the average of the neighbouring vertices.
    Smooth,
}

/// What a brush paints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Channel {
    /// Vertex positions: Paint Geometry.
    #[default]
    Geometry,
    /// Vertex alphas, which blend a two-texture material: Paint Alpha.
    Alpha,
}

/// One application of the paint brush.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dab {
    /// Where the brush is, normally where the mouse ray meets the surface.
    pub center: DVec3,
    /// How far the brush reaches.
    pub radius: f64,
    pub falloff: Falloff,
    pub effect: Effect,
    pub channel: Channel,
    /// Units for geometry, alpha from 0 to 255 for alpha.
    pub amount: f64,
    /// The direction geometry moves in; alpha ignores it.
    pub axis: PaintAxis,
}

impl Dab {
    /// How strongly the brush acts on a vertex this far from its centre: 1 at the
    /// centre and nothing beyond its radius.
    pub fn strength(&self, distance: f64) -> f64 {
        if distance > self.radius || self.radius <= 0.0 {
            return 0.0;
        }
        match self.falloff {
            Falloff::Hard => 1.0,
            Falloff::Soft => 0.5 + 0.5 * (std::f64::consts::PI * distance / self.radius).cos(),
        }
    }
}

/// Applies one dab of the brush to every vertex within its reach, on every patch, and
/// returns the indices of the patches it changed.
pub fn paint(patches: &mut [Patch], dab: &Dab) -> Vec<usize> {
    let mut changed = Vec::new();
    for (index, patch) in patches.iter_mut().enumerate() {
        let near = patch.surface.bounds.min - DVec3::splat(dab.radius);
        let far = patch.surface.bounds.max + DVec3::splat(dab.radius);
        if dab.center.cmplt(near).any() || dab.center.cmpgt(far).any() {
            continue;
        }
        let weights: Vec<(usize, f64)> = patch
            .surface
            .positions
            .iter()
            .enumerate()
            .filter_map(|(vertex, position)| {
                let strength = dab.strength(position.distance(dab.center));
                (strength > 0.0).then_some((vertex, strength))
            })
            .collect();
        if weights.is_empty() {
            continue;
        }
        match dab.channel {
            Channel::Geometry => paint_geometry(patch, dab, &weights),
            Channel::Alpha => paint_alpha(patch, dab, &weights),
        }
        patch.refresh();
        changed.push(index);
    }
    changed
}

fn paint_geometry(patch: &mut Patch, dab: &Dab, weights: &[(usize, f64)]) {
    let surface = patch.surface.clone();
    let axis = match dab.axis {
        PaintAxis::FaceNormal => surface.normal,
        PaintAxis::Direction(direction) => direction.normalize_or(surface.normal),
    };
    let averages = (dab.effect == Effect::Smooth).then(|| neighbour_averages(surface.size, &surface.positions));
    for &(vertex, weight) in weights {
        let position = surface.positions[vertex];
        let shift = match (dab.effect, &averages) {
            (Effect::Raise, _) => dab.amount * weight,
            (Effect::RaiseTo, _) => (dab.amount - (position - surface.flat[vertex]).dot(axis)) * weight,
            (Effect::Smooth, Some(averages)) => (averages[vertex] - position).dot(axis) * weight,
            (Effect::Smooth, None) => 0.0,
        };
        patch
            .displacement
            .move_vertex(&surface, vertex, position + axis * shift);
    }
}

fn paint_alpha(patch: &mut Patch, dab: &Dab, weights: &[(usize, f64)]) {
    let before = patch.surface.alphas.clone();
    let averages = (dab.effect == Effect::Smooth).then(|| {
        let values: Vec<DVec3> = before.iter().map(|alpha| DVec3::splat(*alpha)).collect();
        neighbour_averages(patch.surface.size, &values)
    });
    patch.displacement.alphas.resize(before.len(), 0.0);
    for &(vertex, weight) in weights {
        let alpha = before[vertex];
        let target = match (dab.effect, &averages) {
            (Effect::Raise, _) => alpha + dab.amount,
            (Effect::RaiseTo, _) => dab.amount,
            (Effect::Smooth, Some(averages)) => averages[vertex].x,
            (Effect::Smooth, None) => alpha,
        };
        patch.displacement.alphas[vertex] = (alpha + (target - alpha) * weight).clamp(0.0, 255.0);
    }
}

/// The average of each grid vertex's neighbours, the eight around it, or fewer at the
/// edges.
fn neighbour_averages(size: usize, values: &[DVec3]) -> Vec<DVec3> {
    let mut averages = Vec::with_capacity(values.len());
    for row in 0..size {
        for column in 0..size {
            let mut sum = DVec3::ZERO;
            let mut count = 0.0;
            for next_row in row.saturating_sub(1)..=(row + 1).min(size - 1) {
                for next_column in column.saturating_sub(1)..=(column + 1).min(size - 1) {
                    if (next_row, next_column) != (row, column) {
                        sum += values[next_row * size + next_column];
                        count += 1.0;
                    }
                }
            }
            averages.push(if count > 0.0 {
                sum / count
            } else {
                values[row * size + column]
            });
        }
    }
    averages
}

/// An edge by the weld keys of its ends, the smaller first.
type EdgeKey = ([i64; 3], [i64; 3]);

/// A point rounded to the weld distance, so corners that meet share a key.
fn weld_key(point: DVec3) -> [i64; 3] {
    (point / WELD).round().as_i64vec3().to_array()
}

/// Hammer's Sew: the vertices on the corners and edges that patches share move to where
/// the patches meet, the average of their positions, so no gap opens between them. An
/// edge shared with a patch of a lower power follows that patch's vertices, the
/// vertices between them lying on the line from one to the next. Returns the indices
/// of the patches that changed.
pub fn sew(patches: &mut [Patch]) -> Vec<usize> {
    let mut targets: HashMap<(usize, usize), DVec3> = HashMap::new();
    let mut corners: HashMap<[i64; 3], Vec<(usize, usize)>> = HashMap::new();
    let mut edges: HashMap<EdgeKey, Vec<(usize, usize)>> = HashMap::new();
    for (index, patch) in patches.iter().enumerate() {
        let surface = &patch.surface;
        for corner in 0..4 {
            corners
                .entry(weld_key(surface.corners[corner]))
                .or_default()
                .push((index, surface.corner_vertex(corner)));
            let (from, to) = (
                weld_key(surface.corners[corner]),
                weld_key(surface.corners[(corner + 1) % 4]),
            );
            let key = if from <= to { (from, to) } else { (to, from) };
            edges.entry(key).or_default().push((index, corner));
        }
    }
    for members in corners.values().filter(|members| members.len() > 1) {
        let average = members
            .iter()
            .map(|&(patch, vertex)| patches[patch].surface.positions[vertex])
            .sum::<DVec3>()
            / members.len() as f64;
        for member in members {
            targets.insert(*member, average);
        }
    }
    for members in edges.values() {
        for (at, &first) in members.iter().enumerate() {
            for &second in &members[at + 1..] {
                if first.0 != second.0 {
                    sew_edge(patches, first, second, &mut targets);
                }
            }
        }
    }
    let mut changed: Vec<usize> = Vec::new();
    for ((patch, vertex), target) in targets {
        if patches[patch].surface.positions[vertex].distance_squared(target) > 1e-12 {
            patches[patch].move_vertex(vertex, target);
            if !changed.contains(&patch) {
                changed.push(patch);
            }
        }
    }
    changed.sort_unstable();
    for &patch in &changed {
        patches[patch].refresh();
    }
    changed
}

/// Sews one edge two patches share, given as each patch and the number of its edge.
fn sew_edge(
    patches: &[Patch],
    (first, first_edge): (usize, usize),
    (second, second_edge): (usize, usize),
    targets: &mut HashMap<(usize, usize), DVec3>,
) {
    let mut a = (first, patches[first].surface.edge_vertices(first_edge));
    let mut b = (second, patches[second].surface.edge_vertices(second_edge));
    let start = weld_key(patches[first].surface.corners[first_edge]);
    if weld_key(patches[second].surface.corners[second_edge]) != start {
        b.1.reverse();
    }
    if a.1.len() > b.1.len() {
        std::mem::swap(&mut a, &mut b);
    }
    let (coarse, coarse_vertices) = (a.0, &a.1);
    let (fine, fine_vertices) = (b.0, &b.1);
    let last = coarse_vertices.len() - 1;
    let step = (fine_vertices.len() - 1) / last.max(1);
    let position = |patch: usize, vertex: usize| patches[patch].surface.positions[vertex];
    let meeting: Vec<DVec3> = (0..=last)
        .map(|at| {
            let (coarse_vertex, fine_vertex) = (coarse_vertices[at], fine_vertices[at * step]);
            match targets.get(&(coarse, coarse_vertex)) {
                Some(corner) if at == 0 || at == last => *corner,
                _ => (position(coarse, coarse_vertex) + position(fine, fine_vertex)) * 0.5,
            }
        })
        .collect();
    for at in 0..=last {
        if at > 0 && at < last {
            targets.insert((coarse, coarse_vertices[at]), meeting[at]);
            targets.insert((fine, fine_vertices[at * step]), meeting[at]);
        }
        if at < last {
            for between in 1..step {
                let blend = between as f64 / step as f64;
                targets.insert(
                    (fine, fine_vertices[at * step + between]),
                    meeting[at].lerp(meeting[at + 1], blend),
                );
            }
        }
    }
}

/// Hammer's Subdivide: the patches' faces are taken as one mesh of four-sided faces,
/// joined where their corners meet, and smoothed by Catmull-Clark subdivision once per
/// power, so a box of displacements becomes a rounded shape. Each vertex's offset is
/// where the subdivided mesh puts it, and its offset normal the subdivided mesh's
/// normal; painted distances stay on top. Every patch must have the same power.
/// Returns the indices of the patches that changed.
pub fn subdivide(patches: &mut [Patch]) -> Result<Vec<usize>, String> {
    let Some(power) = patches.first().map(|patch| patch.displacement.power) else {
        return Ok(Vec::new());
    };
    if patches.iter().any(|patch| patch.displacement.power != power) {
        return Err("Subdivide needs displacements that all have the same power".to_owned());
    }
    let mut points: Vec<DVec3> = Vec::new();
    let mut welded: HashMap<[i64; 3], usize> = HashMap::new();
    let mut grids: Vec<Grid> = patches
        .iter()
        .map(|patch| {
            let corners = patch.surface.corners;
            let mut point = |corner: DVec3| {
                *welded.entry(weld_key(corner)).or_insert_with(|| {
                    points.push(corner);
                    points.len() - 1
                })
            };
            let cells = vec![
                point(corners[0]),
                point(corners[3]),
                point(corners[1]),
                point(corners[2]),
            ];
            Grid { size: 2, cells }
        })
        .collect();
    for _ in 0..power {
        (points, grids) = catmull_clark(&points, &grids);
    }
    let mut changed = Vec::new();
    for (index, (patch, grid)) in patches.iter_mut().zip(&grids).enumerate() {
        if grid.size != patch.surface.size {
            continue;
        }
        let normals = grid_normals(grid, &points, patch.surface.normal);
        for vertex in 0..patch.surface.flat.len() {
            patch.displacement.offsets[vertex] = points[grid.cells[vertex]] - patch.surface.flat[vertex];
            patch.displacement.offset_normals[vertex] = normals[vertex];
        }
        patch.displacement.subdivided = true;
        patch.refresh();
        changed.push(index);
    }
    Ok(changed)
}

/// A square grid of point indices, row by row, laid out like a displacement's vertices.
#[derive(Debug, Clone)]
struct Grid {
    size: usize,
    cells: Vec<usize>,
}

impl Grid {
    fn at(&self, row: usize, column: usize) -> usize {
        self.cells[row * self.size + column]
    }

    /// The grid's squares as their four points, in order around each.
    fn quads(&self) -> impl Iterator<Item = [usize; 4]> + '_ {
        (0..self.size - 1).flat_map(move |row| {
            (0..self.size - 1).map(move |column| {
                [
                    self.at(row, column),
                    self.at(row + 1, column),
                    self.at(row + 1, column + 1),
                    self.at(row, column + 1),
                ]
            })
        })
    }
}

/// One step of Catmull-Clark subdivision over every square of every grid. Each grid
/// becomes one of twice the resolution. Edges with one square are boundaries, which
/// stay sharp, and points with one square are corners, which stay put.
fn catmull_clark(points: &[DVec3], grids: &[Grid]) -> (Vec<DVec3>, Vec<Grid>) {
    let quads: Vec<[usize; 4]> = grids.iter().flat_map(Grid::quads).collect();
    let face_points: Vec<DVec3> = quads
        .iter()
        .map(|quad| quad.iter().map(|&point| points[point]).sum::<DVec3>() / 4.0)
        .collect();
    let edge_key = |a: usize, b: usize| if a < b { (a, b) } else { (b, a) };
    let mut edge_faces: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    let mut point_faces: Vec<Vec<usize>> = vec![Vec::new(); points.len()];
    for (face, quad) in quads.iter().enumerate() {
        for corner in 0..4 {
            let (a, b) = (quad[corner], quad[(corner + 1) % 4]);
            let faces = edge_faces.entry(edge_key(a, b)).or_default();
            if !faces.contains(&face) {
                faces.push(face);
            }
            point_faces[a].push(face);
        }
    }
    let mut point_edges: Vec<Vec<(usize, usize)>> = vec![Vec::new(); points.len()];
    for &(a, b) in edge_faces.keys() {
        point_edges[a].push((a, b));
        point_edges[b].push((a, b));
    }
    let mut next: Vec<DVec3> = (0..points.len())
        .map(|point| {
            let faces = &point_faces[point];
            let edges = &point_edges[point];
            let boundary: Vec<&(usize, usize)> = edges.iter().filter(|edge| edge_faces[*edge].len() == 1).collect();
            let original = points[point];
            if faces.is_empty() || faces.len() == 1 {
                return original;
            }
            match boundary.len() {
                0 => {
                    let valence = edges.len() as f64;
                    let face_average = faces.iter().map(|&face| face_points[face]).sum::<DVec3>() / faces.len() as f64;
                    let edge_average =
                        edges.iter().map(|&(a, b)| (points[a] + points[b]) * 0.5).sum::<DVec3>() / valence;
                    (face_average + edge_average * 2.0 + original * (valence - 3.0)) / valence
                }
                2 => {
                    let other = |&&(a, b): &&(usize, usize)| if a == point { points[b] } else { points[a] };
                    (other(&boundary[0]) + other(&boundary[1]) + original * 6.0) / 8.0
                }
                _ => original,
            }
        })
        .collect();
    let mut edge_points: HashMap<(usize, usize), usize> = HashMap::new();
    for (&(a, b), faces) in &edge_faces {
        let position = if faces.len() == 2 {
            (points[a] + points[b] + face_points[faces[0]] + face_points[faces[1]]) / 4.0
        } else {
            (points[a] + points[b]) * 0.5
        };
        next.push(position);
        edge_points.insert((a, b), next.len() - 1);
    }
    let face_start = next.len();
    next.extend_from_slice(&face_points);
    let mut face = 0;
    let grids = grids
        .iter()
        .map(|grid| {
            let size = grid.size * 2 - 1;
            let mut cells = vec![0; size * size];
            for row in 0..grid.size {
                for column in 0..grid.size {
                    cells[row * 2 * size + column * 2] = grid.at(row, column);
                    if column + 1 < grid.size {
                        let edge = edge_key(grid.at(row, column), grid.at(row, column + 1));
                        cells[row * 2 * size + column * 2 + 1] = edge_points[&edge];
                    }
                    if row + 1 < grid.size {
                        let edge = edge_key(grid.at(row, column), grid.at(row + 1, column));
                        cells[(row * 2 + 1) * size + column * 2] = edge_points[&edge];
                    }
                }
            }
            for row in 0..grid.size - 1 {
                for column in 0..grid.size - 1 {
                    cells[(row * 2 + 1) * size + column * 2 + 1] = face_start + face;
                    face += 1;
                }
            }
            Grid { size, cells }
        })
        .collect();
    (next, grids)
}

/// The normal of a grid's surface at each of its points, turned to face like `facing`.
fn grid_normals(grid: &Grid, points: &[DVec3], facing: DVec3) -> Vec<DVec3> {
    let size = grid.size;
    let mut normals = Vec::with_capacity(size * size);
    for row in 0..size {
        for column in 0..size {
            let point = |row: usize, column: usize| points[grid.at(row, column)];
            let along_rows = point((row + 1).min(size - 1), column) - point(row.saturating_sub(1), column);
            let along_columns = point(row, (column + 1).min(size - 1)) - point(row, column.saturating_sub(1));
            let normal = along_columns.cross(along_rows).normalize_or(facing);
            normals.push(if normal.dot(facing) < 0.0 { -normal } else { normal });
        }
    }
    normals
}

/// Hammer's Noise: every vertex moves along its face's normal by a smooth random amount
/// from `min` to `max`. The noise is read at each vertex's spot on its flat face, so
/// patches that meet move alike where they meet. `rockiness`, from 0 to 1, makes the
/// bumps smaller and rougher; `seed` picks the pattern. Returns the indices of the
/// patches that changed.
pub fn noise(patches: &mut [Patch], min: f64, max: f64, rockiness: f64, seed: u32) -> Vec<usize> {
    let rockiness = rockiness.clamp(0.0, 1.0);
    let field = Perlin::new(seed);
    let base = 1.0 / (32.0 + 480.0 * (1.0 - rockiness));
    let persistence = 0.3 + 0.4 * rockiness;
    let mut changed = Vec::new();
    for (index, patch) in patches.iter_mut().enumerate() {
        let surface = patch.surface.clone();
        for vertex in 0..surface.flat.len() {
            let mut total = 0.0;
            let mut weight = 1.0;
            let mut frequency = base;
            let mut weights = 0.0;
            for _ in 0..4 {
                total += field.sample(surface.flat[vertex] * frequency) * weight;
                weights += weight;
                weight *= persistence;
                frequency *= 2.0;
            }
            let unit = (total / weights * 0.5 + 0.5).clamp(0.0, 1.0);
            let lift = min + (max - min) * unit;
            let position = surface.positions[vertex] + surface.normal * lift;
            patch.displacement.move_vertex(&surface, vertex, position);
        }
        patch.refresh();
        changed.push(index);
    }
    changed
}

/// Ken Perlin's improved gradient noise, with a permutation picked by a seed.
struct Perlin {
    permutation: [u8; 512],
}

impl Perlin {
    fn new(seed: u32) -> Self {
        let mut order: Vec<u8> = (0..=255).collect();
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(0x9E37_79B9) | 1;
        for index in (1..order.len()).rev() {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            order.swap(index, state as usize % (index + 1));
        }
        let mut permutation = [0; 512];
        for (at, value) in permutation.iter_mut().enumerate() {
            *value = order[at % 256];
        }
        Self { permutation }
    }

    /// Noise from about -1 to 1 at a point.
    fn sample(&self, point: DVec3) -> f64 {
        let cell = point.floor();
        let local = point - cell;
        let [x, y, z] = cell.to_array().map(|value| (value as i64).rem_euclid(256) as usize);
        let fade = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
        let (u, v, w) = (fade(local.x), fade(local.y), fade(local.z));
        let p = &self.permutation;
        let hash = |x: usize, y: usize, z: usize| p[p[p[x] as usize + y] as usize + z];
        let gradient = |hash: u8, offset: DVec3| {
            let h = hash & 15;
            let a = if h < 8 { offset.x } else { offset.y };
            let b = if h < 4 {
                offset.y
            } else if h == 12 || h == 14 {
                offset.x
            } else {
                offset.z
            };
            (if h & 1 == 0 { a } else { -a }) + (if h & 2 == 0 { b } else { -b })
        };
        let corner = |dx: usize, dy: usize, dz: usize| {
            let offset = local - DVec3::new(dx as f64, dy as f64, dz as f64);
            gradient(hash(x + dx, y + dy, z + dz), offset)
        };
        let lerp = |t: f64, a: f64, b: f64| a + t * (b - a);
        lerp(
            w,
            lerp(
                v,
                lerp(u, corner(0, 0, 0), corner(1, 0, 0)),
                lerp(u, corner(0, 1, 0), corner(1, 1, 0)),
            ),
            lerp(
                v,
                lerp(u, corner(0, 0, 1), corner(1, 0, 1)),
                lerp(u, corner(0, 1, 1), corner(1, 1, 1)),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::solid_faces;
    use crate::Vmf;

    /// A power 2 displacement as Hammer saves it, on the top of a 512 unit floor brush.
    const FLOOR: &str = r#"world
{
	"id" "1"
	"classname" "worldspawn"
	solid
	{
		"id" "2"
		side
		{
			"id" "1"
			"plane" "(-256 256 16) (256 256 16) (256 -256 16)"
			"material" "NATURE/BLENDGRASSGRAVEL001A"
			"uaxis" "[1 0 0 0] 0.25"
			"vaxis" "[0 -1 0 0] 0.25"
			"rotation" "0"
			"lightmapscale" "16"
			"smoothing_groups" "0"
			dispinfo
			{
				"power" "2"
				"startposition" "[-256 -256 16]"
				"flags" "0"
				"elevation" "0"
				"subdiv" "0"
				normals
				{
					"row0" "0 0 1 0 0 1 0 0 1 0 0 1 0 0 1"
					"row1" "0 0 1 0 0 1 0 0 1 0 0 1 0 0 1"
					"row2" "0 0 1 0 0 1 0 0 1 0 0 1 0 0 1"
					"row3" "0 0 1 0 0 1 0 0 1 0 0 1 0 0 1"
					"row4" "0 0 1 0 0 1 0 0 1 0 0 1 0 0 1"
				}
				distances
				{
					"row0" "0 0 0 0 0"
					"row1" "0 0 0 0 0"
					"row2" "0 0 64 0 0"
					"row3" "0 0 0 0 0"
					"row4" "0 0 0 0 0"
				}
				offsets
				{
					"row0" "0 0 0 0 0 0 0 0 0 0 0 0 0 0 0"
					"row1" "0 0 0 0 0 0 0 0 0 0 0 0 0 0 0"
					"row2" "0 0 0 0 0 0 0 0 0 0 0 0 0 0 0"
					"row3" "0 0 0 0 0 0 0 0 0 0 0 0 0 0 0"
					"row4" "0 0 0 0 0 0 0 0 0 0 0 0 0 0 0"
				}
				offset_normals
				{
					"row0" "0 0 1 0 0 1 0 0 1 0 0 1 0 0 1"
					"row1" "0 0 1 0 0 1 0 0 1 0 0 1 0 0 1"
					"row2" "0 0 1 0 0 1 0 0 1 0 0 1 0 0 1"
					"row3" "0 0 1 0 0 1 0 0 1 0 0 1 0 0 1"
					"row4" "0 0 1 0 0 1 0 0 1 0 0 1 0 0 1"
				}
				alphas
				{
					"row0" "0 0 0 0 0"
					"row1" "0 0 0 0 0"
					"row2" "0 0 255 0 0"
					"row3" "0 0 0 0 0"
					"row4" "0 0 0 0 0"
				}
				triangle_tags
				{
					"row0" "9 9 9 9 9 9 9 9"
					"row1" "9 9 9 9 9 9 9 9"
					"row2" "9 9 9 9 9 9 9 9"
					"row3" "9 9 9 9 9 9 9 9"
				}
				allowed_verts
				{
					"10" "-1 -1 -1 -1 -1 -1 -1 -1 -1 -1"
				}
			}
		}
		side
		{
			"id" "2"
			"plane" "(-256 -256 0) (256 -256 0) (256 256 0)"
			"material" "TOOLS/TOOLSNODRAW"
			"uaxis" "[1 0 0 0] 0.25"
			"vaxis" "[0 -1 0 0] 0.25"
			"rotation" "0"
			"lightmapscale" "16"
			"smoothing_groups" "0"
		}
		side
		{
			"id" "3"
			"plane" "(-256 256 16) (-256 -256 16) (-256 -256 0)"
			"material" "TOOLS/TOOLSNODRAW"
			"uaxis" "[0 1 0 0] 0.25"
			"vaxis" "[0 0 -1 0] 0.25"
			"rotation" "0"
			"lightmapscale" "16"
			"smoothing_groups" "0"
		}
		side
		{
			"id" "4"
			"plane" "(256 256 0) (256 -256 0) (256 -256 16)"
			"material" "TOOLS/TOOLSNODRAW"
			"uaxis" "[0 1 0 0] 0.25"
			"vaxis" "[0 0 -1 0] 0.25"
			"rotation" "0"
			"lightmapscale" "16"
			"smoothing_groups" "0"
		}
		side
		{
			"id" "5"
			"plane" "(256 256 16) (-256 256 16) (-256 256 0)"
			"material" "TOOLS/TOOLSNODRAW"
			"uaxis" "[1 0 0 0] 0.25"
			"vaxis" "[0 0 -1 0] 0.25"
			"rotation" "0"
			"lightmapscale" "16"
			"smoothing_groups" "0"
		}
		side
		{
			"id" "6"
			"plane" "(256 -256 0) (-256 -256 0) (-256 -256 16)"
			"material" "TOOLS/TOOLSNODRAW"
			"uaxis" "[1 0 0 0] 0.25"
			"vaxis" "[0 0 -1 0] 0.25"
			"rotation" "0"
			"lightmapscale" "16"
			"smoothing_groups" "0"
		}
	}
}
"#;

    fn floor() -> Vmf {
        Vmf::parse(FLOOR).unwrap()
    }

    /// The displacement on the floor brush's top face, as a patch.
    fn floor_patch(vmf: &Vmf) -> Patch {
        let solid = vmf.world().unwrap().solids().next().unwrap();
        let face = solid_faces(solid).into_iter().find(|face| face.side == 0).unwrap();
        let side = solid.sides().next().unwrap();
        Patch::new(side, &face.vertices, face.plane.normal).unwrap()
    }

    /// A flat displacement of `power` on a square floor from `min` to `max` at height 0.
    fn flat_patch(side: u32, min: [f64; 2], max: [f64; 2], power: u32) -> Patch {
        let polygon = vec![
            DVec3::new(max[0], max[1], 0.0),
            DVec3::new(max[0], min[1], 0.0),
            DVec3::new(min[0], min[1], 0.0),
            DVec3::new(min[0], max[1], 0.0),
        ];
        let displacement = Displacement::create(&polygon, DVec3::Z, power).unwrap();
        let surface = Surface::new(&displacement, &polygon, DVec3::Z).unwrap();
        Patch {
            side,
            displacement,
            surface,
        }
    }

    #[test]
    fn reads_and_writes_a_hammer_block_unchanged() {
        let vmf = floor();
        let side = vmf.world().unwrap().solids().next().unwrap().sides().next().unwrap();
        let block = side.dispinfo().unwrap();
        let displacement = Displacement::read(block).unwrap();
        assert_eq!(displacement.power, 2);
        assert_eq!(displacement.start, DVec3::new(-256.0, -256.0, 16.0));
        assert_eq!(displacement.distances[12], 64.0);
        assert_eq!(displacement.alphas[12], 255.0);
        assert_eq!(displacement.triangle_tags, vec![9; 32]);
        assert_eq!(&displacement.to_block(), block);
    }

    #[test]
    fn keeps_unknown_keys_and_blocks_where_they_were() {
        let mut block = floor_patch(&floor()).displacement.to_block();
        block.items.insert(5, Item::Block(Block::new("multiblend")));
        block.items.push(Item::Pair(Pair::new("custom", "1")));
        let displacement = Displacement::read(&block).unwrap();
        assert_eq!(displacement.to_block(), block);
    }

    #[test]
    fn rows_start_at_the_start_position_and_run_along_the_winding() {
        let patch = floor_patch(&floor());
        let surface = &patch.surface;
        assert_eq!(surface.size, 5);
        assert_eq!(surface.flat[0], DVec3::new(-256.0, -256.0, 16.0));
        assert_eq!(surface.flat[surface.index(0, 4)], DVec3::new(256.0, -256.0, 16.0));
        assert_eq!(surface.flat[surface.index(4, 0)], DVec3::new(-256.0, 256.0, 16.0));
        assert_eq!(surface.flat[surface.index(1, 2)], DVec3::new(0.0, -128.0, 16.0));
        assert_eq!(surface.positions[surface.index(2, 2)], DVec3::new(0.0, 0.0, 80.0));
        assert_eq!(surface.alphas[12], 255.0);
    }

    #[test]
    fn another_start_position_turns_the_rows() {
        let vmf = floor();
        let solid = vmf.world().unwrap().solids().next().unwrap();
        let face = solid_faces(solid).into_iter().find(|face| face.side == 0).unwrap();
        let mut displacement = solid.sides().next().unwrap().displacement().unwrap();
        displacement.start = DVec3::new(250.0, 250.0, 16.0);
        let surface = Surface::new(&displacement, &face.vertices, face.plane.normal).unwrap();
        assert_eq!(surface.flat[0], DVec3::new(256.0, 256.0, 16.0));
        assert_eq!(surface.flat[surface.index(0, 4)], DVec3::new(-256.0, 256.0, 16.0));
        assert_eq!(surface.flat[surface.index(4, 0)], DVec3::new(256.0, -256.0, 16.0));
    }

    #[test]
    fn a_counter_clockwise_polygon_gives_the_same_grid() {
        let clockwise = flat_patch(1, [0.0, 0.0], [64.0, 64.0], 2);
        let mut polygon: Vec<DVec3> = vec![
            DVec3::new(64.0, 64.0, 0.0),
            DVec3::new(64.0, 0.0, 0.0),
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(0.0, 64.0, 0.0),
        ];
        polygon.reverse();
        let surface = Surface::new(&clockwise.displacement, &polygon, DVec3::Z).unwrap();
        assert_eq!(surface.flat, clockwise.surface.flat);
    }

    #[test]
    fn triangles_face_out_in_a_diamond_pattern() {
        let patch = flat_patch(1, [0.0, 0.0], [64.0, 64.0], 2);
        let surface = &patch.surface;
        let triangles = surface.triangles();
        assert_eq!(triangles.len(), 32);
        for [a, b, c] in &triangles {
            let normal =
                (surface.positions[*b] - surface.positions[*a]).cross(surface.positions[*c] - surface.positions[*a]);
            assert!(normal.z > 0.0);
        }
        assert_eq!(triangles[0], [0, 6, 5]);
        assert_eq!(triangles[2], [1, 2, 6]);
        assert_eq!(surface.edges().len(), 5 * 4 * 2 + 16);
        assert_eq!(surface.outline().len(), 16);
    }

    #[test]
    fn rays_hit_the_raised_surface() {
        let patch = floor_patch(&floor());
        let down = DVec3::NEG_Z;
        let hit = patch.surface.ray_hit(DVec3::new(0.0, 0.0, 500.0), down).unwrap();
        assert!((hit - 420.0).abs() < 1e-9);
        let side = patch.surface.ray_hit(DVec3::new(-200.0, -200.0, 500.0), down).unwrap();
        assert!((side - 484.0).abs() < 1e-9);
        assert!(patch.surface.ray_hit(DVec3::new(300.0, 0.0, 500.0), down).is_none());
    }

    #[test]
    fn new_displacements_are_flat_and_tagged_by_slope() {
        let floor = flat_patch(1, [0.0, 0.0], [64.0, 64.0], 3);
        assert_eq!(floor.displacement.normals.len(), 81);
        assert!(floor.surface.positions.iter().all(|position| position.z == 0.0));
        assert!(floor.displacement.triangle_tags.iter().all(|tag| *tag == 9));
        let wall = Displacement::new(2, DVec3::ZERO, DVec3::X);
        assert!(wall.triangle_tags.iter().all(|tag| *tag == 0));
        assert_eq!(Displacement::create(&[DVec3::ZERO; 3], DVec3::Z, 2), None);
    }

    #[test]
    fn steep_triangles_lose_their_walkable_tag() {
        let mut patch = flat_patch(1, [0.0, 0.0], [64.0, 64.0], 2);
        let top = patch.surface.index(2, 2);
        let raised = patch.surface.positions[top] + DVec3::Z * 200.0;
        patch.move_vertex(top, raised);
        patch.refresh();
        assert!(patch.displacement.triangle_tags.contains(&0));
        assert!(patch.displacement.triangle_tags.contains(&9));
    }

    #[test]
    fn soft_raise_is_strongest_in_the_middle() {
        let mut patches = vec![flat_patch(1, [-64.0, -64.0], [64.0, 64.0], 2)];
        let dab = Dab {
            center: DVec3::ZERO,
            radius: 80.0,
            falloff: Falloff::Soft,
            effect: Effect::Raise,
            channel: Channel::Geometry,
            amount: 10.0,
            axis: PaintAxis::FaceNormal,
        };
        assert_eq!(paint(&mut patches, &dab), vec![0]);
        let surface = &patches[0].surface;
        assert!((surface.positions[surface.index(2, 2)].z - 10.0).abs() < 1e-9);
        let near = surface.positions[surface.index(2, 3)].z;
        assert!(near > 0.0 && near < 10.0);
        assert_eq!(surface.positions[surface.index(0, 0)].z, 0.0);
    }

    #[test]
    fn hard_raise_to_sets_an_exact_height_and_lowering_goes_below() {
        let mut patches = vec![flat_patch(1, [-64.0, -64.0], [64.0, 64.0], 2)];
        let mut dab = Dab {
            center: DVec3::ZERO,
            radius: 40.0,
            falloff: Falloff::Hard,
            effect: Effect::RaiseTo,
            channel: Channel::Geometry,
            amount: 24.0,
            axis: PaintAxis::Direction(DVec3::Z),
        };
        paint(&mut patches, &dab);
        paint(&mut patches, &dab);
        let surface = &patches[0].surface;
        assert_eq!(surface.positions[surface.index(2, 2)].z, 24.0);
        assert_eq!(surface.positions[surface.index(2, 3)].z, 24.0);
        assert_eq!(surface.positions[surface.index(2, 4)].z, 0.0);
        dab.effect = Effect::Raise;
        dab.amount = -40.0;
        paint(&mut patches, &dab);
        let patch = &patches[0];
        let middle = patch.surface.index(2, 2);
        assert_eq!(patch.surface.positions[middle].z, -16.0);
        assert_eq!(patch.displacement.normals[middle], DVec3::NEG_Z);
        assert_eq!(patch.displacement.distances[middle], 16.0);
    }

    #[test]
    fn smoothing_flattens_a_spike() {
        let mut patches = vec![flat_patch(1, [-64.0, -64.0], [64.0, 64.0], 2)];
        let middle = patches[0].surface.index(2, 2);
        patches[0].move_vertex(middle, DVec3::new(0.0, 0.0, 80.0));
        patches[0].refresh();
        let dab = Dab {
            center: DVec3::new(0.0, 0.0, 80.0),
            radius: 20.0,
            falloff: Falloff::Hard,
            effect: Effect::Smooth,
            channel: Channel::Geometry,
            amount: 0.0,
            axis: PaintAxis::FaceNormal,
        };
        paint(&mut patches, &dab);
        assert!(patches[0].surface.positions[middle].z.abs() < 1e-9);
    }

    #[test]
    fn alpha_paints_and_inverts_within_its_range() {
        let mut patches = vec![flat_patch(1, [-64.0, -64.0], [64.0, 64.0], 2)];
        let dab = Dab {
            center: DVec3::ZERO,
            radius: 10.0,
            falloff: Falloff::Hard,
            effect: Effect::Raise,
            channel: Channel::Alpha,
            amount: 200.0,
            axis: PaintAxis::FaceNormal,
        };
        paint(&mut patches, &dab);
        paint(&mut patches, &dab);
        let middle = patches[0].surface.index(2, 2);
        assert_eq!(patches[0].displacement.alphas[middle], 255.0);
        assert_eq!(patches[0].surface.alphas[middle], 255.0);
        assert!(patches[0].surface.positions.iter().all(|position| position.z == 0.0));
        patches[0].displacement.invert_alpha();
        assert_eq!(patches[0].displacement.alphas[middle], 0.0);
        assert_eq!(patches[0].displacement.alphas[0], 255.0);
    }

    #[test]
    fn sewing_joins_the_shared_edge_of_two_patches() {
        let mut patches = vec![
            flat_patch(1, [0.0, 0.0], [64.0, 64.0], 2),
            flat_patch(2, [64.0, 0.0], [128.0, 64.0], 2),
        ];
        let raise = |patch: &mut Patch, lift: f64| {
            for vertex in 0..patch.surface.flat.len() {
                let position = patch.surface.flat[vertex] + DVec3::Z * lift;
                patch.move_vertex(vertex, position);
            }
            patch.refresh();
        };
        raise(&mut patches[0], 10.0);
        raise(&mut patches[1], 30.0);
        let changed = sew(&mut patches);
        assert_eq!(changed, vec![0, 1]);
        for patch in &patches {
            for vertex in 0..patch.surface.flat.len() {
                let flat = patch.surface.flat[vertex];
                if flat.x == 64.0 {
                    assert!((patch.surface.positions[vertex].z - 20.0).abs() < 1e-9);
                }
            }
        }
        assert_eq!(patches[0].surface.positions[0].z, 10.0);
        assert!(sew(&mut patches).is_empty());
    }

    #[test]
    fn sewing_a_finer_patch_follows_the_coarser_edge() {
        let mut patches = vec![
            flat_patch(1, [0.0, 0.0], [64.0, 64.0], 2),
            flat_patch(2, [64.0, 0.0], [128.0, 64.0], 3),
        ];
        for vertex in 0..patches[0].surface.flat.len() {
            let flat = patches[0].surface.flat[vertex];
            patches[0].move_vertex(vertex, flat + DVec3::Z * flat.y);
        }
        patches[0].refresh();
        sew(&mut patches);
        let fine = &patches[1];
        for vertex in 0..fine.surface.flat.len() {
            let flat = fine.surface.flat[vertex];
            if flat.x == 64.0 {
                assert!((fine.surface.positions[vertex].z - flat.y * 0.5).abs() < 1e-9, "{flat}");
            }
        }
    }

    #[test]
    fn subdividing_a_flat_face_changes_nothing_but_a_box_rounds() {
        let mut single = vec![flat_patch(1, [0.0, 0.0], [64.0, 64.0], 3)];
        let before = single[0].surface.positions.clone();
        subdivide(&mut single).unwrap();
        for (after, before) in single[0].surface.positions.iter().zip(before) {
            assert!(after.distance(before) < 1e-9);
        }
        assert!(single[0].displacement.subdivided);

        let mut ids = crate::IdAllocator::for_map(&Vmf::new_map());
        let cube =
            crate::build::box_solid(DVec3::splat(-64.0), DVec3::splat(64.0), "BRICK/BRICKWALL001A", &mut ids).unwrap();
        let mut patches: Vec<Patch> = solid_faces(&cube)
            .into_iter()
            .map(|face| {
                let displacement = Displacement::create(&face.vertices, face.plane.normal, 2).unwrap();
                let surface = Surface::new(&displacement, &face.vertices, face.plane.normal).unwrap();
                Patch {
                    side: face.side as u32,
                    displacement,
                    surface,
                }
            })
            .collect();
        assert_eq!(subdivide(&mut patches).unwrap().len(), 6);
        for patch in &patches {
            let corner = patch.surface.positions[0];
            assert!(corner.length() < 64.0 * 3f64.sqrt() - 1.0, "{corner}");
            let middle = patch.surface.positions[patch.surface.index(2, 2)];
            assert!(middle.length() < 64.0 + 1e-9, "{middle}");
        }
        let mut mixed = vec![
            flat_patch(1, [0.0, 0.0], [64.0, 64.0], 2),
            flat_patch(2, [64.0, 0.0], [128.0, 64.0], 3),
        ];
        assert!(subdivide(&mut mixed).is_err());
    }

    #[test]
    fn noise_stays_in_range_and_matches_where_patches_meet() {
        let mut patches = vec![
            flat_patch(1, [0.0, 0.0], [256.0, 256.0], 3),
            flat_patch(2, [256.0, 0.0], [512.0, 256.0], 3),
        ];
        noise(&mut patches, -8.0, 24.0, 0.5, 7);
        let mut heights = Vec::new();
        for patch in &patches {
            for vertex in 0..patch.surface.flat.len() {
                let z = patch.surface.positions[vertex].z;
                assert!((-8.0..=24.0).contains(&z));
                heights.push(z);
                if patch.surface.flat[vertex].x == 256.0 {
                    let flat = patch.surface.flat[vertex];
                    let other = &patches[if patch.side == 1 { 1 } else { 0 }];
                    let matching = (0..other.surface.flat.len())
                        .find(|at| other.surface.flat[*at] == flat)
                        .unwrap();
                    assert!((other.surface.positions[matching].z - z).abs() < 1e-9);
                }
            }
        }
        assert!(heights.iter().any(|z| (*z - heights[0]).abs() > 0.5));
    }

    #[test]
    fn a_side_takes_and_drops_its_displacement() {
        let mut vmf = Vmf::parse(FLOOR).unwrap();
        let world = vmf
            .items
            .iter_mut()
            .find_map(|item| match item {
                crate::TopLevel::World(world) => Some(world),
                _ => None,
            })
            .unwrap();
        let solid = world.solids_mut().next().unwrap();
        let bottom = solid.sides_mut().nth(1).unwrap();
        assert!(bottom.displacement().is_none());
        let displacement = Displacement::new(3, DVec3::new(-256.0, -256.0, 0.0), DVec3::NEG_Z);
        bottom.set_displacement(Some(&displacement));
        assert_eq!(bottom.displacement(), Some(displacement));
        let text = bottom.to_block();
        assert!(matches!(text.items.last(), Some(Item::Block(block)) if block.name == BLOCK));
        bottom.set_displacement(None);
        assert!(bottom.dispinfo().is_none());
        let top = solid.sides_mut().next().unwrap();
        top.set_displacement(None);
        assert!(!vmf.to_text().contains("dispinfo"));
    }
}
