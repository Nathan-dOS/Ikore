//! The canvas's water-bubble physics (feature 25, M1; D8). A spring brings
//! every node home; a dragged node follows the pointer on a stiffer one; the
//! links carry the motion to the neighbours; a ring ripples through the loose
//! nodes of the same box. Pure: time is a parameter and nothing here knows
//! GPUI, so the same inputs always give the same motion.

use std::collections::HashSet;
use std::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

use super::constants::{
    DRAG_K, DRAG_ZETA, HOME_K, HOME_ZETA, LIMIT, LIMIT_K, LINK_K, MAX_SUBSTEPS, MODE_GAIN, MODE_K,
    MODE_MAX, MODE_ZETA, REST_AMPLITUDE, REST_HOLD, REST_OFFSET, REST_SPEED, RING_DECAY,
    RING_IMPULSE, RING_INTERVAL, RING_SPEED, RING_WAVE_SPEED, STEP, STRETCH, STRETCH_SPEED,
};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Vec2 {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

impl Vec2 {
    pub(crate) const ZERO: Self = Self { x: 0.0, y: 0.0 };

    pub(crate) const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub(crate) fn length(self) -> f32 {
        self.x.hypot(self.y)
    }

    /// The unit vector, or zero for a zero vector.
    pub(crate) fn normalized(self) -> Self {
        let length = self.length();
        if length > f32::EPSILON {
            self * (1.0 / length)
        } else {
            Self::ZERO
        }
    }

    #[cfg(test)]
    pub(crate) fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

impl Add for Vec2 {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y)
    }
}

impl Sub for Vec2 {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y)
    }
}

impl Mul<f32> for Vec2 {
    type Output = Self;
    fn mul(self, factor: f32) -> Self {
        Self::new(self.x * factor, self.y * factor)
    }
}

impl Neg for Vec2 {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y)
    }
}

impl AddAssign for Vec2 {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

impl SubAssign for Vec2 {
    fn sub_assign(&mut self, other: Self) {
        *self = *self - other;
    }
}

/// One way a node's outline sways: an underdamped oscillator whose
/// amplitude scales `sin(mθ + phase)` around the outline.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Mode {
    pub(crate) amplitude: f32,
    velocity: f32,
    pub(crate) phase: f32,
}

/// Where a node rests and which box holds it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BodySpec {
    pub(crate) home: Vec2,
    pub(crate) group: usize,
}

#[derive(Debug, Clone, PartialEq)]
struct Body {
    home: Vec2,
    position: Vec2,
    velocity: Vec2,
    /// The box it belongs to: no force crosses boxes.
    group: usize,
    /// Linked to no node: the ring reaches it.
    loose: bool,
    modes: [Mode; 2],
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Drag {
    body: usize,
    /// From the pointer to the node's centre when the drag began.
    offset: Vec2,
    target: Vec2,
}

/// A ring's push on one loose node, travelling towards it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Impulse {
    at: f32,
    body: usize,
    delta: Vec2,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Physics {
    bodies: Vec<Body>,
    links: Vec<(usize, usize)>,
    drag: Option<Drag>,
    pending: Vec<Impulse>,
    forces: Vec<Vec2>,
    accumulator: f32,
    /// Simulated time.
    clock: f32,
    last_ring: Option<f32>,
    /// How long everything has been quiet.
    quiet: f32,
    at_rest: bool,
}

fn damping(stiffness: f32, zeta: f32) -> f32 {
    2.0 * zeta * stiffness.sqrt()
}

impl Physics {
    /// Every node at home and at rest.
    pub(crate) fn new(bodies: &[BodySpec], links: &[(usize, usize)]) -> Self {
        let links = links
            .iter()
            .copied()
            .filter(|(from, to)| from != to && *from < bodies.len() && *to < bodies.len())
            .collect::<Vec<_>>();
        let linked = links
            .iter()
            .flat_map(|(from, to)| [*from, *to])
            .collect::<HashSet<_>>();
        Self {
            bodies: bodies
                .iter()
                .enumerate()
                .map(|(index, spec)| Body {
                    home: spec.home,
                    position: spec.home,
                    velocity: Vec2::ZERO,
                    group: spec.group,
                    loose: !linked.contains(&index),
                    modes: [Mode::default(); 2],
                })
                .collect(),
            forces: vec![Vec2::ZERO; bodies.len()],
            links,
            drag: None,
            pending: Vec::new(),
            accumulator: 0.0,
            clock: 0.0,
            last_ring: None,
            quiet: 0.0,
            at_rest: true,
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.bodies.len()
    }

    /// Where the node's centre is now.
    pub(crate) fn position(&self, index: usize) -> Vec2 {
        self.bodies
            .get(index)
            .map_or(Vec2::ZERO, |body| body.position)
    }

    /// How far the node is from home.
    #[cfg(test)]
    pub(crate) fn offset(&self, index: usize) -> Vec2 {
        self.bodies
            .get(index)
            .map_or(Vec2::ZERO, |body| body.position - body.home)
    }

    pub(crate) fn velocity(&self, index: usize) -> Vec2 {
        self.bodies
            .get(index)
            .map_or(Vec2::ZERO, |body| body.velocity)
    }

    pub(crate) fn modes(&self, index: usize) -> [Mode; 2] {
        self.bodies
            .get(index)
            .map_or([Mode::default(); 2], |body| body.modes)
    }

    /// The stretch along the motion, `1/s` across it (the area stays), and
    /// the motion's direction.
    pub(crate) fn stretch(&self, index: usize) -> (f32, Vec2) {
        let velocity = self.velocity(index);
        let factor = 1.0 + STRETCH * (velocity.length() / STRETCH_SPEED).min(1.0);
        (factor, velocity.normalized())
    }

    /// Whether the node is away from its plain shape: dragged, off home,
    /// moving or swaying. A node at rest is drawn as a plain rectangle.
    pub(crate) fn is_moving(&self, index: usize) -> bool {
        if self.at_rest {
            return false;
        }
        if self.drag.is_some_and(|drag| drag.body == index) {
            return true;
        }
        self.bodies.get(index).is_some_and(|body| {
            (body.position - body.home).length() >= REST_OFFSET
                || body.velocity.length() >= REST_SPEED
                || body
                    .modes
                    .iter()
                    .any(|mode| mode.amplitude.abs() >= REST_AMPLITUDE)
        })
    }

    /// Nothing moves and nothing will until a drag: the canvas asks for no
    /// frames.
    pub(crate) fn is_at_rest(&self) -> bool {
        self.at_rest
    }

    pub(crate) fn dragging(&self) -> Option<usize> {
        self.drag.map(|drag| drag.body)
    }

    fn wake(&mut self) {
        if self.at_rest {
            self.at_rest = false;
            self.accumulator = 0.0;
        }
        self.quiet = 0.0;
    }

    /// Starts dragging `index` from `pointer`, in canvas pixels. Reduced
    /// motion (E6) never starts one.
    pub(crate) fn begin_drag(&mut self, index: usize, pointer: Vec2, reduced_motion: bool) -> bool {
        if reduced_motion || index >= self.bodies.len() {
            return false;
        }
        let offset = self.bodies[index].position - pointer;
        self.drag = Some(Drag {
            body: index,
            offset,
            target: pointer + offset,
        });
        self.wake();
        true
    }

    pub(crate) fn drag_to(&mut self, pointer: Vec2) {
        if let Some(drag) = self.drag.as_mut() {
            drag.target = pointer + drag.offset;
            self.wake();
        }
    }

    /// Lets the dragged node go: it heads home swaying, and its outline
    /// ripples as fast as it moved.
    pub(crate) fn end_drag(&mut self) {
        if let Some(drag) = self.drag.take()
            && let Some(body) = self.bodies.get_mut(drag.body)
        {
            let velocity = body.velocity;
            excite(body, velocity.length(), velocity.normalized());
            self.wake();
        }
    }

    /// Moves a node off home without dragging it, as a shake would.
    #[cfg(test)]
    pub(crate) fn displace(&mut self, index: usize, by: Vec2) {
        if let Some(body) = self.bodies.get_mut(index) {
            body.position += by;
            self.wake();
        }
    }

    /// Advances by `elapsed` seconds in fixed steps. A long frame takes at
    /// most `MAX_SUBSTEPS` steps and drops the rest, so a stall never makes
    /// the motion jump or explode.
    pub(crate) fn step(&mut self, elapsed: f32) {
        if self.at_rest || !elapsed.is_finite() || elapsed <= 0.0 {
            return;
        }
        self.accumulator += elapsed;
        let mut steps = 0;
        while self.accumulator >= STEP && steps < MAX_SUBSTEPS {
            self.substep();
            self.accumulator -= STEP;
            steps += 1;
            if self.at_rest {
                self.accumulator = 0.0;
                return;
            }
        }
        if self.accumulator >= STEP {
            self.accumulator = 0.0;
        }
    }

    fn substep(&mut self) {
        let step = STEP;
        self.clock += step;
        let clock = self.clock;
        let mut arrived = Vec::new();
        self.pending.retain(|impulse| {
            if impulse.at <= clock {
                arrived.push(*impulse);
                false
            } else {
                true
            }
        });
        for impulse in arrived {
            if let Some(body) = self.bodies.get_mut(impulse.body) {
                body.velocity += impulse.delta;
                excite(body, impulse.delta.length(), impulse.delta.normalized());
            }
        }

        let home_damping = damping(HOME_K, HOME_ZETA);
        let drag_damping = damping(DRAG_K, DRAG_ZETA);
        for (index, body) in self.bodies.iter().enumerate() {
            let offset = body.position - body.home;
            let mut force = match self.drag {
                Some(drag) if drag.body == index => {
                    (drag.target - body.position) * DRAG_K - body.velocity * drag_damping
                }
                _ => -offset * HOME_K - body.velocity * home_damping,
            };
            let distance = offset.length();
            if distance > LIMIT {
                force -= offset.normalized() * ((distance - LIMIT) * LIMIT_K);
            }
            self.forces[index] = force;
        }
        for &(from, to) in &self.links {
            let stretch = (self.bodies[from].position - self.bodies[from].home)
                - (self.bodies[to].position - self.bodies[to].home);
            self.forces[from] -= stretch * LINK_K;
            self.forces[to] += stretch * LINK_K;
        }
        for (body, force) in self.bodies.iter_mut().zip(&self.forces) {
            body.velocity += *force * step;
            body.position += body.velocity * step;
            for (index, mode) in body.modes.iter_mut().enumerate() {
                let stiffness = MODE_K[index];
                let acceleration =
                    -stiffness * mode.amplitude - damping(stiffness, MODE_ZETA) * mode.velocity;
                mode.velocity += acceleration * step;
                mode.amplitude += mode.velocity * step;
            }
        }

        if let Some(drag) = self.drag {
            let speed = self.bodies[drag.body].velocity.length();
            let due = self
                .last_ring
                .is_none_or(|last| clock - last >= RING_INTERVAL);
            if speed > RING_SPEED && due {
                self.ring(drag.body);
            }
        }

        let speeds: f32 = self.bodies.iter().map(|body| body.velocity.length()).sum();
        let offset = self
            .bodies
            .iter()
            .map(|body| (body.position - body.home).length())
            .fold(0.0, f32::max);
        let sway = self
            .bodies
            .iter()
            .flat_map(|body| body.modes.iter())
            .map(|mode| mode.amplitude.abs())
            .fold(0.0, f32::max);
        let quiet = self.drag.is_none()
            && self.pending.is_empty()
            && speeds < REST_SPEED
            && offset < REST_OFFSET
            && sway < REST_AMPLITUDE;
        if !quiet {
            self.quiet = 0.0;
            return;
        }
        self.quiet += step;
        if self.quiet >= REST_HOLD {
            for body in &mut self.bodies {
                body.position = body.home;
                body.velocity = Vec2::ZERO;
                body.modes = [Mode::default(); 2];
            }
            self.at_rest = true;
            self.quiet = 0.0;
        }
    }

    /// A ring from the dragged node through the loose nodes of its box: each
    /// gets pushed away, later and weaker the farther it is.
    fn ring(&mut self, origin: usize) {
        let center = self.bodies[origin].position;
        let group = self.bodies[origin].group;
        for (index, body) in self.bodies.iter().enumerate() {
            if index == origin || !body.loose || body.group != group {
                continue;
            }
            let away = body.position - center;
            let distance = away.length();
            self.pending.push(Impulse {
                at: self.clock + distance / RING_WAVE_SPEED,
                body: index,
                delta: away.normalized() * (RING_IMPULSE * (-distance / RING_DECAY).exp()),
            });
        }
        self.last_ring = Some(self.clock);
    }

    /// The motion's energy: speeds, springs, links and sway. Damping only
    /// takes it away once nothing pushes.
    #[cfg(test)]
    pub(crate) fn energy(&self) -> f32 {
        let mut energy = 0.0;
        for body in &self.bodies {
            let offset = body.position - body.home;
            let beyond = (offset.length() - LIMIT).max(0.0);
            energy += 0.5 * body.velocity.length().powi(2)
                + 0.5 * HOME_K * offset.length().powi(2)
                + 0.5 * LIMIT_K * beyond * beyond;
            for (index, mode) in body.modes.iter().enumerate() {
                energy +=
                    0.5 * mode.velocity.powi(2) + 0.5 * MODE_K[index] * mode.amplitude.powi(2);
            }
        }
        for &(from, to) in &self.links {
            let stretch = (self.bodies[from].position - self.bodies[from].home)
                - (self.bodies[to].position - self.bodies[to].home);
            energy += 0.5 * LINK_K * stretch.length().powi(2);
        }
        energy
    }

    /// Whether a ring is still on its way.
    #[cfg(test)]
    pub(crate) fn ring_in_flight(&self) -> bool {
        !self.pending.is_empty()
    }
}

/// Sets the outline swaying, in proportion to `speed`, aligned with the
/// motion.
fn excite(body: &mut Body, speed: f32, direction: Vec2) {
    let amplitude = MODE_GAIN * (speed / STRETCH_SPEED).min(1.0);
    if amplitude <= 0.0 {
        return;
    }
    let angle = direction.y.atan2(direction.x);
    for (index, mode) in body.modes.iter_mut().enumerate() {
        let order = (index + 2) as f32;
        mode.amplitude = (mode.amplitude + amplitude).clamp(-MODE_MAX, MODE_MAX);
        mode.phase = -order * angle;
    }
}
