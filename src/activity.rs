//! One camera's events, one per kind at a time: a person event lasts while
//! anyone is in view, and ends once nobody has been seen for the cooldown.
//! Pure: detections and times in, event actions out. The daemon sends them
//! as `event.start` / `event.update` / `event.end`.
use crate::detector::Detection;
use crate::settings::{DetectSettings, Kind};
use std::collections::BTreeSet;

/// How often an open event's peak score, area and zones are sent.
pub const UPDATE_INTERVAL_MS: i64 = 2000;
/// A sighting not yet long enough to report is forgotten after this gap.
const CANDIDATE_GAP_MS: i64 = 2000;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Start {
        kind: Kind,
        score: f32,
    },
    Update {
        kind: Kind,
        peak: f32,
        bbox: Option<[f32; 4]>,
        zones: Vec<String>,
    },
    End {
        kind: Kind,
    },
}

#[derive(Debug, Default)]
struct Track {
    open: bool,
    /// Since when it has been in view (not yet reported).
    first_seen_ms: Option<i64>,
    last_seen_ms: i64,
    updated_ms: i64,
    peak: f32,
    bbox: Option<[f32; 4]>,
    zones: BTreeSet<String>,
}

#[derive(Debug)]
pub struct Activity {
    tracks: [Track; 3],
    min_duration_ms: i64,
    cooldown_ms: i64,
}

impl Activity {
    pub fn new(settings: &DetectSettings) -> Self {
        Self {
            tracks: Default::default(),
            min_duration_ms: (settings.min_duration_sec * 1000.0) as i64,
            cooldown_ms: (settings.cooldown_sec * 1000.0) as i64,
        }
    }

    pub fn is_open(&self, kind: Kind) -> bool {
        self.tracks[kind.index()].open
    }

    /// One analysed picture's detections (already filtered by the settings).
    pub fn observe(
        &mut self,
        detections: &[Detection],
        settings: &DetectSettings,
        now_ms: i64,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        for kind in Kind::ALL {
            let seen: Vec<&Detection> = detections.iter().filter(|d| d.kind == kind).collect();
            let (min_duration, cooldown) = (self.min_duration_ms, self.cooldown_ms);
            let track = &mut self.tracks[kind.index()];
            if seen.is_empty() {
                if track.open && now_ms - track.last_seen_ms >= cooldown {
                    actions.extend(finish(track, kind));
                } else if !track.open
                    && track
                        .first_seen_ms
                        .is_some_and(|_| now_ms - track.last_seen_ms > CANDIDATE_GAP_MS)
                {
                    *track = Track::default();
                }
                continue;
            }
            let first = *track.first_seen_ms.get_or_insert(now_ms);
            track.last_seen_ms = now_ms;
            for detection in &seen {
                track.peak = track.peak.max(detection.score);
                track.bbox = Some(union(track.bbox, detection.rect));
                let (x, y) = detection.foot();
                track.zones.extend(settings.zones_at(x, y));
            }
            if !track.open && now_ms - first >= min_duration {
                track.open = true;
                actions.push(Action::Start {
                    kind,
                    score: track.peak,
                });
                // The first update follows at once, with what was seen so far.
                track.updated_ms = now_ms - UPDATE_INTERVAL_MS;
            }
            if track.open && now_ms - track.updated_ms >= UPDATE_INTERVAL_MS {
                actions.push(update(track, kind));
                track.updated_ms = now_ms;
            }
        }
        actions
    }

    /// End every open event: on a gap in the stream, while the camera moves,
    /// when settings change and on stop.
    pub fn finish(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        for kind in Kind::ALL {
            actions.extend(finish(&mut self.tracks[kind.index()], kind));
        }
        actions
    }
}

fn update(track: &Track, kind: Kind) -> Action {
    Action::Update {
        kind,
        peak: track.peak,
        bbox: track.bbox,
        zones: track.zones.iter().cloned().collect(),
    }
}

fn finish(track: &mut Track, kind: Kind) -> Vec<Action> {
    let actions = if track.open {
        vec![update(track, kind), Action::End { kind }]
    } else {
        vec![]
    };
    *track = Track::default();
    actions
}

fn union(current: Option<[f32; 4]>, rect: [f32; 4]) -> [f32; 4] {
    match current {
        None => rect,
        Some(c) => {
            let x0 = c[0].min(rect[0]);
            let y0 = c[1].min(rect[1]);
            let x1 = (c[0] + c[2]).max(rect[0] + rect[2]);
            let y1 = (c[1] + c[3]).max(rect[1] + rect[3]);
            [x0, y0, x1 - x0, y1 - y0]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(score: f32) -> Detection {
        Detection {
            kind: Kind::Person,
            class: "person",
            score,
            rect: [0.2, 0.2, 0.1, 0.4],
        }
    }

    fn settings() -> DetectSettings {
        DetectSettings {
            min_duration_sec: 1.0,
            cooldown_sec: 5.0,
            ..DetectSettings::default()
        }
    }

    #[test]
    fn a_person_is_reported_after_the_minimum_duration_and_ends_after_the_cooldown() {
        let settings = settings();
        let mut activity = Activity::new(&settings);
        assert!(activity.observe(&[person(0.6)], &settings, 0).is_empty());
        assert!(activity.observe(&[person(0.7)], &settings, 500).is_empty());
        let started = activity.observe(&[person(0.8)], &settings, 1000);
        assert_eq!(started.len(), 2);
        assert_eq!(
            started[0],
            Action::Start {
                kind: Kind::Person,
                score: 0.8
            }
        );
        assert!(activity.is_open(Kind::Person));
        // Out of view, but not for long enough.
        assert!(activity.observe(&[], &settings, 4000).is_empty());
        let ended = activity.observe(&[], &settings, 6000);
        assert_eq!(ended.last(), Some(&Action::End { kind: Kind::Person }));
        assert!(!activity.is_open(Kind::Person));
    }

    #[test]
    fn a_glimpse_shorter_than_the_minimum_is_forgotten() {
        let settings = settings();
        let mut activity = Activity::new(&settings);
        activity.observe(&[person(0.9)], &settings, 0);
        assert!(activity.observe(&[], &settings, 3000).is_empty());
        // A new sighting starts its own count.
        assert!(activity.observe(&[person(0.9)], &settings, 3500).is_empty());
        assert!(!activity.is_open(Kind::Person));
    }

    #[test]
    fn kinds_have_their_own_events() {
        let settings = DetectSettings {
            min_duration_sec: 0.0,
            ..settings()
        };
        let mut activity = Activity::new(&settings);
        let dog = Detection {
            kind: Kind::Animal,
            class: "dog",
            score: 0.7,
            rect: [0.5, 0.5, 0.1, 0.1],
        };
        let actions = activity.observe(&[person(0.9), dog], &settings, 0);
        let starts: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                Action::Start { kind, .. } => Some(*kind),
                _ => None,
            })
            .collect();
        assert_eq!(starts, vec![Kind::Person, Kind::Animal]);
        let ended = activity.finish();
        assert_eq!(
            ended
                .iter()
                .filter(|a| matches!(a, Action::End { .. }))
                .count(),
            2
        );
    }
}
