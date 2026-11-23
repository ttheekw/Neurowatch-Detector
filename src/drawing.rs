//! What the plugin draws over the camera's picture: a box around each object
//! it follows (`objects`), and the zones it watches (`zones`).
use crate::objects::Objects;
use crate::settings::{DetectSettings, ZoneKind};
use nw_analytics_protocol::messages::{OverlayItem, Style};

const SERIES: [Style; 8] = [
    Style::Series1,
    Style::Series2,
    Style::Series3,
    Style::Series4,
    Style::Series5,
    Style::Series6,
    Style::Series7,
    Style::Series8,
];

/// The colour of a class, stable: chosen from its index in the model.
pub fn style(class: usize) -> Style {
    SERIES[class % SERIES.len()]
}

/// Every object seen on the picture: in its class's colour while it counts
/// towards an event, muted while it does not (not yet confirmed, outside the
/// areas events need, or stationary: "still").
pub fn objects(objects: &Objects, settings: &DetectSettings, now_ms: i64) -> Vec<OverlayItem> {
    objects
        .seen()
        .map(|object| {
            let counts = objects.counts(object, settings, now_ms);
            let still = if object.stationary { " · still" } else { "" };
            OverlayItem::Box {
                rect: object.rect,
                label: Some(format!(
                    "{} {:.0}%{still}",
                    object.label,
                    object.score * 100.0
                )),
                style: if counts {
                    style(object.class)
                } else {
                    Style::Muted
                },
                fill: false,
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_counting_object_gets_its_classs_colour_and_a_doubtful_one_is_muted() {
        let mut car = crate::detector::tests::found(2, "car", 0.87, [0.1, 0.2, 0.3, 0.2]);
        car.label = "Car".into();
        let settings = DetectSettings {
            classes: vec![crate::settings::ClassChoice::new("car", 50)],
            ..DetectSettings::default()
        };
        let mut followed = Objects::new();
        followed.observe(&[car.clone()], &settings, 0);
        let first = objects(&followed, &settings, 0);
        assert!(
            matches!(&first[0], OverlayItem::Box { label: Some(l), style: Style::Muted, .. } if l == "Car 87%"),
            "one sighting: not confirmed yet"
        );
        followed.observe(&[car], &settings, 500);
        let second = objects(&followed, &settings, 500);
        assert!(matches!(
            &second[0],
            OverlayItem::Box {
                style: Style::Series3,
                ..
            }
        ));
    }

    #[test]
    fn colours_repeat_every_eight_classes() {
        assert_eq!(style(0), Style::Series1);
        assert_eq!(style(7), Style::Series8);
        assert_eq!(style(8), Style::Series1);
        assert_eq!(style(79), style(7));
    }
}
