//! Hammer's Sculpt tool. Its brush is a circle on the screen rather than a sphere in
//! the world: Push moves the vertices inside it along a direction, strongest out to
//! the falloff position and easing to the final falloff at the edge; Carve moves them
//! by a profile across the stroke, as for a road or a ditch; Smooth evens them out;
//! and Blend paints the texture weights of four-texture blend materials.

use std::collections::HashMap;

use glam::{DVec2, DVec3};

use super::{neighbour_averages, FourWayBlend, Patch};

/// Carve's profile until another is drawn: heights at even steps across the stroke,
/// from its left edge to its right, making a trench with a flat bottom.
pub const CARVE_PROFILE: [f64; 9] = [0.0, 0.45, 0.85, 1.0, 1.0, 1.0, 0.85, 0.45, 0.0];

/// What a step of the sculpt brush does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SculptMode {
    /// Hammer's Push: moves vertices along the direction.
    #[default]
    Push,
    /// Hammer's Carve: moves vertices by the profile across the stroke.
    Carve,
    /// Shift with the brush: moves vertices towards the average of their neighbours.
    Smooth,
    /// Hammer's Blend: paints a four-texture material's texture weights.
    Blend,
}

/// One step of the sculpt brush.
#[derive(Clone, Debug, PartialEq)]
pub struct SculptBrush {
    pub mode: SculptMode,
    /// Where the brush is on the screen.
    pub center: DVec2,
    /// Where it was at the step before, which Carve cuts along to `center`.
    pub previous: DVec2,
    /// The brush's radius on the screen, in the same units as `center`.
    pub radius: f64,
    /// The direction Push, Carve and Smooth move vertices in; Smooth moves them freely
    /// when it is zero.
    pub direction: DVec3,
    /// How far a vertex at full strength moves in one step, in units; for Blend, how
    /// much of the texture it gains, from 0 to 1.
    pub amount: f64,
    /// The share of the radius, from the middle, at full strength: Hammer's Falloff
    /// Position.
    pub falloff_start: f64,
    /// The strength at the edge, as a share of full strength: Hammer's Falloff Final.
    pub falloff_end: f64,
    /// How much moved vertices are evened out with their neighbours, from 0 to 1.
    pub smoothing: f64,
    /// Hammer's Attenuated density: in one stroke a vertex moves at most as far as one
    /// step at the strongest the brush reached it, however often the brush passes. Off,
    /// every step adds up, as Hammer's Additive does.
    pub attenuated: bool,
    /// Carve's profile across the stroke, as [`CARVE_PROFILE`].
    pub profile: Vec<f64>,
    /// The texture Blend paints, from 0 to 3.
    pub texture: usize,
    /// Blend takes the texture away instead of adding it.
    pub erase: bool,
}

impl SculptBrush {
    /// How strongly the brush acts on a vertex at this point of the screen: nothing
    /// outside it.
    pub fn strength(&self, screen: DVec2) -> f64 {
        if self.radius <= 0.0 {
            return 0.0;
        }
        if self.mode == SculptMode::Carve {
            return self.carve_strength(screen);
        }
        let distance = screen.distance(self.center) / self.radius;
        if distance > 1.0 {
            return 0.0;
        }
        self.falloff(distance)
    }

    /// Full strength out to `falloff_start` of the radius, then falling in a straight
    /// line to `falloff_end` at the edge.
    fn falloff(&self, distance: f64) -> f64 {
        let start = self.falloff_start.clamp(0.0, 1.0);
        if distance <= start || start >= 1.0 {
            return 1.0;
        }
        let along = (distance - start) / (1.0 - start);
        1.0 + (self.falloff_end.clamp(0.0, 1.0) - 1.0) * along
    }

    /// The profile at where a point lies across the stroke from `previous` to `center`:
    /// -1 at the stroke's left edge, 0 on it and 1 at its right edge, the stroke's ends
    /// rounded. Nothing while the brush has not moved, as a carve needs a direction.
    fn carve_strength(&self, screen: DVec2) -> f64 {
        let stroke = self.center - self.previous;
        let length = stroke.length();
        if length < 1e-6 {
            return 0.0;
        }
        let along = ((screen - self.previous).dot(stroke) / (length * length)).clamp(0.0, 1.0);
        let distance = screen.distance(self.previous + stroke * along) / self.radius;
        if distance > 1.0 {
            return 0.0;
        }
        let across = if stroke.perp_dot(screen - self.previous) < 0.0 {
            -distance
        } else {
            distance
        };
        profile_at(&self.profile, across)
    }
}

/// A profile of heights at even steps from -1 to 1, read at `across` between them.
fn profile_at(profile: &[f64], across: f64) -> f64 {
    match profile {
        [] => 1.0,
        [only] => *only,
        _ => {
            let place = (across.clamp(-1.0, 1.0) + 1.0) * 0.5 * (profile.len() - 1) as f64;
            let low = (place.floor() as usize).min(profile.len() - 2);
            let blend = place - low as f64;
            profile[low] + (profile[low + 1] - profile[low]) * blend
        }
    }
}

/// What one stroke remembers between its steps: how far Attenuated has already moved
/// each vertex, by side id.
#[derive(Debug, Clone, Default)]
pub struct StrokeMemory {
    moved: HashMap<u32, Vec<f64>>,
}

/// Applies one step of the sculpt brush to every patch and returns the indices of the
/// patches it changed. `project` puts a world point on the screen, or gives `None` for
/// points behind the camera, which the brush never reaches.
pub fn sculpt(
    patches: &mut [Patch],
    brush: &SculptBrush,
    project: &dyn Fn(DVec3) -> Option<DVec2>,
    memory: &mut StrokeMemory,
) -> Vec<usize> {
    let direction = brush.direction.normalize_or_zero();
    let mut changed = Vec::new();
    for (index, patch) in patches.iter_mut().enumerate() {
        let strengths: Vec<(usize, f64)> = patch
            .surface
            .positions
            .iter()
            .enumerate()
            .filter_map(|(vertex, position)| {
                let strength = brush.strength(project(*position)?);
                (strength > 0.0).then_some((vertex, strength))
            })
            .collect();
        if strengths.is_empty() {
            continue;
        }
        let moved = match brush.mode {
            SculptMode::Blend => blend(patch, brush, &strengths),
            SculptMode::Smooth => smooth(patch, direction, &strengths),
            SculptMode::Push | SculptMode::Carve if direction != DVec3::ZERO => {
                let count = patch.surface.positions.len();
                let applied = memory.moved.entry(patch.side).or_insert_with(|| vec![0.0; count]);
                applied.resize(count, 0.0);
                push(patch, brush, direction, &strengths, applied)
            }
            SculptMode::Push | SculptMode::Carve => false,
        };
        if moved {
            patch.refresh();
            changed.push(index);
        }
    }
    changed
}

/// Push and Carve: each vertex moves along `direction` by the amount times its
/// strength, held back by Attenuated, then evened out by the smoothing.
fn push(
    patch: &mut Patch,
    brush: &SculptBrush,
    direction: DVec3,
    strengths: &[(usize, f64)],
    applied: &mut [f64],
) -> bool {
    let surface = patch.surface.clone();
    let averages = (brush.smoothing > 0.0).then(|| neighbour_averages(surface.size, &surface.positions));
    let sign = if brush.amount < 0.0 { -1.0 } else { 1.0 };
    let mut moved = false;
    for &(vertex, strength) in strengths {
        let wanted = brush.amount.abs() * strength;
        let shift = if brush.attenuated {
            let shift = (wanted - applied[vertex]).max(0.0);
            applied[vertex] += shift;
            shift
        } else {
            wanted
        };
        let mut position = surface.positions[vertex] + direction * shift * sign;
        if let Some(averages) = &averages {
            position +=
                direction * (averages[vertex] - position).dot(direction) * brush.smoothing.clamp(0.0, 1.0) * strength;
        }
        if position.distance_squared(surface.positions[vertex]) > 1e-18 {
            patch.displacement.move_vertex(&surface, vertex, position);
            moved = true;
        }
    }
    moved
}

/// Moves each vertex towards the average of its neighbours by its strength, along
/// `direction` unless it is zero.
fn smooth(patch: &mut Patch, direction: DVec3, strengths: &[(usize, f64)]) -> bool {
    let surface = patch.surface.clone();
    let averages = neighbour_averages(surface.size, &surface.positions);
    let mut moved = false;
    for &(vertex, strength) in strengths {
        let position = surface.positions[vertex];
        let towards = averages[vertex] - position;
        let towards = if direction == DVec3::ZERO {
            towards
        } else {
            direction * towards.dot(direction)
        };
        if towards.length_squared() > 1e-18 {
            patch
                .displacement
                .move_vertex(&surface, vertex, position + towards * strength);
            moved = true;
        }
    }
    moved
}

/// Adds the brush's texture to each vertex's weights by its strength, or takes it away,
/// keeping the weights adding up to 1. A displacement without weights starts as Hammer
/// starts them, all first texture.
fn blend(patch: &mut Patch, brush: &SculptBrush, strengths: &[(usize, f64)]) -> bool {
    let count = patch.surface.positions.len();
    let texture = brush.texture.min(3);
    let mix = patch
        .displacement
        .multiblend
        .get_or_insert_with(|| FourWayBlend::new(count));
    mix.weights.resize(count, [1.0, 0.0, 0.0, 0.0]);
    let mut moved = false;
    for &(vertex, strength) in strengths {
        let weights = &mut mix.weights[vertex];
        let before = *weights;
        let step = brush.amount.abs() * strength;
        if brush.erase {
            weights[texture] = (weights[texture] - step).max(0.0);
        } else {
            weights[texture] += step;
        }
        normalise(weights);
        moved |= *weights != before;
    }
    moved
}

/// Scales weights to add up to 1; weights that add up to nothing become all first
/// texture.
fn normalise(weights: &mut [f64; 4]) {
    for weight in weights.iter_mut() {
        *weight = weight.max(0.0);
    }
    let total: f64 = weights.iter().sum();
    if total <= 1e-9 {
        *weights = [1.0, 0.0, 0.0, 0.0];
    } else {
        for weight in weights.iter_mut() {
            *weight /= total;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::displacement::{Displacement, Surface};

    /// A flat power 2 displacement over the square from `min` to `max` at height 0.
    fn flat(side: u32, min: [f64; 2], max: [f64; 2]) -> Patch {
        let polygon = vec![
            DVec3::new(max[0], max[1], 0.0),
            DVec3::new(max[0], min[1], 0.0),
            DVec3::new(min[0], min[1], 0.0),
            DVec3::new(min[0], max[1], 0.0),
        ];
        let displacement = Displacement::create(&polygon, DVec3::Z, 2).unwrap();
        let surface = Surface::new(&displacement, &polygon, DVec3::Z).unwrap();
        Patch {
            side,
            displacement,
            surface,
        }
    }

    /// A camera looking straight down: one screen unit per world unit.
    fn top_down(point: DVec3) -> Option<DVec2> {
        Some(DVec2::new(point.x, -point.y))
    }

    fn brush(mode: SculptMode) -> SculptBrush {
        SculptBrush {
            mode,
            center: DVec2::ZERO,
            previous: DVec2::ZERO,
            radius: 40.0,
            direction: DVec3::Z,
            amount: 10.0,
            falloff_start: 0.5,
            falloff_end: 0.25,
            smoothing: 0.0,
            attenuated: false,
            profile: CARVE_PROFILE.to_vec(),
            texture: 1,
            erase: false,
        }
    }

    #[test]
    fn push_falls_off_from_the_falloff_position_to_the_final_strength() {
        let pushing = brush(SculptMode::Push);
        assert_eq!(pushing.strength(DVec2::new(10.0, 0.0)), 1.0);
        assert_eq!(pushing.strength(DVec2::new(20.0, 0.0)), 1.0);
        assert!((pushing.strength(DVec2::new(30.0, 0.0)) - 0.625).abs() < 1e-12);
        assert!((pushing.strength(DVec2::new(40.0, 0.0)) - 0.25).abs() < 1e-12);
        assert_eq!(pushing.strength(DVec2::new(41.0, 0.0)), 0.0);
    }

    #[test]
    fn push_moves_what_is_inside_the_circle_on_screen() {
        let mut patches = vec![flat(1, [-64.0, -64.0], [64.0, 64.0])];
        let mut memory = StrokeMemory::default();
        assert_eq!(
            sculpt(&mut patches, &brush(SculptMode::Push), &top_down, &mut memory),
            vec![0]
        );
        let surface = &patches[0].surface;
        assert_eq!(surface.positions[surface.index(2, 2)].z, 10.0);
        let near = surface.positions[surface.index(2, 3)].z;
        assert!((near - 5.5).abs() < 1e-9, "{near}");
        assert_eq!(surface.positions[surface.index(0, 0)].z, 0.0);
        let behind = |_: DVec3| None;
        assert!(sculpt(&mut patches, &brush(SculptMode::Push), &behind, &mut memory).is_empty());
    }

    #[test]
    fn attenuated_strokes_move_each_vertex_once() {
        let mut patches = vec![flat(1, [-64.0, -64.0], [64.0, 64.0])];
        let mut attenuated = brush(SculptMode::Push);
        attenuated.attenuated = true;
        let mut memory = StrokeMemory::default();
        for _ in 0..3 {
            sculpt(&mut patches, &attenuated, &top_down, &mut memory);
        }
        let middle = patches[0].surface.index(2, 2);
        assert_eq!(patches[0].surface.positions[middle].z, 10.0);
        let mut additive = attenuated.clone();
        additive.attenuated = false;
        sculpt(&mut patches, &additive, &top_down, &mut StrokeMemory::default());
        assert_eq!(patches[0].surface.positions[middle].z, 20.0);
    }

    #[test]
    fn carve_follows_the_profile_across_the_stroke() {
        let mut patches = vec![flat(1, [-64.0, -64.0], [64.0, 64.0])];
        let mut carving = brush(SculptMode::Carve);
        carving.direction = DVec3::NEG_Z;
        let mut memory = StrokeMemory::default();
        assert!(sculpt(&mut patches, &carving, &top_down, &mut memory).is_empty());
        carving.previous = DVec2::new(-64.0, 0.0);
        carving.center = DVec2::new(64.0, 0.0);
        sculpt(&mut patches, &carving, &top_down, &mut memory);
        let surface = &patches[0].surface;
        for column in 0..5 {
            assert_eq!(surface.positions[surface.index(2, column)].z, -10.0);
        }
        let side = surface.positions[surface.index(1, 2)].z;
        let expected = -10.0 * profile_at(&CARVE_PROFILE, -0.8);
        assert!((side - expected).abs() < 1e-9, "{side} {expected}");
        assert_eq!(surface.positions[surface.index(4, 2)].z, 0.0);
    }

    #[test]
    fn profiles_read_between_their_steps() {
        assert_eq!(profile_at(&[0.0, 1.0, 0.0], 0.0), 1.0);
        assert_eq!(profile_at(&[0.0, 1.0, 0.0], -0.5), 0.5);
        assert_eq!(profile_at(&[0.0, 1.0, 0.0], 1.0), 0.0);
        assert_eq!(profile_at(&[], 0.3), 1.0);
    }

    #[test]
    fn smoothing_evens_out_a_spike() {
        let mut patches = vec![flat(1, [-64.0, -64.0], [64.0, 64.0])];
        let middle = patches[0].surface.index(2, 2);
        patches[0].move_vertex(middle, DVec3::new(0.0, 0.0, 40.0));
        patches[0].refresh();
        let mut smoothing = brush(SculptMode::Smooth);
        smoothing.radius = 10.0;
        sculpt(&mut patches, &smoothing, &top_down, &mut StrokeMemory::default());
        assert!(patches[0].surface.positions[middle].z.abs() < 1e-9);
    }

    #[test]
    fn blend_paints_weights_that_add_up_to_one() {
        let mut patches = vec![flat(1, [-64.0, -64.0], [64.0, 64.0])];
        assert!(patches[0].displacement.multiblend.is_none());
        let mut painting = brush(SculptMode::Blend);
        painting.amount = 1.0;
        painting.radius = 10.0;
        sculpt(&mut patches, &painting, &top_down, &mut StrokeMemory::default());
        let middle = patches[0].surface.index(2, 2);
        let blend = patches[0].displacement.multiblend.as_ref().unwrap();
        assert_eq!(blend.weights[middle], [0.5, 0.5, 0.0, 0.0]);
        assert_eq!(blend.weights[0], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(
            patches[0].surface.weights.as_ref().unwrap()[middle],
            [0.5, 0.5, 0.0, 0.0]
        );
        painting.erase = true;
        sculpt(&mut patches, &painting, &top_down, &mut StrokeMemory::default());
        let blend = patches[0].displacement.multiblend.as_ref().unwrap();
        assert_eq!(blend.weights[middle], [1.0, 0.0, 0.0, 0.0]);
        let written = patches[0].displacement.to_block();
        let read = Displacement::read(&written).unwrap();
        assert_eq!(read.multiblend, patches[0].displacement.multiblend);
        assert!(written.blocks_named("multiblend_color_3").next().is_some());
    }
}
