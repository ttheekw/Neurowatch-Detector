use super::*;
use crate::detector::tests::found;
use crate::settings::{ClassChoice, ZoneKind};

const STEP: i64 = 500; // 2 pictures a second

fn person_at(x: f32, score: f32) -> Detection {
    found(0, "person", score, [x, 0.3, 0.05, 0.3])
}

fn car_at(x: f32, score: f32) -> Detection {
    found(2, "car", score, [x, 0.5, 0.2, 0.15])
}

fn area(id: &str, x0: f32, x1: f32) -> Zone {
    Zone {
        id: id.into(),
        name: String::new(),
        kind: ZoneKind::Include,
        points: vec![[x0, 0.0], [x1, 0.0], [x1, 1.0], [x0, 1.0]],
        classes: vec![],
        frames: 1,
        loiter_sec: 0.0,
        min_speed_kmh: 0.0,
        sides_m: [0.0; 4],
        events: true,
        alerts: true,
    }
}

fn settings(zones: Vec<Zone>) -> DetectSettings {
    DetectSettings {
        classes: vec![ClassChoice::new("person", 50), ClassChoice::new("car", 50)],
        zones,
        ..DetectSettings::default()
    }
}

/// Feed `pictures` (each a list of detections) from `from_ms`, `STEP` apart;
/// the presence after each.
fn run(
    objects: &mut Objects,
    settings: &DetectSettings,
    from_ms: i64,
    pictures: &[Vec<Detection>],
) -> Vec<Vec<Presence>> {
    pictures
        .iter()
        .enumerate()
        .map(|(i, detections)| {
            let now = from_ms + i as i64 * STEP;
            objects.observe(detections, settings, now);
            objects.presence(settings, now)
        })
        .collect()
}

fn kinds(presence: &[Presence]) -> Vec<&str> {
    presence.iter().map(|p| p.kind.as_str()).collect()
}

#[test]
fn one_sure_sighting_is_not_enough_the_median_must_reach_confirm() {
    let s = settings(vec![]);
    let mut objects = Objects::new();
    // Median of [0, 0, 0.9] is 0; of [0, 0.8, 0.9] is 0.8 >= 70%.
    let seen = run(
        &mut objects,
        &s,
        0,
        &[vec![person_at(0.1, 0.9)], vec![person_at(0.1, 0.8)]],
    );
    assert!(seen[0].is_empty(), "one sighting does not count");
    assert_eq!(kinds(&seen[1]), ["person"]);
    // Then weak sightings do not make it doubtful again.
    let later = run(
        &mut objects,
        &s,
        1000,
        &[vec![person_at(0.1, 0.55)], vec![person_at(0.1, 0.5)]],
    );
    assert_eq!(kinds(&later[1]), ["person"]);
}

#[test]
fn sightings_that_never_reach_confirm_never_count() {
    let s = settings(vec![]);
    let mut objects = Objects::new();
    let seen = run(&mut objects, &s, 0, &vec![vec![person_at(0.1, 0.6)]; 8]);
    assert!(seen.iter().all(|p| p.is_empty()));
}

#[test]
fn each_object_keeps_its_own_identity_as_it_moves() {
    let s = settings(vec![]);
    let mut objects = Objects::new();
    let pictures: Vec<Vec<Detection>> = (0..5)
        .map(|i| {
            vec![
                person_at(0.1 + 0.01 * i as f32, 0.9),
                person_at(0.7 - 0.01 * i as f32, 0.9),
            ]
        })
        .collect();
    run(&mut objects, &s, 0, &pictures);
    let mut ids: Vec<u64> = objects.seen().map(|o| o.id).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        vec![1, 2],
        "two people, two ids, kept over five pictures"
    );
}

#[test]
fn an_object_must_stand_in_an_area_for_its_pictures_in_a_row() {
    let s = settings(vec![Zone {
        frames: 3,
        ..area("road", 0.0, 0.5)
    }]);
    let mut objects = Objects::new();
    let seen = run(&mut objects, &s, 0, &vec![vec![person_at(0.1, 0.9)]; 4]);
    assert!(seen[1].is_empty(), "two pictures in the area");
    assert_eq!(kinds(&seen[2]), ["person"], "the third in a row");
    assert_eq!(seen[2][0].zones, vec!["road".to_owned()]);
}

#[test]
fn an_object_must_stay_in_a_loitering_area_for_its_time() {
    let s = settings(vec![Zone {
        loiter_sec: 2.0,
        ..area("door", 0.0, 0.5)
    }]);
    let mut objects = Objects::new();
    let seen = run(&mut objects, &s, 0, &vec![vec![person_at(0.1, 0.9)]; 6]);
    assert!(seen[3].is_empty(), "1.5 s");
    assert_eq!(kinds(&seen[4]), ["person"], "2 s");
}

#[test]
fn with_an_events_area_only_objects_that_entered_one_count_even_after_leaving_it() {
    let s = settings(vec![area("road", 0.0, 0.5)]);
    let mut objects = Objects::new();
    let outside = run(&mut objects, &s, 0, &vec![vec![person_at(0.7, 0.9)]; 4]);
    assert!(outside.iter().all(|p| p.is_empty()), "never in the area");
    // Walks out of the area (it stands at x + 0.025): in on the first three.
    let mut objects = Objects::new();
    let path: Vec<Vec<Detection>> = [0.35, 0.4, 0.45, 0.5, 0.55]
        .iter()
        .map(|x| vec![person_at(*x, 0.9)])
        .collect();
    let crossed = run(&mut objects, &s, 0, &path);
    assert_eq!(
        kinds(&crossed[4]),
        ["person"],
        "entered the area, then left it"
    );
    assert_eq!(objects.seen().count(), 1, "one person all along");
}

#[test]
fn an_area_not_needed_for_events_does_not_hold_them_back() {
    let s = settings(vec![Zone {
        events: false,
        ..area("road", 0.0, 0.5)
    }]);
    let mut objects = Objects::new();
    let seen = run(&mut objects, &s, 0, &vec![vec![person_at(0.7, 0.9)]; 3]);
    assert_eq!(kinds(&seen[2]), ["person"]);
}

#[test]
fn alerts_need_an_alert_class_and_an_alert_area() {
    // Default alert classes: person and car.
    let mut s = settings(vec![Zone {
        events: false,
        ..area("gate", 0.0, 0.3)
    }]);
    s.classes.push(ClassChoice::new("dog", 50));
    let mut objects = Objects::new();
    let dog = |x: f32| found(16, "dog", 0.9, [x, 0.4, 0.1, 0.1]);
    let seen = run(
        &mut objects,
        &s,
        0,
        &[vec![dog(0.1)], vec![dog(0.1)], vec![dog(0.1)]],
    );
    assert_eq!(kinds(&seen[2]), ["dog"]);
    assert!(
        !seen[2][0].alert,
        "a dog: a detection, even in the alert area"
    );
    // A person walks into the alert area (it stands at x + 0.025) and out again.
    let mut objects = Objects::new();
    let path: Vec<Vec<Detection>> = [0.45, 0.4, 0.35, 0.25, 0.3, 0.4]
        .iter()
        .map(|x| vec![person_at(*x, 0.9)])
        .collect();
    let seen = run(&mut objects, &s, 0, &path);
    assert!(!seen[2][0].alert, "outside the alert area: a detection");
    assert!(seen[3][0].alert, "in it: an alert");
    assert!(seen[5][0].alert, "and still one after leaving it");
}

#[test]
fn a_stationary_object_stops_counting_and_counts_again_when_it_moves() {
    let s = DetectSettings {
        stationary_after_sec: 2.0,
        ..settings(vec![])
    };
    let mut objects = Objects::new();
    let parked = run(&mut objects, &s, 0, &vec![vec![car_at(0.4, 0.9)]; 6]);
    assert_eq!(kinds(&parked[2]), ["car"], "1 s still");
    assert!(parked[4].is_empty(), "2 s still: stationary");
    let moving = run(&mut objects, &s, 3000, &[vec![car_at(0.5, 0.9)]]);
    assert_eq!(kinds(&moving[0]), ["car"], "drives off");
}

#[test]
fn a_loitering_area_keeps_a_still_person_but_a_still_car_only_for_its_time() {
    let s = DetectSettings {
        stationary_after_sec: 1.0,
        ..settings(vec![Zone {
            loiter_sec: 2.0,
            ..area("drive", 0.0, 1.0)
        }])
    };
    // Moves for 2.5 s (the loitering time is met at 2 s), then stops for 3 s.
    let path = |at: fn(f32, f32) -> Detection| -> Vec<Vec<Detection>> {
        (0..12)
            .map(|i| vec![at(0.1 + 0.02 * (i.min(5)) as f32, 0.9)])
            .collect()
    };
    let mut objects = Objects::new();
    let person = run(&mut objects, &s, 0, &path(person_at));
    assert!(
        person[3].is_empty(),
        "1.5 s: not yet in the area for its time"
    );
    assert_eq!(kinds(&person[4]), ["person"], "2 s");
    assert_eq!(
        kinds(&person[11]),
        ["person"],
        "standing still in it for 3 s: still there"
    );
    let mut objects = Objects::new();
    let car = run(&mut objects, &s, 0, &path(car_at));
    assert_eq!(kinds(&car[5]), ["car"], "moving after its loitering time");
    assert!(
        car[8].is_empty(),
        "parked for over 1 s after its loitering time: it ends"
    );
}

#[test]
fn an_area_for_cars_only_does_not_count_a_person() {
    let s = settings(vec![Zone {
        classes: vec!["car".into()],
        ..area("road", 0.0, 1.0)
    }]);
    let mut objects = Objects::new();
    let seen = run(
        &mut objects,
        &s,
        0,
        &vec![vec![person_at(0.3, 0.9), car_at(0.5, 0.9)]; 3],
    );
    assert_eq!(kinds(&seen[2]), ["car"]);
}

#[test]
fn ground_speed_comes_from_a_four_corner_areas_side_lengths() {
    // The whole picture is 40 m wide and 10 m deep.
    let road = Zone {
        points: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        sides_m: [40.0, 10.0, 40.0, 10.0],
        ..area("road", 0.0, 1.0)
    };
    let ground = Ground::of(&road).unwrap();
    let corner = ground.map([1.0, 1.0]).unwrap();
    assert!((corner[0] - 40.0).abs() < 1e-3 && (corner[1] - 10.0).abs() < 1e-3);
    // 0.125 of the width (5 m) a picture, 2 pictures a second: 10 m/s = 36 km/h.
    let at = |i: usize| car_at(0.05 + 0.125 * i as f32, 0.9);
    for (min, counts) in [(20.0, true), (50.0, false)] {
        let s = settings(vec![Zone {
            min_speed_kmh: min,
            ..road.clone()
        }]);
        let mut objects = Objects::new();
        let seen = run(
            &mut objects,
            &s,
            0,
            &(0..5).map(|i| vec![at(i)]).collect::<Vec<_>>(),
        );
        assert_eq!(objects.seen().count(), 1);
        assert_eq!(
            !seen[4].is_empty(),
            counts,
            "36 km/h against at least {min}"
        );
    }
    let tilted = Zone {
        points: vec![[0.3, 0.2], [0.7, 0.2], [1.0, 0.9], [0.0, 0.9]],
        sides_m: [10.0, 30.0, 10.0, 30.0],
        ..area("road", 0.0, 1.0)
    };
    let ground = Ground::of(&tilted).unwrap();
    let far = ground.map([0.5, 0.2]).unwrap();
    let near = ground.map([0.5, 0.9]).unwrap();
    assert!((far[0] - 5.0).abs() < 1e-3 && far[1].abs() < 1e-3);
    assert!(
        (near[1] - 30.0).abs() < 1e-3,
        "the far edge to the near edge is 30 m"
    );
    let without = Zone {
        sides_m: [0.0; 4],
        ..tilted
    };
    assert!(Ground::of(&without).is_none(), "no side lengths: no speed");
}

#[test]
fn an_object_not_seen_for_a_while_is_gone() {
    let s = settings(vec![]);
    let mut objects = Objects::new();
    run(
        &mut objects,
        &s,
        0,
        &[vec![person_at(0.1, 0.9)], vec![person_at(0.1, 0.9)]],
    );
    objects.observe(&[], &s, 500 + LOST_MS + 1);
    assert_eq!(objects.objects.len(), 0);
    let again = run(&mut objects, &s, 5000, &[vec![person_at(0.1, 0.9)]]);
    assert!(again[0].is_empty(), "a new object: confirmed afresh");
}
