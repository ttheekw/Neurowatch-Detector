//! What the plugin draws over the camera's picture: a box around each object
//! it reports (`objects`), and the zones it watches (`zones`). Figures beside
//! the picture count what is in view now.
use crate::detector::Detection;
use crate::settings::{DetectSettings, Kind, ZoneKind};
use nw_analytics_protocol::messages::{Figure, OverlayItem, Style};

/// The colour of a kind: on its boxes and on its figure, which is the key.
pub fn style(kind: Kind) -> Style {
    match kind {
        Kind::Person => Style::Series1,
        Kind::Vehicle => Style::Series2,
        Kind::Animal => Style::Series3,
    }
}

pub fn objects(detections: &[Detection]) -> Vec<OverlayItem> {
    detections
        .iter()
        .map(|detection| OverlayItem::Box {
            rect: detection.rect,
            label: Some(format!(
                "{} {:.0}%",
                detection.class,
                detection.score * 100.0
            )),
            style: style(detection.kind),
            fill: false,
        })
        .collect()
}

pub fn zones(settings: &DetectSettings) -> Vec<OverlayItem> {
    settings
        .zones
        .iter()
        .filter(|zone| zone.is_valid())
        .map(|zone| {
            let watched = zone.kind == ZoneKind::Include;
            OverlayItem::Polygon {
                points: zone.points.clone(),
                label: Some(if zone.name.is_empty() {
                    zone.id.clone()
                } else {
                    zone.name.clone()
                }),
                style: if watched {
                    Style::Accent
                } else {
                    Style::Danger
                },
                fill: !watched,
            }
        })
        .collect()
}

/// How many of each chosen kind are in view now.
pub fn figures(detections: &[Detection], settings: &DetectSettings) -> Vec<Figure> {
    Kind::ALL
        .into_iter()
        .filter(|kind| settings.wants(*kind))
        .map(|kind| Figure {
            label: kind.plural().into(),
            value: detections
                .iter()
                .filter(|d| d.kind == kind)
                .count()
                .to_string(),
            icon: match kind {
                Kind::Person => Some("person".into()),
                Kind::Vehicle => Some("car".into()),
                Kind::Animal => None,
            },
            style: Some(style(kind)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_object_gets_a_labelled_box_in_its_kinds_colour() {
        let items = objects(&[Detection {
            kind: Kind::Vehicle,
            class: "car",
            score: 0.87,
            rect: [0.1, 0.2, 0.3, 0.2],
        }]);
        match &items[0] {
            OverlayItem::Box { label, style, .. } => {
                assert_eq!(label.as_deref(), Some("car 87%"));
                assert_eq!(*style, Style::Series2);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn figures_count_only_the_chosen_kinds() {
        let figures = figures(&[], &DetectSettings::default());
        let labels: Vec<&str> = figures.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, vec!["People", "Vehicles"]);
        assert!(figures.iter().all(|f| f.value == "0"));
    }
}
