//! Each object the detector finds, followed from picture to picture, and the
//! rules that decide whether it counts:
//!
//! - **Confirmed:** the median of its recent scores (padded to three with
//!   zeros) has reached its class's `confirm`. One lucky sighting does not
//!   start an event, and one weak sighting does not end one.
//! - **In a watched area:** where it stands (the middle of its box's bottom
//!   edge) has been inside the area for the area's `frames` pictures in a row,
//!   for at least its `loiter_sec`, at least at its `min_speed_kmh` (a
//!   4-corner area with its real side lengths gives ground speed), and the
//!   area applies to its class.
//! - **Raises an event:** with any area marked `events`, only an object that
//!   has been in one of them; **an alert:** its class is an alert class and,
//!   with any area marked `alerts`, it has been in one of them.
//! - **Stationary:** it has not moved for `stationary_after_sec`. A
//!   stationary object no longer starts or keeps an event, except in an area
//!   with a loitering time: a person keeps it while still there, anything else
//!   until that time has passed.
//!
//! Pure: detections and times in, objects and the counting ones out.
use crate::detector::Detection;
use crate::settings::{DetectSettings, Zone};
use std::collections::VecDeque;
use std::sync::Arc;

/// An object not seen for this long is gone.
const LOST_MS: i64 = 2000;
/// The scores kept for the median.
const SCORES: usize = 10;
/// The fewest values the median is taken over (missing ones are zeros).
const MEDIAN_OF_AT_LEAST: usize = 3;
/// Two sightings are one object when their boxes overlap at least this much
/// (intersection over union), or their centres are close for their size.
const SAME_OBJECT_IOU: f32 = 0.2;
const SAME_OBJECT_DISTANCE: f32 = 0.6;
/// It has moved when its centre left a circle of this share of its box's
/// diagonal around where it stopped.
const MOVED: f32 = 0.15;
/// Where it stood over this long gives its speed.
const SPEED_WINDOW_MS: i64 = 1500;

/// One object followed over pictures.
#[derive(Debug, Clone)]
pub struct Object {
    pub id: u64,
    /// The model's class index (its colour) and its id (the event kind).
    pub class: usize,
    pub kind: Arc<str>,
    pub label: Arc<str>,
    /// Its newest box and score.
    pub rect: [f32; 4],
    pub score: f32,
    /// Seen on the newest picture.
    pub seen_now: bool,
    pub confirmed: bool,
    /// Has been in an area marked `events` / `alerts`.
    pub entered_events: bool,
    pub entered_alerts: bool,
    pub stationary: bool,
    scores: VecDeque<f32>,
    seen_ms: i64,
    /// Where it last stopped moving, and since when.
    rest: ([f32; 2], i64),
    /// Where it stood, and when, for its speed.
    feet: VecDeque<(i64, [f32; 2])>,
    /// Per watched area, in the settings' order.
    areas: Vec<AreaState>,
}

#[derive(Debug, Clone, Copy, Default)]
struct AreaState {
    /// Pictures in a row standing inside.
    run: u32,
    /// When that run began.
    since_ms: i64,
    /// Counts as in the area now (all of the area's rules met).
    counts: bool,
}

/// One counting object on one picture: what its event is told.
#[derive(Debug, Clone, PartialEq)]
pub struct Presence {
    /// The object ([`Object::id`]).
    pub object: u64,
    pub kind: String,
    pub score: f32,
    /// Its box.
    pub rect: [f32; 4],
    /// The watched areas it counts in.
    pub zones: Vec<String>,
    /// It makes its event an alert.
    pub alert: bool,
}

#[derive(Debug, Default)]
pub struct Objects {
    next_id: u64,
    objects: Vec<Object>,
}

impl Objects {
    pub fn new() -> Self {
        Self::default()
    }

    /// The objects seen on the newest picture.
    pub fn seen(&self) -> impl Iterator<Item = &Object> {
        self.objects.iter().filter(|object| object.seen_now)
    }

    /// Follow one analysed picture's detections (already kept by
    /// [`crate::detector::keep`]).
    pub fn observe(&mut self, detections: &[Detection], settings: &DetectSettings, now_ms: i64) {
        for object in &mut self.objects {
            object.seen_now = false;
        }
        let matched = self.match_detections(detections);
        for (index, detection) in detections.iter().enumerate() {
            match matched[index] {
                Some(at) => self.objects[at].sighted(detection, now_ms),
                None => {
                    self.next_id += 1;
                    self.objects
                        .push(Object::new(self.next_id, detection, now_ms));
                }
            }
        }
        self.objects
            .retain(|object| now_ms.saturating_sub(object.seen_ms) <= LOST_MS);
        let watched: Vec<&Zone> = settings.watched_zones().collect();
        let needs_events = watched.iter().any(|zone| zone.events);
        let needs_alerts = watched.iter().any(|zone| zone.alerts);
        for object in self.objects.iter_mut().filter(|object| object.seen_now) {
            object.judge(settings, &watched, now_ms);
            if !needs_events {
                object.entered_events = true;
            }
            if !needs_alerts {
                object.entered_alerts = true;
            }
        }
    }

    /// The counting objects on the newest picture, each with what its event
    /// is told. An object counts when it is seen now, confirmed, has been in
    /// an events area (when there are any) and is not stationary (see the
    /// module).
    pub fn presence(&self, settings: &DetectSettings, now_ms: i64) -> Vec<Presence> {
        let watched: Vec<&Zone> = settings.watched_zones().collect();
        self.seen()
            .filter(|object| object.counts(&watched, now_ms))
            .map(|object| Presence {
                object: object.id,
                kind: object.kind.to_string(),
                score: object.score,
                rect: object.rect,
                zones: watched
                    .iter()
                    .zip(&object.areas)
                    .filter(|(_, state)| state.counts)
                    .map(|(zone, _)| zone.id.clone())
                    .collect(),
                alert: object.entered_alerts
                    && settings
                        .alert_classes
                        .iter()
                        .any(|class| *class == *object.kind),
            })
            .collect()
    }

    /// Whether this object, seen now, counts towards its event.
    pub fn counts(&self, object: &Object, settings: &DetectSettings, now_ms: i64) -> bool {
        let watched: Vec<&Zone> = settings.watched_zones().collect();
        object.counts(&watched, now_ms)
    }

    /// For each detection, the object it continues (`None`: a new object).
    /// Greedy: the best overlapping pairs of the same class first.
    fn match_detections(&self, detections: &[Detection]) -> Vec<Option<usize>> {
        let mut pairs: Vec<(f32, usize, usize)> = Vec::new();
        for (d, detection) in detections.iter().enumerate() {
            for (o, object) in self.objects.iter().enumerate() {
                if object.kind != detection.id {
                    continue;
                }
                let overlap = iou(&object.rect, &detection.rect);
                let close = centre_distance(&object.rect, &detection.rect)
                    / object.rect[2].max(object.rect[3]).max(1e-3);
                if overlap >= SAME_OBJECT_IOU || close <= SAME_OBJECT_DISTANCE {
                    // Overlap first, then closeness.
                    pairs.push((overlap + (1.0 - close.min(1.0)) * 0.01, d, o));
                }
            }
        }
        pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut matched = vec![None; detections.len()];
        let mut taken = vec![false; self.objects.len()];
        for (_, d, o) in pairs {
            if matched[d].is_none() && !taken[o] {
                matched[d] = Some(o);
                taken[o] = true;
            }
        }
        matched
    }
}

impl Object {
    fn new(id: u64, detection: &Detection, now_ms: i64) -> Self {
        let mut object = Self {
            id,
            class: detection.class,
            kind: detection.id.clone(),
            label: detection.label.clone(),
            rect: detection.rect,
            score: detection.score,
            seen_now: false,
            confirmed: false,
            entered_events: false,
            entered_alerts: false,
            stationary: false,
            scores: VecDeque::new(),
            seen_ms: now_ms,
            rest: (centre(&detection.rect), now_ms),
            feet: VecDeque::new(),
            areas: Vec::new(),
        };
        object.sighted(detection, now_ms);
        object
    }

    fn sighted(&mut self, detection: &Detection, now_ms: i64) {
        self.rect = detection.rect;
        self.score = detection.score;
        self.seen_now = true;
        self.seen_ms = now_ms;
        self.scores.push_back(detection.score);
        while self.scores.len() > SCORES {
            self.scores.pop_front();
        }
        let (x, y) = detection.foot();
        self.feet.push_back((now_ms, [x, y]));
        while self
            .feet
            .front()
            .is_some_and(|(at, _)| now_ms.saturating_sub(*at) > SPEED_WINDOW_MS)
        {
            self.feet.pop_front();
        }
        let here = centre(&self.rect);
        let diagonal = (self.rect[2].powi(2) + self.rect[3].powi(2)).sqrt();
        if distance(here, self.rest.0) > MOVED * diagonal {
            self.rest = (here, now_ms);
        }
    }

    /// Apply the settings' rules to this picture's sighting.
    fn judge(&mut self, settings: &DetectSettings, watched: &[&Zone], now_ms: i64) {
        let confirm = settings
            .class(&self.kind)
            .map_or(crate::settings::DEFAULT_CONFIRM, |choice| choice.confirm);
        self.confirmed |= median(&self.scores) >= f32::from(confirm) / 100.0;
        self.stationary = settings.stationary_after_sec > 0.0
            && now_ms.saturating_sub(self.rest.1)
                >= (settings.stationary_after_sec * 1000.0) as i64;
        if self.areas.len() != watched.len() {
            self.areas = vec![AreaState::default(); watched.len()];
        }
        let (x, y) = self.foot();
        for (zone, state) in watched.iter().zip(self.areas.iter_mut()) {
            if zone.applies_to(&self.kind) && zone.contains(x, y) {
                if state.run == 0 {
                    state.since_ms = now_ms;
                }
                state.run = state.run.saturating_add(1);
            } else {
                state.run = 0;
            }
            let long_enough =
                now_ms.saturating_sub(state.since_ms) >= (zone.loiter_sec * 1000.0) as i64;
            let fast_enough = zone.min_speed_kmh <= 0.0
                || speed_kmh(zone, &self.feet).is_none_or(|kmh| kmh >= zone.min_speed_kmh);
            state.counts = state.run >= u32::from(zone.frames.max(1)) && long_enough && fast_enough;
            if state.counts && self.confirmed {
                self.entered_events |= zone.events;
                self.entered_alerts |= zone.alerts;
            }
        }
    }

    /// Where it stands: the middle of its box's bottom edge.
    pub fn foot(&self) -> (f32, f32) {
        (
            self.rect[0] + self.rect[2] / 2.0,
            self.rect[1] + self.rect[3],
        )
    }

    fn counts(&self, watched: &[&Zone], now_ms: i64) -> bool {
        if !self.seen_now || !self.confirmed || !self.entered_events {
            return false;
        }
        if !self.stationary {
            return true;
        }
        // Stationary: only a loitering area keeps it (see the module).
        watched.iter().zip(&self.areas).any(|(zone, state)| {
            zone.loiter_sec > 0.0
                && state.run > 0
                && (&*self.kind == "person"
                    || now_ms.saturating_sub(state.since_ms) < (zone.loiter_sec * 1000.0) as i64)
        })
    }
}

/// The median of `scores` padded with zeros to at least three values.
fn median(scores: &VecDeque<f32>) -> f32 {
    let mut values: Vec<f32> = scores.iter().copied().collect();
    while values.len() < MEDIAN_OF_AT_LEAST {
        values.push(0.0);
    }
    values.sort_by(f32::total_cmp);
    values[values.len() / 2]
}

/// Ground speed over the speed window, in km/h, for a 4-corner area whose
/// real side lengths are known; `None` when it cannot be measured.
fn speed_kmh(zone: &Zone, feet: &VecDeque<(i64, [f32; 2])>) -> Option<f32> {
    let ground = Ground::of(zone)?;
    let (first, last) = (feet.front()?, feet.back()?);
    let seconds = (last.0 - first.0) as f32 / 1000.0;
    if seconds <= 0.0 {
        return None;
    }
    let (a, b) = (ground.map(first.1)?, ground.map(last.1)?);
    Some(distance(a, b) / seconds * 3.6)
}

/// A perspective map from a 4-corner area on the picture to the ground, in
/// metres: corner 1 at (0, 0), corner 2 at (width, 0), corner 3 at (width,
/// depth), corner 4 at (0, depth); width and depth average the opposite sides.
pub struct Ground {
    h: [f32; 8],
}

impl Ground {
    pub fn of(zone: &Zone) -> Option<Self> {
        if zone.points.len() != 4 || zone.sides_m.iter().any(|side| *side <= 0.0) {
            return None;
        }
        let [a, b, c, d] = zone.sides_m;
        let (width, depth) = ((a + c) / 2.0, (b + d) / 2.0);
        let to = [[0.0, 0.0], [width, 0.0], [width, depth], [0.0, depth]];
        homography(&zone.points, &to).map(|h| Self { h })
    }

    pub fn map(&self, [x, y]: [f32; 2]) -> Option<[f32; 2]> {
        let h = &self.h;
        let w = h[6] * x + h[7] * y + 1.0;
        if w.abs() < 1e-9 {
            return None;
        }
        Some([
            (h[0] * x + h[1] * y + h[2]) / w,
            (h[3] * x + h[4] * y + h[5]) / w,
        ])
    }
}

/// The 3×3 perspective transform (h33 = 1) taking `from[i]` to `to[i]`.
fn homography(from: &[[f32; 2]], to: &[[f32; 2]; 4]) -> Option<[f32; 8]> {
    let mut m = [[0f64; 9]; 8];
    for i in 0..4 {
        let (x, y) = (f64::from(from[i][0]), f64::from(from[i][1]));
        let (u, v) = (f64::from(to[i][0]), f64::from(to[i][1]));
        m[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        m[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    // Gaussian elimination with partial pivoting.
    for col in 0..8 {
        let pivot = (col..8).max_by(|a, b| m[*a][col].abs().total_cmp(&m[*b][col].abs()))?;
        if m[pivot][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, pivot);
        for row in 0..8 {
            if row != col {
                let factor = m[row][col] / m[col][col];
                for k in col..9 {
                    m[row][k] -= factor * m[col][k];
                }
            }
        }
    }
    let mut h = [0f32; 8];
    for (i, value) in h.iter_mut().enumerate() {
        *value = (m[i][8] / m[i][i]) as f32;
    }
    Some(h)
}

fn centre(rect: &[f32; 4]) -> [f32; 2] {
    [rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0]
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// Whether an object last at `last` may now be at `next` (the tracker's own
/// test, also used to give a lost object found again its open event).
pub(crate) fn same_place(last: &[f32; 4], next: &[f32; 4]) -> bool {
    iou(last, next) >= SAME_OBJECT_IOU
        || centre_distance(last, next) / last[2].max(last[3]).max(1e-3) <= SAME_OBJECT_DISTANCE
}

fn centre_distance(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    distance(centre(a), centre(b))
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = (a[0] + a[2]).min(b[0] + b[2]);
    let y1 = (a[1] + a[3]).min(b[1] + b[3]);
    let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
    let total = a[2] * a[3] + b[2] * b[3] - inter;
    if total <= 0.0 {
        0.0
    } else {
        inter / total
    }
}

#[cfg(test)]
mod tests;
