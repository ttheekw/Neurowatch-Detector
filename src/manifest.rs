//! What this plugin tells NeuroWatch about itself.
use crate::classes::ClassList;
use nw_analytics_protocol::{
    messages::{
        Codec, EventKindInfo, Manifest, MediaRequest, OverlayLayer, StreamChoice, VideoRequest,
    },
    PROTOCOL_VERSION,
};
use serde_json::json;

pub const PLUGIN_ID: &str = "nw.detect";
/// A box around each object reported.
pub const OBJECTS: &str = "objects";
/// The watched and ignored zones.
pub const ZONES: &str = "zones";

/// The manifest of the process's model.
pub fn manifest() -> Manifest {
    manifest_for(&crate::detector::shared_classes())
}

/// The manifest of a plugin running a model with these classes.
pub fn manifest_for(classes: &ClassList) -> Manifest {
    Manifest {
        id: PLUGIN_ID.into(),
        name: "Object detection".into(),
        vendor: "NeuroWatch".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol: PROTOCOL_VERSION,
        ping: true,
        locality_proof: None,
        identity: None,
        web_ui: None,
        permissions: vec![
            "video".into(),
            "events".into(),
            "overlays".into(),
            "decoded_frames".into(),
        ],
        media: MediaRequest {
            decoded: Some(nw_analytics_protocol::decoded::DecodedRequest {
                formats: vec![
                    nw_analytics_protocol::decoded::RawFormat::Rgb24,
                    nw_analytics_protocol::decoded::RawFormat::Nv12,
                ],
                max_fps: 10,
                min_width: 416,
                min_height: 234,
            }),
            video: VideoRequest {
                // The model looks at 416×416: the sub stream is enough.
                stream: StreamChoice::Sub,
                codecs: vec![Codec::H264, Codec::H265],
                keyframes_only: false,
                delivery: nw_analytics_protocol::messages::DeliveryMode::Latest,
                max_buffer_ms: 1000,
            },
        },
        config_schema: Some(config_schema(classes)),
        event_kinds: classes
            .iter()
            .map(|class| EventKindInfo {
                id: class.id.to_string(),
                label: class.label.to_string(),
            })
            .collect(),
        overlay_layers: vec![
            OverlayLayer {
                id: OBJECTS.into(),
                label: "Object boxes".into(),
                default_on: true,
            },
            OverlayLayer {
                id: ZONES.into(),
                label: "Watched and ignored areas".into(),
                default_on: false,
            },
        ],
    }
}

/// JSON Schema of a binding's settings (`crate::settings::DetectSettings`).
fn config_schema(classes: &ClassList) -> serde_json::Value {
    let point = json!({"type": "array", "items": {"type": "number", "minimum": 0, "maximum": 1},
                       "minItems": 2, "maxItems": 2});
    let decoder_values: Vec<&str> = crate::decoding::choices().iter().map(|c| c.0).collect();
    let decoder_labels: serde_json::Map<String, serde_json::Value> = crate::decoding::choices()
        .iter()
        .map(|(value, label)| ((*value).to_owned(), json!(label)))
        .collect();
    let ids: Vec<&str> = classes.iter().map(|class| &*class.id).collect();
    let labels: serde_json::Map<String, serde_json::Value> = classes
        .iter()
        .map(|class| (class.id.to_string(), json!(&*class.label)))
        .collect();
    let starts: serde_json::Map<String, serde_json::Value> = classes
        .iter()
        .map(|class| (class.id.to_string(), json!(class.confidence)))
        .collect();
    let defaults = crate::settings::DetectSettings::default_classes(classes);
    let alert_defaults: Vec<String> = crate::settings::DetectSettings::default()
        .alert_classes
        .into_iter()
        .filter(|id| classes.find(id).is_some())
        .collect();
    let class_list = json!({"type": "array", "uniqueItems": true,
                            "items": {"enum": ids, "x-nw-labels": labels}});
    let include = json!(["include"]);
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        // The order the settings form shows them in (JSON objects lose theirs).
        "x-nw-order": ["classes", "alert_classes", "min_duration_sec", "cooldown_sec",
                       "stationary_after_sec", "zones", "analysis_fps", "low_cpu", "decoder"],
        "properties": {
            "classes": {
                "type": "array", "x-nw-widget": "nw:classes", "uniqueItems": true,
                "maxItems": classes.len().max(1),
                "title": "Detect",
                "description": "What to look for, and how sure the detector must be of each. Each one is its own event, alert and recording trigger.",
                // Only what a new camera chooses: every other per-class rule
                // takes its own field's default.
                "default": defaults
                    .iter()
                    .map(|c| json!({"id": c.id, "confidence": c.confidence}))
                    .collect::<Vec<_>>(),
                "items": {
                    "type": "object", "required": ["id"],
                    "x-nw-order": ["confidence", "confirm", "min_size", "max_size", "min_ratio", "max_ratio"],
                    "properties": {
                        "id": {"enum": ids, "x-nw-labels": labels},
                        "confidence": {
                            "type": "integer", "minimum": 10, "maximum": 95,
                            "default": crate::classes::DEFAULT_CONFIDENCE,
                            "title": "Confidence (%)",
                            "description": "A single sighting scored below this is ignored.",
                            "x-nw-defaults": starts
                        },
                        "confirm": {
                            "type": "integer", "minimum": 10, "maximum": 99,
                            "default": crate::settings::DEFAULT_CONFIRM,
                            "title": "Confirmed at (%)",
                            "description": "An object counts once the middle of its recent scores reaches this (counting missing scores as zero until there are three), so one lucky sighting cannot start an event and one weak sighting cannot end one."
                        },
                        "min_size": {
                            "type": "number", "minimum": 0, "maximum": 100, "default": 0,
                            "title": "Smallest (% of picture)",
                            "description": "Boxes smaller than this share of the picture are ignored, such as a 'person' that is really a distant sign. 0: no limit."
                        },
                        "max_size": {
                            "type": "number", "minimum": 0, "maximum": 100, "default": 100,
                            "title": "Largest (% of picture)",
                            "description": "Boxes larger than this share of the picture are ignored, such as a 'person' the size of a car. 100: no limit."
                        },
                        "min_ratio": {
                            "type": "number", "minimum": 0, "maximum": 20, "default": 0,
                            "title": "Narrowest (width ÷ height)",
                            "description": "The box's width divided by its height: 1 is square, 0.5 tall and narrow (a standing person). Narrower boxes are ignored. 0: no limit."
                        },
                        "max_ratio": {
                            "type": "number", "minimum": 0, "maximum": 20, "default": 20,
                            "title": "Widest (width ÷ height)",
                            "description": "The box's width divided by its height: 2 is wide and flat (a car from the side). Wider boxes are ignored. 20: no limit."
                        }
                    }
                }
            },
            "min_duration_sec": {"type": "number", "minimum": 0, "maximum": 60, "default": 1,
                                 "title": "Minimum duration (seconds)",
                                 "description": "An object counting for less than this is not reported."},
            "cooldown_sec": {"type": "number", "minimum": 0, "maximum": 600, "default": 10,
                             "title": "Ends after (seconds)",
                             "description": "An object's event ends once it has not counted for this long (it left, stopped or went out of the areas)."},
            "alert_classes": {
                "type": "array", "uniqueItems": true, "default": alert_defaults,
                "items": {"enum": ids, "x-nw-labels": labels},
                "title": "Alerts for",
                "description": "Events of these kinds are alerts and can send notifications. The others are detections: recorded and listed, never sending alerts. Watched areas can also require an object to have been in them for an alert."
            },
            "stationary_after_sec": {
                "type": "number", "minimum": 0, "maximum": 600, "default": 10,
                "title": "Stationary after (seconds)",
                "description": "An object that has not moved for this long, such as a parked car, no longer starts or keeps an event; it counts again once it moves. In a watched area with a loitering time a person still keeps the event while there, anything else until that time has passed. 0: never stationary."
            },
            "analysis_fps": {"type": "integer", "minimum": 1, "maximum": 10, "default": 2,
                             "title": "Pictures analysed per second",
                             "description": "More catches quick visits and uses more processing. Two is enough for people walking past."},
            "low_cpu": {"type": "boolean", "default": false, "title": "Keyframes only (low CPU)",
                        "description": "Looks at about one picture every one or two seconds. Uses far less processing, and can miss quick visits."},
            "decoder": {
                "enum": decoder_values, "default": "auto", "x-nw-labels": decoder_labels,
                "x-nw-widget": "nw:decoder",
                "title": "Video decoder",
                "description": "What decodes this camera's video. Automatic and hardware decoders leave more of the processor free; a choice that cannot be used here falls back to the CPU, and the status says so."
            },
            "zones": {
                "type": "array", "maxItems": 32, "x-nw-widget": "nw:zones",
                "title": "Watched areas",
                "description": "Areas to watch or to ignore. An object is where it stands, at the middle of its box's bottom edge. An object standing in an ignored area is ignored. Objects outside the watched areas are still followed and shown greyed; each watched area's own settings decide what counts.",
                "items": {
                    "type": "object", "required": ["id", "points"],
                    "x-nw-order": ["classes", "frames", "loiter_sec", "min_speed_kmh", "sides_m",
                                   "events", "alerts"],
                    "properties": {
                        "id": {"type": "string", "minLength": 1},
                        "name": {"type": "string"},
                        "kind": {"enum": ["include", "exclude"], "default": "include"},
                        "points": {"type": "array", "items": point, "minItems": 3, "maxItems": 64},
                        "classes": {
                            "type": "array", "uniqueItems": true, "default": [],
                            "items": class_list["items"].clone(), "x-nw-zone-kinds": include,
                            "title": "Applies to",
                            "description": "The kinds of object this area counts. None chosen: every kind."
                        },
                        "frames": {
                            "type": "integer", "minimum": 1, "maximum": 30, "default": 3,
                            "x-nw-zone-kinds": include,
                            "title": "Pictures in a row",
                            "description": "An object is in the area once it has stood in it on this many analysed pictures in a row, so a box that jitters over the edge does not count."
                        },
                        "loiter_sec": {
                            "type": "number", "minimum": 0, "maximum": 3600, "default": 0,
                            "x-nw-zone-kinds": include,
                            "title": "Loitering time (seconds)",
                            "description": "An object must stay in the area this long before it counts. While it stays, a person keeps its event going even standing still. 0: at once."
                        },
                        "min_speed_kmh": {
                            "type": "number", "minimum": 0, "maximum": 300, "default": 0,
                            "x-nw-zone-kinds": include,
                            "title": "Minimum speed (km/h)",
                            "description": "Only an object moving at least this fast counts in the area. Needs a 4-corner area on the ground with its side lengths below. 0: any speed."
                        },
                        "sides_m": {
                            "type": "array", "minItems": 4, "maxItems": 4, "default": [0, 0, 0, 0],
                            "items": {"type": "number", "minimum": 0, "maximum": 10000},
                            "x-nw-zone-kinds": include,
                            "title": "Side lengths (metres)",
                            "description": "For a 4-corner area drawn on the ground: the real length of each side, in the order the corners were drawn (first to second corner, second to third, and so on). Used to measure speed."
                        },
                        "events": {
                            "type": "boolean", "default": true, "x-nw-zone-kinds": include,
                            "title": "Events need this area",
                            "description": "With any area marked so, an object raises an event only after it has been in one of them."
                        },
                        "alerts": {
                            "type": "boolean", "default": true, "x-nw-zone-kinds": include,
                            "title": "Alerts need this area",
                            "description": "With any area marked so, an event is an alert only once an object of an alert kind has been in one of them; until then it is a detection."
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_event_rules_are_settings_with_their_meaning_in_the_form() {
        let classes = crate::classes::ClassList::built_in().unwrap();
        let schema = config_schema(&classes);
        let props = &schema["properties"];
        let per_class = &props["classes"]["items"]["properties"];
        for key in ["confirm", "min_size", "max_size", "min_ratio", "max_ratio"] {
            assert!(
                per_class[key]["description"]
                    .as_str()
                    .is_some_and(|d| d.len() > 20),
                "{key}"
            );
        }
        assert_eq!(per_class["confirm"]["default"], 70);
        let per_area = &props["zones"]["items"]["properties"];
        for key in [
            "classes",
            "frames",
            "loiter_sec",
            "min_speed_kmh",
            "sides_m",
            "events",
            "alerts",
        ] {
            assert_eq!(
                per_area[key]["x-nw-zone-kinds"],
                serde_json::json!(["include"]),
                "{key}"
            );
            assert!(
                per_area[key]["description"]
                    .as_str()
                    .is_some_and(|d| d.len() > 20),
                "{key}"
            );
        }
        assert_eq!(per_area["frames"]["default"], 3);
        assert_eq!(
            (
                per_area["sides_m"]["minItems"].clone(),
                per_area["sides_m"]["maxItems"].clone()
            ),
            (serde_json::json!(4), serde_json::json!(4))
        );
        assert_eq!(
            props["alert_classes"]["default"],
            serde_json::json!(["person", "car"])
        );
        assert_eq!(props["stationary_after_sec"]["default"], 10);
        // The form's defaults are what a binding gets when it leaves them out.
        let settings = crate::settings::validate(&serde_json::json!({}), &classes).unwrap();
        assert_eq!(settings.classes[0].confirm, 70);
        assert_eq!(settings.alert_classes, ["person", "car"]);
    }

    /// Documents CURRENT behaviour, not a decided one: Detector asks for a
    /// minimum picture size (416x234), so NeuroWatch may move it to another
    /// of the camera's desktop streams (e.g. the main stream) when its own
    /// stalls. Motion asks for 0x0 and never moves (its manifest test). Changing
    /// Detector's request is a product decision, pending: when it is taken,
    /// change this test with it.
    #[test]
    fn detector_currently_permits_a_larger_desktop_stream_by_its_minimum_size() {
        let decoded = manifest()
            .media
            .decoded
            .expect("uses NeuroWatch's pictures");
        assert_eq!((decoded.min_width, decoded.min_height), (416, 234));
        assert_eq!(manifest().media.video.stream, StreamChoice::Sub);
    }

    #[test]
    fn every_class_is_an_event_and_a_choice() {
        let manifest = manifest();
        assert_eq!(manifest.event_kinds.len(), 80);
        assert_eq!(manifest.event_kinds[0].id, "person");
        assert_eq!(manifest.event_kinds[9].id, "traffic_light");
        assert_eq!(manifest.event_kinds[9].label, "Traffic light");
        let schema = manifest.config_schema.unwrap();
        let classes = &schema["properties"]["classes"];
        assert_eq!(classes["x-nw-widget"], "nw:classes");
        assert_eq!(
            classes["items"]["properties"]["id"]["enum"][79],
            "toothbrush"
        );
        assert_eq!(
            classes["items"]["properties"]["id"]["x-nw-labels"]["fire_hydrant"],
            "Fire hydrant"
        );
        assert_eq!(
            classes["default"],
            json!([{"id": "person", "confidence": 50}, {"id": "car", "confidence": 50}])
        );
        assert_eq!(
            classes["items"]["properties"]["confidence"]["x-nw-defaults"]["dog"],
            50
        );
        assert!(schema["properties"].get("objects").is_none());
        assert!(schema["properties"].get("confidence").is_none());
        // Defaults in the form are the defaults the plugin uses.
        let defaults = crate::settings::DetectSettings::default();
        assert_eq!(
            schema["properties"]["analysis_fps"]["default"],
            json!(defaults.analysis_fps)
        );
    }

    #[test]
    fn another_models_classes_are_what_is_offered() {
        let list = ClassList::parse(
            r#"{"name": "T", "classes": [{"id": "ball", "label": "Ball", "default": true, "confidence": 30}]}"#,
        )
        .unwrap();
        let manifest = manifest_for(&list);
        assert_eq!(manifest.event_kinds.len(), 1);
        let schema = manifest.config_schema.unwrap();
        assert_eq!(
            schema["properties"]["classes"]["default"],
            json!([{"id": "ball", "confidence": 30}])
        );
    }
}
