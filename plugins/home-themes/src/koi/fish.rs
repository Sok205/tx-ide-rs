//! A koi: a spine that trails its head, steered by a wander, its neighbours, the pond's edges,
//! food and the pointer, and drawn as a tapered body with fins and markings.

use std::f32::consts::{PI, TAU};

use fastrand::Rng;
use ratatui::style::Color;
use tx_home::backdrop::{Pixels, dim};

pub const SEGMENTS: usize = 14;

/// The turn rate is capped at `TURNING * speed / length`, so the turn radius is `length / TURNING`.
const TURNING: f32 = 3.2;
/// Turn rate, in rad/s, per radian between the heading and the steering target.
const STEERING_GAIN: f32 = 1.1;
const MAX_BEND: f32 = 0.3;
const WANDER_DRIFT: f32 = 0.6;
const WANDER_LIMIT: f32 = 0.45;
const SEPARATION_WEIGHT: f32 = 1.0;
const ALIGNMENT_WEIGHT: f32 = 0.2;
const EDGE_WEIGHT: f32 = 20.0;
const EDGE_MARGIN: f32 = 4.0;
const FOOD_RANGE: f32 = 25.0;
const FOOD_WEIGHT: f32 = 3.0;
/// How much faster and tighter a fish turns while it goes for food or circles the pointer.
const FOCUSED_AGILITY: f32 = 3.0;
const CURIOSITY_RANGE: f32 = 20.0;
/// How far from the pointer a curious fish circles it.
const CURIOSITY_RADIUS: f32 = 6.0;
const CURIOSITY_WEIGHT: f32 = 1.5;
const BURST_FACTOR: f32 = 2.0;
const TAIL_BEATS_PER_SECOND: f32 = 0.8;

const WHITE: Color = Color::Rgb(244, 238, 226);
const RED: Color = Color::Rgb(222, 70, 34);
const BLACK: Color = Color::Rgb(24, 22, 28);
const FIN: Color = Color::Rgb(168, 178, 210);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variety {
    Kohaku,
    Sanke,
    Tancho,
}

/// A disc in body coordinates: `along` the spine (0 head, 1 tail) and `across` it, as a fraction
/// of the half-width there.
#[derive(Clone, Copy, Debug)]
struct Marking {
    along: f32,
    across: f32,
    radius: f32,
    color: Color,
}

/// What draws a fish's attention, wherever it is: it only reacts within its own ranges.
#[derive(Clone, Copy, Debug, Default)]
pub struct Attractions {
    /// The nearest pellet.
    pub food: Option<(f32, f32)>,
    pub pointer: Option<(f32, f32)>,
}

#[derive(Clone, Debug)]
pub struct Fish {
    spine: [(f32, f32); SEGMENTS],
    heading: f32,
    speed: f32,
    pub(super) bursting: bool,
    /// Seconds left in the current burst or glide.
    timer: f32,
    wander: f32,
    markings: Vec<Marking>,
    length: f32,
    pub(super) depth: f32,
    /// Offsets the tail wave so the fish don't beat in step.
    beat: f32,
}

pub fn distance(from: (f32, f32), to: (f32, f32)) -> f32 {
    (to.0 - from.0).hypot(to.1 - from.1)
}

/// Straight at the food.
fn hunger(head: (f32, f32), food: (f32, f32)) -> (f32, f32) {
    let reach = FOOD_WEIGHT / distance(head, food).max(0.1);
    ((food.0 - head.0) * reach, (food.1 - head.1) * reach)
}

/// In towards the pointer, or out from it, to circle it at `CURIOSITY_RADIUS` on whichever side
/// it already is.
fn curiosity(head: (f32, f32), heading: f32, pointer: (f32, f32)) -> (f32, f32) {
    let (offset_x, offset_y) = (pointer.0 - head.0, pointer.1 - head.1);
    let distance = offset_x.hypot(offset_y).max(0.1);
    let (radial_x, radial_y) = (offset_x / distance, offset_y / distance);
    let side = (heading.cos() * offset_y - heading.sin() * offset_x).signum();
    let inward = ((distance - CURIOSITY_RADIUS) / CURIOSITY_RADIUS).clamp(-1.0, 1.0);
    (
        CURIOSITY_WEIGHT * (radial_x * inward + side * radial_y),
        CURIOSITY_WEIGHT * (radial_y * inward - side * radial_x),
    )
}

fn wrap(angle: f32) -> f32 {
    (angle + PI).rem_euclid(TAU) - PI
}

/// (fraction along `from`→`to`, squared distance, side: +1 left of the segment, -1 right).
fn project(from: (f32, f32), to: (f32, f32), point: (f32, f32)) -> (f32, f32, f32) {
    let (segment_x, segment_y) = (to.0 - from.0, to.1 - from.1);
    let (offset_x, offset_y) = (point.0 - from.0, point.1 - from.1);
    let fraction = ((offset_x * segment_x + offset_y * segment_y)
        / (segment_x * segment_x + segment_y * segment_y))
        .clamp(0.0, 1.0);
    let (away_x, away_y) = (
        offset_x - fraction * segment_x,
        offset_y - fraction * segment_y,
    );
    let side = (segment_x * offset_y - segment_y * offset_x).signum();
    (fraction, away_x * away_x + away_y * away_y, side)
}

/// Pull back from an edge within `EDGE_MARGIN`, growing past it: positive near 0, negative near
/// `size`.
fn edge(position: f32, size: f32) -> f32 {
    ((EDGE_MARGIN - position).max(0.0) - (position - size + EDGE_MARGIN).max(0.0)) / EDGE_MARGIN
}

/// Half-width as a fraction of the widest: a blunt round head, widest at 30%, tapering to the tail.
fn profile(along: f32) -> f32 {
    const WIDEST: f32 = 0.3;
    if along < WIDEST {
        (1.0 - ((WIDEST - along) / (WIDEST + 0.04)).powi(2)).sqrt()
    } else {
        1.0 - 0.65 * ((along - WIDEST) / (1.0 - WIDEST))
    }
}

/// A line from `from` to `to`, `half_width` pixels either side.
fn stroke(pixels: &mut Pixels, from: (f32, f32), to: (f32, f32), half_width: f32, color: Color) {
    let reach = half_width.ceil() as i32 + 1;
    for y in from.1.min(to.1) as i32 - reach..=from.1.max(to.1) as i32 + reach {
        for x in from.0.min(to.0) as i32 - reach..=from.0.max(to.0) as i32 + reach {
            let (_, distance_squared, _) = project(from, to, (x as f32 + 0.5, y as f32 + 0.5));
            if distance_squared <= half_width * half_width {
                pixels.set(x, y, color);
            }
        }
    }
}

fn markings(variety: Variety, length: f32, random: &mut Rng) -> Vec<Marking> {
    let patches = |random: &mut Rng| -> Vec<Marking> {
        let count = random.usize(2..=3);
        (0..count)
            .map(|index| Marking {
                along: 0.08 + 0.55 * (index as f32 + random.f32()) / count as f32,
                across: random.f32() * 1.4 - 0.7,
                radius: length * (0.09 + random.f32() * 0.06),
                color: RED,
            })
            .collect()
    };
    match variety {
        Variety::Kohaku => patches(random),
        Variety::Sanke => {
            let mut markings = patches(random);
            let spots = random.usize(2..=4);
            markings.extend((0..spots).map(|_| Marking {
                along: 0.2 + random.f32() * 0.55,
                across: random.f32() * 1.6 - 0.8,
                radius: length * (0.035 + random.f32() * 0.03),
                color: BLACK,
            }));
            markings
        }
        Variety::Tancho => vec![Marking {
            along: 0.12,
            across: 0.0,
            radius: length * 0.075,
            color: RED,
        }],
    }
}

impl Fish {
    pub fn spawn(random: &mut Rng, bounds: (f32, f32)) -> Self {
        let length = 12.0 + random.f32() * 13.0;
        let heading = random.f32() * TAU;
        let head = (random.f32() * bounds.0, random.f32() * bounds.1);
        let gap = length / SEGMENTS as f32;
        let variety = [Variety::Kohaku, Variety::Sanke, Variety::Tancho][random.usize(..3)];
        let mut fish = Self {
            spine: std::array::from_fn(|index| {
                let behind = gap * index as f32;
                (
                    head.0 - heading.cos() * behind,
                    head.1 - heading.sin() * behind,
                )
            }),
            heading,
            speed: 0.0,
            bursting: false,
            timer: random.f32() * 3.0,
            wander: 0.0,
            markings: markings(variety, length, random),
            length,
            depth: 0.5 + random.f32() * 0.5,
            beat: random.f32() * TAU,
        };
        fish.speed = fish.cruise();
        fish
    }

    pub fn head(&self) -> (f32, f32) {
        self.spine[0]
    }

    fn cruise(&self) -> f32 {
        2.0 + self.length * 0.1
    }

    /// `neighbours` may include this fish; it is skipped.
    pub fn swim(
        &mut self,
        seconds: f32,
        neighbours: &[Fish],
        bounds: (f32, f32),
        attractions: Attractions,
        random: &mut Rng,
    ) {
        let head = self.spine[0];
        let noticed = Attractions {
            food: attractions
                .food
                .filter(|&food| distance(head, food) <= FOOD_RANGE),
            pointer: attractions
                .pointer
                .filter(|&pointer| distance(head, pointer) <= CURIOSITY_RANGE),
        };
        self.pace(seconds, noticed.food.is_some(), random);
        self.wander = (self.wander + (random.f32() * 2.0 - 1.0) * WANDER_DRIFT * seconds.sqrt())
            .clamp(-WANDER_LIMIT, WANDER_LIMIT);
        let agility = if noticed.food.is_some() || noticed.pointer.is_some() {
            FOCUSED_AGILITY
        } else {
            1.0
        };
        self.turn_towards(self.steering(neighbours, bounds, noticed), agility, seconds);
        let (x, y) = self.spine[0];
        let travel = self.speed * seconds;
        self.spine[0] = (
            x + self.heading.cos() * travel,
            y + self.heading.sin() * travel,
        );
        self.follow();
    }

    /// Alternates bursts at `BURST_FACTOR` × cruise with glides that slowly ease back to cruise.
    /// A `hungry` fish bursts.
    fn pace(&mut self, seconds: f32, hungry: bool, random: &mut Rng) {
        self.timer -= seconds;
        if self.timer <= 0.0 || (hungry && !self.bursting) {
            self.bursting = !self.bursting;
            self.timer = if self.bursting {
                0.7 + random.f32() * 0.8
            } else {
                1.0 + random.f32() * 3.0
            };
        }
        let (target, rate) = if self.bursting {
            (self.cruise() * BURST_FACTOR, 2.5)
        } else {
            (self.cruise(), 0.5)
        };
        self.speed += (target - self.speed) * (rate * seconds).min(1.0);
    }

    /// The heading to steer towards.
    fn steering(&self, neighbours: &[Fish], bounds: (f32, f32), noticed: Attractions) -> f32 {
        let head = self.spine[0];
        let wander = self.heading + self.wander;
        let (mut x, mut y) = (wander.cos(), wander.sin());
        // A fish whose head sits exactly on ours is this fish.
        for neighbour in neighbours.iter().filter(|fish| fish.spine[0] != head) {
            let (away_x, away_y) = neighbour
                .spine
                .iter()
                .map(|&(node_x, node_y)| (head.0 - node_x, head.1 - node_y))
                .min_by(|first, second| first.0.hypot(first.1).total_cmp(&second.0.hypot(second.1)))
                .expect("a spine has nodes");
            let distance = away_x.hypot(away_y);
            if distance < self.length && distance > 0.0 {
                let push = SEPARATION_WEIGHT * (1.0 - distance / self.length) / distance;
                x += away_x * push;
                y += away_y * push;
            }
            if distance < 2.0 * self.length {
                x += ALIGNMENT_WEIGHT * neighbour.heading.cos();
                y += ALIGNMENT_WEIGHT * neighbour.heading.sin();
            }
        }
        // Feel for the edges a U-turn (two turn radii) and some ahead, so there is room to turn.
        let reach = 2.5 * self.length / TURNING;
        let near = edge(head.0 + self.heading.cos() * reach, bounds.0)
            .hypot(edge(head.1 + self.heading.sin() * reach, bounds.1));
        // Towards the centre rather than straight off the wall, so a fish facing a wall or a
        // corner knows which way to turn.
        let (centre_x, centre_y) = (bounds.0 / 2.0 - head.0, bounds.1 / 2.0 - head.1);
        let pull = EDGE_WEIGHT * near / centre_x.hypot(centre_y).max(1.0);
        let (food_x, food_y) = noticed.food.map_or((0.0, 0.0), |food| hunger(head, food));
        let (curious_x, curious_y) = noticed
            .pointer
            .map_or((0.0, 0.0), |pointer| curiosity(head, self.heading, pointer));
        (y + centre_y * pull + food_y + curious_y).atan2(x + centre_x * pull + food_x + curious_x)
    }

    /// `agility` scales both the turn rate and its cap.
    fn turn_towards(&mut self, target: f32, agility: f32, seconds: f32) {
        let limit = agility * TURNING * self.speed / self.length;
        let rate = (agility * wrap(target - self.heading) * STEERING_GAIN).clamp(-limit, limit);
        self.heading = (self.heading + rate * seconds).rem_euclid(TAU);
    }

    /// Each node trails its parent at `length / SEGMENTS`, bending at most `MAX_BEND` per joint.
    fn follow(&mut self) {
        let gap = self.length / SEGMENTS as f32;
        let mut forward = self.heading;
        for index in 1..SEGMENTS {
            let (ahead, behind) = (self.spine[index - 1], self.spine[index]);
            let angle = (ahead.1 - behind.1).atan2(ahead.0 - behind.0);
            forward += wrap(angle - forward).clamp(-MAX_BEND, MAX_BEND);
            self.spine[index] = (ahead.0 - forward.cos() * gap, ahead.1 - forward.sin() * gap);
        }
    }

    /// The spine with a sine wave travelling head to tail, larger towards the tail and in a burst.
    fn waved(&self, phase: f32) -> [(f32, f32); SEGMENTS] {
        let amplitude = self.length * 0.05 * self.speed / self.cruise();
        let beat = phase * TAIL_BEATS_PER_SECOND * TAU + self.beat;
        std::array::from_fn(|index| {
            let (x, y) = self.spine[index];
            let forward = if index == 0 {
                self.heading
            } else {
                let ahead = self.spine[index - 1];
                (ahead.1 - y).atan2(ahead.0 - x)
            };
            let along = index as f32 / (SEGMENTS - 1) as f32;
            let offset = amplitude * along.powf(1.5) * (beat - along * 4.0).sin();
            (x - forward.sin() * offset, y + forward.cos() * offset)
        })
    }

    /// `phase`: seconds, driving the tail wave and the fin strokes.
    pub fn draw(&self, phase: f32, pixels: &mut Pixels) {
        let spine = self.waved(phase);
        let widest = self.length * 0.15;
        let fin = dim(FIN, self.depth);

        let angle = |from: (f32, f32), to: (f32, f32)| (to.1 - from.1).atan2(to.0 - from.0);
        let at = |origin: (f32, f32), angle: f32, distance: f32| {
            (
                origin.0 + angle.cos() * distance,
                origin.1 + angle.sin() * distance,
            )
        };
        let tail = spine[SEGMENTS - 1];
        let backward = angle(spine[SEGMENTS - 2], tail);
        for side in [-1.0, 1.0] {
            let lobe = at(tail, backward + side * 0.5, self.length * 0.2);
            stroke(pixels, tail, lobe, 0.8, fin);
        }
        let shoulder = spine[3];
        let forward = angle(shoulder, spine[2]);
        let flap = 0.25 * (phase * TAIL_BEATS_PER_SECOND * TAU + self.beat).sin();
        for side in [-1.0, 1.0] {
            let tip = at(shoulder, forward + side * (2.0 + flap), self.length * 0.15);
            stroke(pixels, shoulder, tip, 0.7, fin);
        }

        let reach = widest.ceil() as i32 + 1;
        let (low_x, high_x, low_y, high_y) = spine.iter().fold(
            (f32::MAX, f32::MIN, f32::MAX, f32::MIN),
            |(low_x, high_x, low_y, high_y), &(x, y)| {
                (low_x.min(x), high_x.max(x), low_y.min(y), high_y.max(y))
            },
        );
        for y in low_y as i32 - reach..=high_y as i32 + reach {
            for x in low_x as i32 - reach..=high_x as i32 + reach {
                let point = (x as f32 + 0.5, y as f32 + 0.5);
                let (along, distance_squared, side) = spine
                    .windows(2)
                    .enumerate()
                    .map(|(index, pair)| {
                        let (fraction, distance_squared, side) = project(pair[0], pair[1], point);
                        let along = (index as f32 + fraction) / (SEGMENTS - 1) as f32;
                        (along, distance_squared, side)
                    })
                    .min_by(|first, second| first.1.total_cmp(&second.1))
                    .expect("a spine has segments");
                let half_width = widest * profile(along);
                let across = side * distance_squared.sqrt() / half_width;
                if across.abs() > 1.0 {
                    continue;
                }
                let color = self
                    .markings
                    .iter()
                    .rev()
                    .find(|marking| {
                        let along_distance = (along - marking.along) * self.length;
                        let across_distance = (across - marking.across) * half_width;
                        along_distance.hypot(across_distance) <= marking.radius
                    })
                    .map_or(WHITE, |marking| marking.color);
                let rounding = 1.0 - 0.3 * across * across;
                pixels.set(x, y, dim(color, self.depth * rounding));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use fastrand::Rng;

    use super::*;

    const STEP: f32 = 1.0 / 60.0;
    const BOUNDS: (f32, f32) = (80.0, 48.0);

    fn school(random: &mut Rng, count: usize) -> Vec<Fish> {
        (0..count).map(|_| Fish::spawn(random, BOUNDS)).collect()
    }

    fn step(school: &mut [Fish], random: &mut Rng) {
        let neighbours = school.to_vec();
        for fish in school.iter_mut() {
            fish.swim(STEP, &neighbours, BOUNDS, Attractions::default(), random);
        }
    }

    #[test]
    fn fish_stay_in_bounds() {
        let mut random = Rng::with_seed(7);
        let mut fish = school(&mut random, 5);
        for steps in 0..10_000 {
            step(&mut fish, &mut random);
            for fish in &fish {
                let length = fish.length;
                for &(x, y) in &fish.spine {
                    assert!((-length..=BOUNDS.0 + length).contains(&x), "x {x}");
                    assert!((-length..=BOUNDS.1 + length).contains(&y), "y {y}");
                }
                if steps >= 300 {
                    let (x, y) = fish.spine[0];
                    assert!(
                        (-4.0..=BOUNDS.0 + 4.0).contains(&x),
                        "head x {x} at {steps}"
                    );
                    assert!(
                        (-4.0..=BOUNDS.1 + 4.0).contains(&y),
                        "head y {y} at {steps}"
                    );
                }
            }
        }
    }

    #[test]
    fn long_fish_turn_wider() {
        let mut random = Rng::with_seed(7);
        let turn = |length: f32, random: &mut Rng| {
            let mut fish = Fish::spawn(random, BOUNDS);
            fish.length = length;
            fish.heading = 0.0;
            fish.speed = 6.0;
            fish.turn_towards(std::f32::consts::FRAC_PI_2, 1.0, STEP);
            fish.heading
        };
        assert!(turn(40.0, &mut random) < turn(20.0, &mut random));
    }

    #[test]
    fn the_spine_keeps_its_length() {
        let mut random = Rng::with_seed(7);
        let mut fish = school(&mut random, 5);
        for _ in 0..1_000 {
            step(&mut fish, &mut random);
        }
        for fish in &fish {
            let gap = fish.length / SEGMENTS as f32;
            for pair in fish.spine.windows(2) {
                let distance = (pair[0].0 - pair[1].0).hypot(pair[0].1 - pair[1].1);
                assert!((distance - gap).abs() <= gap * 0.01, "{distance} vs {gap}");
            }
        }
    }
}
