//! A binding's configuration document: what to look for, how sure the
//! detector must be, how long something must stay, and where to look.
use serde::{Deserialize, Serialize};

/// What the detector reports. Each is its own NeuroWatch event kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Person,
    Vehicle,
    Animal,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Person, Kind::Vehicle, Kind::Animal];

    /// The NeuroWatch event kind (and the settings' value).
    pub fn id(self) -> &'static str {
        match self {
            Kind::Person => "person",
            Kind::Vehicle => "vehicle",
            Kind::Animal => "animal",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Person => "Person",
            Kind::Vehicle => "Vehicle",
            Kind::Animal => "Animal",
        }
    }

    /// The figure's label: how many are in view now.
    pub fn plural(self) -> &'static str {
        match self {
            Kind::Person => "People",
            Kind::Vehicle => "Vehicles",
            Kind::Animal => "Animals",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Kind::Person => 0,
            Kind::Vehicle => 1,
            Kind::Animal => 2,
        }
    }
}

/// Whether a zone is watched or ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ZoneKind {
    #[default]
    Include,
    Exclude,
}

/// A polygon on the picture, in coordinates normalised to 0..1, so it
/// survives a change of stream resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Zone {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: ZoneKind,
    pub points: Vec<[f32; 2]>,
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

/// Fields left out take their defaults, so a client may send only what it sets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectSettings {
    /// What to report.
    pub objects: Vec<Kind>,
    /// How sure the detector must be, in percent.
    pub confidence: u8,
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
}

impl Default for DetectSettings {
    fn default() -> Self {
        Self {
            objects: vec![Kind::Person, Kind::Vehicle],
            confidence: 40,
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
    pub fn wants(&self, kind: Kind) -> bool {
        self.objects.contains(&kind)
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

pub fn validate(value: &serde_json::Value) -> Result<DetectSettings, String> {
    if !value.is_object() {
        return Err("Detection settings must be an object".into());
    }
    let settings: DetectSettings = serde_json::from_value(value.clone())
        .map_err(|_| "Invalid detection settings".to_owned())?;
    if !(10..=95).contains(&settings.confidence)
        || !(1..=10).contains(&settings.analysis_fps)
        || !settings.cooldown_sec.is_finite()
        || !(0.0..=600.0).contains(&settings.cooldown_sec)
        || !settings.min_duration_sec.is_finite()
        || !(0.0..=60.0).contains(&settings.min_duration_sec)
    {
        return Err("Confidence must be 10–95%, analysis 1–10 pictures a second, ends after 0–600 seconds and minimum duration 0–60 seconds".into());
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
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_fields_take_their_defaults() {
        let settings = validate(&json!({})).unwrap();
        assert_eq!(settings, DetectSettings::default());
        assert!(settings.wants(Kind::Person) && !settings.wants(Kind::Animal));
    }

    #[test]
    fn chosen_objects_and_numbers_are_kept_and_checked() {
        let settings =
            validate(&json!({"objects": ["animal"], "confidence": 70, "analysis_fps": 4})).unwrap();
        assert_eq!(settings.objects, vec![Kind::Animal]);
        assert_eq!(settings.confidence, 70);
        assert!(validate(&json!({"confidence": 5})).is_err());
        assert!(validate(&json!({"analysis_fps": 0})).is_err());
        assert!(validate(&json!({"objects": ["dragon"]})).is_err());
    }

    #[test]
    fn zones_decide_where_an_object_counts() {
        let square = |id: &str, kind: &str, x0: f32, x1: f32| json!({"id": id, "kind": kind, "points": [[x0, 0.0], [x1, 0.0], [x1, 1.0], [x0, 1.0]]});
        let settings = validate(&json!({"zones": [square("drive", "include", 0.0, 0.5), square("road", "exclude", 0.4, 0.5)]})).unwrap();
        assert!(settings.watched(0.2, 0.9));
        assert!(!settings.watched(0.45, 0.9), "inside the ignored area");
        assert!(!settings.watched(0.8, 0.9), "outside every watched zone");
        assert_eq!(settings.zones_at(0.2, 0.9), vec!["drive".to_owned()]);
        assert!(
            DetectSettings::default().watched(0.9, 0.9),
            "no zones: everywhere"
        );
        assert!(
            validate(&json!({"zones": [{"id": "x", "points": [[0, 0], [2, 0], [0, 1]]}]})).is_err()
        );
    }
}
