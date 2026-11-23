//! A binding's configuration document: what to look for, how sure the
//! detector must be, how long something must stay, and where to look.
use crate::classes::{ClassList, CONFIDENCE_RANGE, DEFAULT_CONFIDENCE};
use serde::{Deserialize, Serialize};

/// Whether a zone is watched or ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ZoneKind {
    #[default]
    Include,
    Exclude,
}

/// A polygon on the picture, in coordinates normalised to 0..1, so it
/// survives a change of stream resolution. The rules below apply to watched
/// (include) areas; an object standing in an ignored (exclude) area is ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Zone {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: ZoneKind,
    pub points: Vec<[f32; 2]>,
    /// The classes this area applies to; empty: every class.
    #[serde(default)]
    pub classes: Vec<String>,
    /// Pictures in a row an object must stand in it to be in it.
    #[serde(default = "default_frames")]
    pub frames: u8,
    /// Seconds it must stay in it before it counts.
    #[serde(default)]
    pub loiter_sec: f32,
    /// It counts only moving at least this fast (0: any speed; needs `sides_m`).
    #[serde(default)]
    pub min_speed_kmh: f32,
    /// The real lengths of a 4-corner area's sides, in metres, in drawing order.
    #[serde(default)]
    pub sides_m: [f32; 4],
    /// Events need this area: with any such area, an object must have been in
    /// one to raise an event.
    #[serde(default = "yes")]
    pub events: bool,
    /// Alerts need this area: with any such area, an object must have been in
    /// one for its event to be an alert.
    #[serde(default = "yes")]
    pub alerts: bool,
}

fn default_frames() -> u8 {
    3
}

fn yes() -> bool {
    true
}

impl Zone {
    /// At least three points, every one inside the picture.
    pub fn is_valid(&self) -> bool {
        !self.id.is_empty()
            && self.points.len() >= 3
            && self
                .points
                .iter()
                .all(|[x, y]| (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y))
    }

    /// Whether its rules apply to `class` (a class id).
    pub fn applies_to(&self, class: &str) -> bool {
        self.classes.is_empty() || self.classes.iter().any(|c| c == class)
    }

    /// Even-odd test of a normalised point.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let mut inside = false;
        let mut j = self.points.len() - 1;
        for i in 0..self.points.len() {
            let [xi, yi] = self.points[i];
            let [xj, yj] = self.points[j];
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                inside = !inside;
            }
            j = i;
        }
        inside
    }
}

/// One class a binding reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassChoice {
    pub id: String,
    /// A sighting scored below this (percent) is ignored.
    pub confidence: u8,
    /// An object counts once the median of its recent scores (padded to three
    /// with zeros) reaches this, in percent.
    #[serde(default = "default_confirm")]
    pub confirm: u8,
    /// Its box's size, in percent of the picture: smaller or larger is ignored.
    #[serde(default)]
    pub min_size: f32,
    #[serde(default = "default_max_size")]
    pub max_size: f32,
    /// Its box's width divided by its height: narrower or wider is ignored.
    #[serde(default)]
    pub min_ratio: f32,
    #[serde(default = "default_max_ratio")]
    pub max_ratio: f32,
}

/// Where `confirm` starts.
pub const DEFAULT_CONFIRM: u8 = 70;
pub const CONFIRM_RANGE: std::ops::RangeInclusive<u8> = 10..=99;
pub const MAX_RATIO: f32 = 20.0;

fn default_confirm() -> u8 {
    DEFAULT_CONFIRM
}

fn default_max_size() -> f32 {
    100.0
}

fn default_max_ratio() -> f32 {
    MAX_RATIO
}

impl ClassChoice {
    /// `id` at `confidence`, every other rule at its default.
    pub fn new(id: impl Into<String>, confidence: u8) -> Self {
        Self {
            id: id.into(),
            confidence,
            confirm: DEFAULT_CONFIRM,
            min_size: 0.0,
            max_size: default_max_size(),
            min_ratio: 0.0,
            max_ratio: MAX_RATIO,
        }
    }

    /// Whether a box `[x, y, w, h]` (normalised) has an allowed size and shape.
    pub fn fits(&self, rect: [f32; 4]) -> bool {
        let size = rect[2] * rect[3] * 100.0;
        let ratio = if rect[3] > 0.0 {
            rect[2] / rect[3]
        } else {
            f32::INFINITY
        };
        size >= self.min_size
            && size <= self.max_size
            && ratio >= self.min_ratio
            && (self.max_ratio >= MAX_RATIO || ratio <= self.max_ratio)
    }
}

/// Fields left out take their defaults, so a client may send only what it sets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectSettings {
    /// What to report, each class with how sure the detector must be of it
    /// (percent). Each is its own event kind.
    pub classes: Vec<ClassChoice>,
    /// Something seen for less than this is not reported.
    pub min_duration_sec: f32,
    /// An event ends once nothing of its kind has been seen for this long.
    pub cooldown_sec: f32,
    /// Pictures analysed a second.
    pub analysis_fps: u8,
    /// Analyse keyframes only: far less work, and quick visits may be missed.
    pub low_cpu: bool,
    /// What decodes the camera's video (Automatic: the first hardware decoder
    /// that works here, else the CPU).
    pub decoder: crate::decoding::DecoderChoice,
    /// Include and exclude zones. An object counts where it stands: the
    /// middle of its box's bottom edge.
    #[serde(deserialize_with = "deserialize_zones")]
    pub zones: Vec<Zone>,
    /// Events of these classes are alerts (when their areas allow); the
    /// others are detections: recorded and listed, never sending alerts.
    pub alert_classes: Vec<String>,
    /// An object that has not moved for this long is stationary: it no longer
    /// starts or keeps an event (0: never).
    pub stationary_after_sec: f32,
}

impl Default for DetectSettings {
    fn default() -> Self {
        Self {
            classes: vec![],
            alert_classes: vec!["person".into(), "car".into()],
            stationary_after_sec: 10.0,
            min_duration_sec: 1.0,
            cooldown_sec: 10.0,
            analysis_fps: 2,
            low_cpu: false,
            decoder: crate::decoding::DecoderChoice::default(),
            zones: vec![],
        }
    }
}

impl DetectSettings {
    /// The classes a model's class file marks as chosen on a new camera.
    pub fn default_classes(classes: &ClassList) -> Vec<ClassChoice> {
        classes
            .iter()
            .filter(|class| class.default)
            .map(|class| ClassChoice::new(class.id.to_string(), class.confidence))
            .collect()
    }

    /// The chosen class `id`, if it is chosen.
    pub fn class(&self, id: &str) -> Option<&ClassChoice> {
        self.classes.iter().find(|choice| choice.id == id)
    }

    /// Whether an object standing at `(x, y)` is in an ignored area.
    pub fn ignored(&self, x: f32, y: f32) -> bool {
        self.zones
            .iter()
            .any(|zone| zone.is_valid() && zone.kind == ZoneKind::Exclude && zone.contains(x, y))
    }

    /// The valid watched areas.
    pub fn watched_zones(&self) -> impl Iterator<Item = &Zone> {
        self.zones
            .iter()
            .filter(|zone| zone.is_valid() && zone.kind == ZoneKind::Include)
    }

    /// The confidence chosen for a class, if it is chosen.
    pub fn confidence_of(&self, id: &str) -> Option<u8> {
        self.classes
            .iter()
            .find(|choice| choice.id == id)
            .map(|choice| choice.confidence)
    }

    /// The lowest chosen confidence, as a score (0..1).
    pub fn lowest_score(&self) -> Option<f32> {
        self.classes
            .iter()
            .map(|choice| choice.confidence)
            .min()
            .map(|percent| f32::from(percent) / 100.0)
    }

    /// Each model class's score threshold, `None` for one not chosen.
    pub fn thresholds(&self, classes: &ClassList) -> Vec<Option<f32>> {
        let mut thresholds = vec![None; classes.len()];
        for choice in &self.classes {
            if let Some(index) = classes.index_of(&choice.id) {
                thresholds[index] = Some(f32::from(choice.confidence) / 100.0);
            }
        }
        thresholds
    }

    /// Whether an object standing at `(x, y)` (normalised) is in a watched place.
    pub fn watched(&self, x: f32, y: f32) -> bool {
        let zones: Vec<&Zone> = self.zones.iter().filter(|zone| zone.is_valid()).collect();
        let includes = zones.iter().any(|zone| zone.kind == ZoneKind::Include);
        (!includes
            || zones
                .iter()
                .any(|zone| zone.kind == ZoneKind::Include && zone.contains(x, y)))
            && !zones
                .iter()
                .any(|zone| zone.kind == ZoneKind::Exclude && zone.contains(x, y))
    }

    /// The watched zones an object standing at `(x, y)` is in.
    pub fn zones_at(&self, x: f32, y: f32) -> Vec<String> {
        self.zones
            .iter()
            .filter(|zone| zone.is_valid() && zone.kind == ZoneKind::Include && zone.contains(x, y))
            .map(|zone| zone.id.clone())
            .collect()
    }
}

fn deserialize_zones<'de, D>(deserializer: D) -> Result<Vec<Zone>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .filter_map(|value| serde_json::from_value::<Zone>(value).ok())
        .filter(Zone::is_valid)
        .collect())
}

/// What a v0.1.1 binding saved (`objects` and one `confidence`) as the
/// classes it means, keeping the ones this model has.
fn legacy_classes(value: &serde_json::Value, classes: &ClassList) -> Vec<serde_json::Value> {
    const VEHICLE: [&str; 5] = ["bicycle", "car", "motorcycle", "bus", "truck"];
    const ANIMAL: [&str; 10] = [
        "bird", "cat", "dog", "horse", "sheep", "cow", "elephant", "bear", "zebra", "giraffe",
    ];
    let confidence = value
        .get("confidence")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(40);
    let mut ids: Vec<&str> = Vec::new();
    for object in value
        .get("objects")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
    {
        match object {
            "person" => ids.push("person"),
            "vehicle" => ids.extend(VEHICLE),
            "animal" => ids.extend(ANIMAL),
            _ => {}
        }
    }
    let mut seen = std::collections::HashSet::new();
    ids.into_iter()
        .filter(|id| classes.find(id).is_some() && seen.insert(*id))
        .map(|id| serde_json::json!({"id": id, "confidence": confidence}))
        .collect()
}

/// Check a configuration document against the loaded model's classes.
pub fn validate(value: &serde_json::Value, classes: &ClassList) -> Result<DetectSettings, String> {
    let Some(object) = value.as_object() else {
        return Err("Detection settings must be an object".into());
    };
    let mut document = object.clone();
    match document.get_mut("classes") {
        Some(serde_json::Value::Array(items)) => {
            // An item without a confidence starts at its class's own.
            for item in items.iter_mut() {
                if let Some(item) = item.as_object_mut() {
                    if !item.contains_key("confidence") {
                        let start = item
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .and_then(|id| classes.find(id))
                            .map_or(DEFAULT_CONFIDENCE, |class| class.confidence);
                        item.insert("confidence".into(), start.into());
                    }
                }
            }
        }
        Some(_) => return Err("Invalid detection settings".into()),
        None if object.contains_key("objects") => {
            document.insert(
                "classes".into(),
                serde_json::Value::Array(legacy_classes(value, classes)),
            );
        }
        None => {
            document.insert(
                "classes".into(),
                serde_json::to_value(DetectSettings::default_classes(classes))
                    .map_err(|_| "Invalid detection settings".to_owned())?,
            );
        }
    }
    if !object.contains_key("alert_classes") {
        // The default alert classes this model has.
        let defaults: Vec<serde_json::Value> = DetectSettings::default()
            .alert_classes
            .into_iter()
            .filter(|id| classes.find(id).is_some())
            .map(serde_json::Value::from)
            .collect();
        document.insert("alert_classes".into(), serde_json::Value::Array(defaults));
    }
    let settings: DetectSettings = serde_json::from_value(serde_json::Value::Object(document))
        .map_err(|_| "Invalid detection settings".to_owned())?;
    let mut ids = std::collections::HashSet::new();
    for choice in &settings.classes {
        if classes.find(&choice.id).is_none() {
            return Err(format!("Unknown class {:?}", choice.id));
        }
        if !ids.insert(choice.id.as_str()) {
            return Err(format!("Class {:?} is chosen twice", choice.id));
        }
        if !CONFIDENCE_RANGE.contains(&choice.confidence) {
            return Err("Confidence must be 10–95% for every class".into());
        }
        let numbers = [
            choice.min_size,
            choice.max_size,
            choice.min_ratio,
            choice.max_ratio,
        ];
        if !CONFIRM_RANGE.contains(&choice.confirm)
            || numbers.iter().any(|n| !n.is_finite())
            || !(0.0..=100.0).contains(&choice.min_size)
            || !(0.0..=100.0).contains(&choice.max_size)
            || choice.min_size > choice.max_size
            || !(0.0..=MAX_RATIO).contains(&choice.min_ratio)
            || !(0.0..=MAX_RATIO).contains(&choice.max_ratio)
            || choice.min_ratio > choice.max_ratio
        {
            return Err(format!(
                "{}: confirmed at 10–99%, size 0–100% and width ÷ height 0–20, each minimum at most its maximum",
                choice.id
            ));
        }
    }
    for id in &settings.alert_classes {
        if classes.find(id).is_none() {
            return Err(format!("Unknown alert class {id:?}"));
        }
    }
    if !settings.stationary_after_sec.is_finite()
        || !(0.0..=600.0).contains(&settings.stationary_after_sec)
    {
        return Err("Stationary after must be 0–600 seconds".into());
    }
    if !(1..=10).contains(&settings.analysis_fps)
        || !settings.cooldown_sec.is_finite()
        || !(0.0..=600.0).contains(&settings.cooldown_sec)
        || !settings.min_duration_sec.is_finite()
        || !(0.0..=60.0).contains(&settings.min_duration_sec)
    {
        return Err("Analysis must be 1–10 pictures a second, ends after 0–600 seconds and minimum duration 0–60 seconds".into());
    }
    if let Some(raw) = value.get("zones") {
        let zones: Vec<Zone> =
            serde_json::from_value(raw.clone()).map_err(|_| "Invalid zones".to_owned())?;
        let mut ids = std::collections::HashSet::new();
        if zones.len() > 32
            || zones
                .iter()
                .any(|zone| !zone.is_valid() || zone.points.len() > 64 || !ids.insert(&zone.id))
        {
            return Err(
                "Use at most 32 uniquely named zones, each with 3–64 points inside the picture"
                    .into(),
            );
        }
        for zone in &zones {
            if !(1..=30).contains(&zone.frames)
                || !zone.loiter_sec.is_finite()
                || !(0.0..=3600.0).contains(&zone.loiter_sec)
                || !zone.min_speed_kmh.is_finite()
                || !(0.0..=300.0).contains(&zone.min_speed_kmh)
                || zone
                    .sides_m
                    .iter()
                    .any(|s| !s.is_finite() || *s < 0.0 || *s > 10_000.0)
            {
                return Err(format!(
                    "{}: 1–30 pictures in a row, loitering 0–3600 seconds, speed 0–300 km/h and side lengths 0–10000 m",
                    zone.id
                ));
            }
            if let Some(unknown) = zone.classes.iter().find(|id| classes.find(id).is_none()) {
                return Err(format!("{}: unknown class {unknown:?}", zone.id));
            }
        }
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn classes() -> ClassList {
        ClassList::built_in().unwrap()
    }

    fn ids(settings: &DetectSettings) -> Vec<(&str, u8)> {
        settings
            .classes
            .iter()
            .map(|c| (c.id.as_str(), c.confidence))
            .collect()
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let settings = validate(&json!({}), &classes()).unwrap();
        assert_eq!(ids(&settings), vec![("person", 50), ("car", 50)]);
        assert_eq!(
            DetectSettings {
                classes: vec![],
                ..settings
            },
            DetectSettings::default()
        );
    }

    #[test]
    fn chosen_classes_and_numbers_are_kept_and_checked() {
        let settings = validate(
            &json!({"classes": [{"id": "dog", "confidence": 70}, {"id": "cell_phone"}],
                    "analysis_fps": 4}),
            &classes(),
        )
        .unwrap();
        assert_eq!(ids(&settings), vec![("dog", 70), ("cell_phone", 50)]);
        assert_eq!(settings.confidence_of("dog"), Some(70));
        assert_eq!(settings.lowest_score(), Some(0.5));
        let bad = |value: serde_json::Value| validate(&value, &classes()).is_err();
        assert!(bad(json!({"classes": [{"id": "dog", "confidence": 5}]})));
        assert!(bad(json!({"classes": [{"id": "dog", "confidence": 96}]})));
        assert!(bad(json!({"classes": [{"id": "dragon"}]})));
        assert!(bad(json!({"classes": [{"id": "dog"}, {"id": "dog"}]})));
        assert!(bad(json!({"analysis_fps": 0})));
        assert!(!bad(json!({"classes": []})));
    }

    #[test]
    fn a_class_starts_at_its_own_confidence() {
        let list = ClassList::parse(
            r#"{"name": "T", "classes": [{"id": "a", "label": "A", "confidence": 30}]}"#,
        )
        .unwrap();
        let settings = validate(&json!({"classes": [{"id": "a"}]}), &list).unwrap();
        assert_eq!(ids(&settings), vec![("a", 30)]);
    }

    #[test]
    fn a_v0_1_1_binding_keeps_what_it_looked_for() {
        let settings = validate(
            &json!({"objects": ["person", "vehicle", "animal"], "confidence": 65}),
            &classes(),
        )
        .unwrap();
        let found: Vec<&str> = settings.classes.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            found,
            vec![
                "person",
                "bicycle",
                "car",
                "motorcycle",
                "bus",
                "truck",
                "bird",
                "cat",
                "dog",
                "horse",
                "sheep",
                "cow",
                "elephant",
                "bear",
                "zebra",
                "giraffe"
            ]
        );
        assert!(settings.classes.iter().all(|c| c.confidence == 65));
        let only = validate(&json!({"objects": ["animal"]}), &classes()).unwrap();
        assert!(only.classes.iter().all(|c| c.confidence == 40));
        // `classes` wins when both are there; no objects means none chosen.
        let both = validate(
            &json!({"objects": ["person"], "classes": [{"id": "dog"}]}),
            &classes(),
        )
        .unwrap();
        assert_eq!(ids(&both), vec![("dog", 50)]);
        assert!(validate(&json!({"objects": []}), &classes())
            .unwrap()
            .classes
            .is_empty());
    }

    #[test]
    fn zones_decide_where_an_object_counts() {
        let square = |id: &str, kind: &str, x0: f32, x1: f32| json!({"id": id, "kind": kind, "points": [[x0, 0.0], [x1, 0.0], [x1, 1.0], [x0, 1.0]]});
        let settings = validate(&json!({"zones": [square("drive", "include", 0.0, 0.5), square("road", "exclude", 0.4, 0.5)]}), &classes()).unwrap();
        assert!(settings.watched(0.2, 0.9));
        assert!(!settings.watched(0.45, 0.9), "inside the ignored area");
        assert!(!settings.watched(0.8, 0.9), "outside every watched zone");
        assert_eq!(settings.zones_at(0.2, 0.9), vec!["drive".to_owned()]);
        assert!(
            DetectSettings::default().watched(0.9, 0.9),
            "no zones: everywhere"
        );
        assert!(validate(
            &json!({"zones": [{"id": "x", "points": [[0, 0], [2, 0], [0, 1]]}]}),
            &classes()
        )
        .is_err());
    }
}
