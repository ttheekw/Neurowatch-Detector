//! What this plugin tells NeuroWatch about itself.
use crate::settings::Kind;
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

pub fn manifest() -> Manifest {
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
        config_schema: Some(config_schema()),
        event_kinds: Kind::ALL
            .into_iter()
            .map(|kind| EventKindInfo {
                id: kind.id().into(),
                label: kind.label().into(),
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
fn config_schema() -> serde_json::Value {
    let point = json!({"type": "array", "items": {"type": "number", "minimum": 0, "maximum": 1},
                       "minItems": 2, "maxItems": 2});
    let decoder_values: Vec<&str> = crate::decoding::choices().iter().map(|c| c.0).collect();
    let decoder_labels: serde_json::Map<String, serde_json::Value> = crate::decoding::choices()
        .iter()
        .map(|(value, label)| ((*value).to_owned(), json!(label)))
        .collect();
    let kinds: Vec<&str> = Kind::ALL.iter().map(|kind| kind.id()).collect();
    let kind_labels: serde_json::Map<String, serde_json::Value> = Kind::ALL
        .iter()
        .map(|kind| (kind.id().to_owned(), json!(kind.plural())))
        .collect();
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "objects": {"type": "array", "uniqueItems": true, "default": ["person", "vehicle"],
                        "items": {"enum": kinds, "x-nw-labels": kind_labels},
                        "title": "Report",
                        "description": "What to look for. Each one is its own event, alert and recording trigger."},
            "confidence": {"type": "integer", "minimum": 10, "maximum": 95, "default": 40,
                           "title": "Confidence (%)",
                           "description": "How sure the detector must be. Higher gives fewer false alarms and can miss small or distant objects."},
            "min_duration_sec": {"type": "number", "minimum": 0, "maximum": 60, "default": 1,
                                 "title": "Minimum duration (seconds)",
                                 "description": "Something seen for less than this is not reported."},
            "cooldown_sec": {"type": "number", "minimum": 0, "maximum": 600, "default": 10,
                             "title": "Ends after (seconds)",
                             "description": "An event ends once nothing of its kind has been seen for this long."},
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
                "description": "Areas to watch or to ignore. An object counts where it stands, at the bottom of its box. With no watched area, the whole picture is watched.",
                "items": {
                    "type": "object", "required": ["id", "points"],
                    "properties": {
                        "id": {"type": "string", "minLength": 1},
                        "name": {"type": "string"},
                        "kind": {"enum": ["include", "exclude"], "default": "include"},
                        "points": {"type": "array", "items": point, "minItems": 3, "maxItems": 64}
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
    fn every_kind_is_an_event_and_a_choice() {
        let manifest = manifest();
        let ids: Vec<&str> = manifest.event_kinds.iter().map(|k| k.id.as_str()).collect();
        assert_eq!(ids, vec!["person", "vehicle", "animal"]);
        let schema = manifest.config_schema.unwrap();
        assert_eq!(
            schema["properties"]["objects"]["items"]["enum"],
            json!(["person", "vehicle", "animal"])
        );
        // Defaults in the form are the defaults the plugin uses.
        let defaults = crate::settings::DetectSettings::default();
        assert_eq!(
            schema["properties"]["confidence"]["default"],
            json!(defaults.confidence)
        );
        assert_eq!(
            schema["properties"]["analysis_fps"]["default"],
            json!(defaults.analysis_fps)
        );
    }
}
