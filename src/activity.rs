//! One camera's events, one per object: an object's event starts once it has
//! counted (`crate::objects`: confirmed, in the areas events need, not
//! stationary) for the minimum duration, and ends once it has not counted for
//! the cooldown. An object lost and found again where it was, before its
//! event ended, keeps that event. An event is an alert or a detection; a
//! detection becomes an alert when its object comes to make it one. Pure:
//! presence and times in, event actions out. The daemon sends them as
//! `event.start` / `event.update` / `event.end`.
use crate::objects::{same_place, Presence};
use crate::settings::DetectSettings;
use std::collections::{BTreeMap, BTreeSet};

/// How often an open event's peak score, area and zones are sent.
pub const UPDATE_INTERVAL_MS: i64 = 2000;
/// A sighting not yet long enough to report is forgotten after this gap.
const CANDIDATE_GAP_MS: i64 = 2000;

/// `event` numbers this camera's events, from 1 (the daemon gives each its
/// protocol number at its start).
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Start {
        event: u64,
        kind: String,
        score: f32,
        alert: bool,
    },
    Update {
        event: u64,
        peak: f32,
        bbox: Option<[f32; 4]>,
        zones: Vec<String>,
        alert: bool,
    },
    End {
        event: u64,
    },
}

#[derive(Debug)]
struct Track {
    kind: String,
    /// The object it follows now ([`Presence::object`]).
    object: u64,
    /// Where that object was last.
    rect: [f32; 4],
    open: bool,
    /// Since when it has counted (not yet reported).
    first_seen_ms: i64,
    last_seen_ms: i64,
    updated_ms: i64,
    peak: f32,
    bbox: Option<[f32; 4]>,
    zones: BTreeSet<String>,
    alert: bool,
}

#[derive(Debug)]
pub struct Activity {
    /// The objects counting lately, by event number.
    tracks: BTreeMap<u64, Track>,
    next: u64,
    min_duration_ms: i64,
    cooldown_ms: i64,
}

impl Activity {
    pub fn new(settings: &DetectSettings) -> Self {
        Self {
            tracks: BTreeMap::new(),
            next: 0,
            min_duration_ms: (settings.min_duration_sec * 1000.0) as i64,
            cooldown_ms: (settings.cooldown_sec * 1000.0) as i64,
        }
    }

    /// Open events of `kind`.
    pub fn open_of(&self, kind: &str) -> usize {
        self.tracks
            .values()
            .filter(|track| track.open && track.kind == kind)
            .count()
    }

    /// One analysed picture: its counting objects.
    pub fn observe(&mut self, present: &[Presence], now_ms: i64) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut seen: BTreeSet<u64> = BTreeSet::new();
        for presence in present {
            let event = self.track_of(presence, present, &seen, now_ms);
            seen.insert(event);
            let min_duration = self.min_duration_ms;
            let Some(track) = self.tracks.get_mut(&event) else {
                continue;
            };
            track.object = presence.object;
            track.rect = presence.rect;
            track.last_seen_ms = now_ms;
            track.peak = track.peak.max(presence.score);
            track.zones.extend(presence.zones.iter().cloned());
            let upgraded = track.open && presence.alert && !track.alert;
            track.alert |= presence.alert;
            if !track.open && now_ms.saturating_sub(track.first_seen_ms) >= min_duration {
                track.open = true;
                // The event's box is its object on the picture it started on
                // (near its thumbnail's moment), not where it went later.
                track.bbox = Some(presence.rect);
                actions.push(Action::Start {
                    event,
                    kind: track.kind.clone(),
                    score: track.peak,
                    alert: track.alert,
                });
                // The first update follows at once, with what was seen so far.
                track.updated_ms = now_ms.saturating_sub(UPDATE_INTERVAL_MS);
            }
            if upgraded
                || (track.open && now_ms.saturating_sub(track.updated_ms) >= UPDATE_INTERVAL_MS)
            {
                actions.push(update(event, track));
                track.updated_ms = now_ms;
            }
        }
        let cooldown = self.cooldown_ms;
        let mut gone = Vec::new();
        for (event, track) in &self.tracks {
            if seen.contains(event) {
                continue;
            }
            let since = now_ms.saturating_sub(track.last_seen_ms);
            if track.open && since >= cooldown {
                actions.push(update(*event, track));
                actions.push(Action::End { event: *event });
                gone.push(*event);
            } else if !track.open && since > CANDIDATE_GAP_MS {
                gone.push(*event);
            }
        }
        for event in gone {
            self.tracks.remove(&event);
        }
        actions
    }

    /// The event `presence`'s object belongs to: its own; else one whose
    /// object was lost where this one is now, not yet over; else a new one.
    fn track_of(
        &mut self,
        presence: &Presence,
        present: &[Presence],
        taken: &BTreeSet<u64>,
        now_ms: i64,
    ) -> u64 {
        if let Some((event, _)) = self
            .tracks
            .iter()
            .find(|(event, track)| track.object == presence.object && !taken.contains(event))
        {
            return *event;
        }
        let (cooldown, lost) = (self.cooldown_ms, |track: &Track| {
            !present.iter().any(|other| other.object == track.object)
        });
        let found = self
            .tracks
            .iter()
            .filter(|(event, track)| {
                !taken.contains(event)
                    && track.kind == presence.kind
                    && track.last_seen_ms < now_ms
                    && lost(track)
                    && now_ms.saturating_sub(track.last_seen_ms)
                        < if track.open {
                            cooldown
                        } else {
                            CANDIDATE_GAP_MS
                        }
                    && same_place(&track.rect, &presence.rect)
            })
            .max_by_key(|(_, track)| track.last_seen_ms)
            .map(|(event, _)| *event);
        if let Some(event) = found {
            return event;
        }
        self.next += 1;
        self.tracks.insert(
            self.next,
            Track {
                kind: presence.kind.clone(),
                object: presence.object,
                rect: presence.rect,
                open: false,
                first_seen_ms: now_ms,
                last_seen_ms: now_ms,
                updated_ms: now_ms,
                peak: 0.0,
                bbox: None,
                zones: BTreeSet::new(),
                alert: false,
            },
        );
        self.next
    }

    /// End every open event: on a gap in the stream, while the camera moves,
    /// when settings change and on stop.
    pub fn finish(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        for (event, track) in &self.tracks {
            if track.open {
                actions.push(update(*event, track));
                actions.push(Action::End { event: *event });
            }
        }
        self.tracks.clear();
        actions
    }
}

fn update(event: u64, track: &Track) -> Action {
    Action::Update {
        event,
        peak: track.peak,
        bbox: track.bbox,
        zones: track.zones.iter().cloned().collect(),
        alert: track.alert,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(object: u64, kind: &str, x: f32, score: f32, alert: bool) -> Presence {
        Presence {
            object,
            kind: kind.into(),
            score,
            rect: [x, 0.2, 0.1, 0.4],
            zones: vec![],
            alert,
        }
    }

    fn person(score: f32) -> Presence {
        seen(1, "person", 0.2, score, true)
    }

    fn settings() -> DetectSettings {
        DetectSettings {
            min_duration_sec: 1.0,
            cooldown_sec: 5.0,
            ..DetectSettings::default()
        }
    }

    fn instant() -> DetectSettings {
        DetectSettings {
            min_duration_sec: 0.0,
            ..settings()
        }
    }

    fn starts(actions: &[Action]) -> Vec<u64> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Start { event, .. } => Some(*event),
                _ => None,
            })
            .collect()
    }

    fn ends(actions: &[Action]) -> Vec<u64> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::End { event } => Some(*event),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_object_is_reported_after_the_minimum_duration_and_ends_after_the_cooldown() {
        let settings = settings();
        let mut activity = Activity::new(&settings);
        assert!(activity.observe(&[person(0.6)], 0).is_empty());
        assert!(activity.observe(&[person(0.7)], 500).is_empty());
        let started = activity.observe(&[person(0.8)], 1000);
        assert_eq!(started.len(), 2);
        assert_eq!(
            started[0],
            Action::Start {
                event: 1,
                kind: "person".into(),
                score: 0.8,
                alert: true
            }
        );
        assert_eq!(activity.open_of("person"), 1);
        // Out of view, but not for long enough.
        assert!(activity.observe(&[], 4000).is_empty());
        let ended = activity.observe(&[], 6000);
        assert_eq!(ended.last(), Some(&Action::End { event: 1 }));
        assert_eq!(activity.open_of("person"), 0);
    }

    #[test]
    fn a_glimpse_shorter_than_the_minimum_is_forgotten() {
        let settings = settings();
        let mut activity = Activity::new(&settings);
        activity.observe(&[person(0.9)], 0);
        assert!(activity.observe(&[], 3000).is_empty());
        // A new sighting starts its own count.
        assert!(activity.observe(&[person(0.9)], 3500).is_empty());
        assert_eq!(activity.open_of("person"), 0);
    }

    #[test]
    fn each_car_on_a_busy_road_is_its_own_event() {
        // Cars keep coming, a new one before the last has gone: each is
        // reported, not folded into one long event of the first car.
        let settings = instant();
        let mut activity = Activity::new(&settings);
        let mut started = Vec::new();
        let mut ended = Vec::new();
        for step in 0..40i64 {
            let now = step * 500;
            // Car n is in view from picture 5n to 5n + 7, moving right.
            let present: Vec<Presence> = (0..8u64)
                .filter(|n| (5 * *n as i64..5 * *n as i64 + 8).contains(&step))
                .map(|n| seen(n + 1, "car", (step - 5 * n as i64) as f32 * 0.1, 0.9, true))
                .collect();
            let actions = activity.observe(&present, now);
            started.extend(starts(&actions));
            ended.extend(ends(&actions));
        }
        ended.extend(ends(&activity.finish()));
        assert_eq!(started.len(), 8, "eight cars, eight events");
        assert_eq!(started, ended, "each ends");
        assert!(activity.open_of("car") == 0);
    }

    #[test]
    fn two_objects_at_once_are_two_events_with_their_own_boxes() {
        let settings = instant();
        let mut activity = Activity::new(&settings);
        let actions = activity.observe(
            &[
                seen(1, "person", 0.1, 0.9, true),
                seen(2, "person", 0.7, 0.8, true),
            ],
            0,
        );
        assert_eq!(starts(&actions), [1, 2]);
        let boxes: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                Action::Update { bbox, .. } => *bbox,
                _ => None,
            })
            .collect();
        assert_eq!(boxes, [[0.1, 0.2, 0.1, 0.4], [0.7, 0.2, 0.1, 0.4]]);
        assert_eq!(activity.open_of("person"), 2);
    }

    #[test]
    fn an_object_lost_and_found_again_where_it_was_keeps_its_event() {
        // The tracker gives an object hidden for a while a new id; found
        // where it was, before its event ended, it is the same event.
        let settings = instant();
        let mut activity = Activity::new(&settings);
        assert_eq!(
            starts(&activity.observe(&[seen(1, "car", 0.3, 0.9, true)], 0)),
            [1]
        );
        assert!(activity.observe(&[], 2500).is_empty());
        let again = activity.observe(&[seen(7, "car", 0.32, 0.9, true)], 3000);
        assert!(starts(&again).is_empty(), "{again:?}");
        // Elsewhere, or another class: a new event.
        let other = activity.observe(
            &[
                seen(7, "car", 0.34, 0.9, true),
                seen(8, "car", 0.85, 0.9, true),
                seen(9, "dog", 0.34, 0.9, false),
            ],
            3500,
        );
        assert_eq!(starts(&other), [2, 3]);
        assert_eq!(activity.open_of("car"), 2);
    }

    #[test]
    fn a_detection_becomes_an_alert_when_its_object_makes_it_one() {
        let settings = instant();
        let mut activity = Activity::new(&settings);
        let started = activity.observe(&[seen(1, "person", 0.2, 0.8, false)], 0);
        assert!(matches!(&started[0], Action::Start { alert: false, .. }));
        // Within the update interval, the upgrade is sent at once.
        let upgraded = activity.observe(&[seen(1, "person", 0.2, 0.8, true)], 500);
        assert!(
            matches!(upgraded.as_slice(), [Action::Update { alert: true, .. }]),
            "{upgraded:?}"
        );
        // And it stays an alert.
        let later = activity.observe(&[seen(1, "person", 0.2, 0.8, false)], 3000);
        assert!(matches!(
            later.as_slice(),
            [Action::Update { alert: true, .. }]
        ));
    }

    #[test]
    fn an_events_box_is_its_object_where_it_started_not_where_it_went() {
        let settings = instant();
        let mut activity = Activity::new(&settings);
        activity.observe(&[seen(1, "car", 0.1, 0.9, true)], 0);
        for (i, x) in [0.15, 0.2, 0.25, 0.3].into_iter().enumerate() {
            activity.observe(&[seen(1, "car", x, 0.9, true)], 2000 * (i as i64 + 1));
        }
        let ended = activity.finish();
        let bbox = ended.iter().find_map(|a| match a {
            Action::Update { bbox, .. } => *bbox,
            _ => None,
        });
        assert_eq!(bbox, Some([0.1, 0.2, 0.1, 0.4]));
    }

    #[test]
    fn extreme_timestamps_do_not_overflow() {
        let settings = settings();
        let mut activity = Activity::new(&settings);
        activity.observe(&[person(0.6)], i64::MIN);
        activity.observe(&[person(0.6)], i64::MAX);
        activity.observe(&[], i64::MAX);
    }
}
