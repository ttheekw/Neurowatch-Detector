//! Object detection with a YOLOX model (Apache-2.0, trained on COCO) run by
//! tract, a pure-Rust ONNX runtime: no native libraries to ship.
//!
//! One model is loaded per process and shared by every camera's worker
//! ([`install`], [`shared`]). A picture goes in as RGB of any size; it is
//! letterboxed to the model's square input as YOLOX was trained (top-left,
//! grey 114 padding, BGR, 0–255), and the raw grid output is decoded and
//! thinned with per-class non-maximum suppression.
use crate::settings::{DetectSettings, Kind};
use std::sync::{Arc, OnceLock};
use tract_onnx::prelude::*;

/// The model built into the binary: YOLOX-Nano, 416×416.
const BUILT_IN: &[u8] = include_bytes!("../models/yolox_nano.onnx");
const STRIDES: [usize; 3] = [8, 16, 32];
const PAD: f32 = 114.0;
const NMS_IOU: f32 = 0.45;
/// Boxes considered before suppression, highest scores first.
const MAX_CANDIDATES: usize = 300;

type Plan = Arc<TypedSimplePlan>;

pub struct Model {
    plan: Plan,
    /// The square input's side, in pixels.
    pub size: usize,
    /// Where it came from, for the logs and `--check`.
    pub name: String,
}

static SHARED: OnceLock<Arc<Model>> = OnceLock::new();

/// Load the model once for the whole process: the built-in one, or a YOLOX
/// ONNX export at `path`.
pub fn install(path: Option<&std::path::Path>) -> anyhow::Result<Arc<Model>> {
    if let Some(model) = SHARED.get() {
        return Ok(Arc::clone(model));
    }
    let model = Arc::new(match path {
        Some(path) => Model::load(&std::fs::read(path)?, path.display().to_string())?,
        None => Model::load(BUILT_IN, "YOLOX-Nano (built in)".into())?,
    });
    Ok(Arc::clone(SHARED.get_or_init(|| model)))
}

/// The process's model, loading the built-in one if none was installed.
pub fn shared() -> anyhow::Result<Arc<Model>> {
    install(None)
}

/// One object the detector found, in coordinates normalised to the picture.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    pub kind: Kind,
    /// What the model called it (`car`, `dog`…).
    pub class: &'static str,
    pub score: f32,
    /// `[x, y, width, height]`, 0..1.
    pub rect: [f32; 4],
}

impl Detection {
    /// Where it stands: the middle of its box's bottom edge.
    pub fn foot(&self) -> (f32, f32) {
        (
            self.rect[0] + self.rect[2] / 2.0,
            self.rect[1] + self.rect[3],
        )
    }
}

/// COCO's classes this plugin reports, and as what.
pub fn class(index: usize) -> Option<(Kind, &'static str)> {
    Some(match index {
        0 => (Kind::Person, "person"),
        1 => (Kind::Vehicle, "bicycle"),
        2 => (Kind::Vehicle, "car"),
        3 => (Kind::Vehicle, "motorcycle"),
        5 => (Kind::Vehicle, "bus"),
        7 => (Kind::Vehicle, "truck"),
        14 => (Kind::Animal, "bird"),
        15 => (Kind::Animal, "cat"),
        16 => (Kind::Animal, "dog"),
        17 => (Kind::Animal, "horse"),
        18 => (Kind::Animal, "sheep"),
        19 => (Kind::Animal, "cow"),
        20 => (Kind::Animal, "elephant"),
        21 => (Kind::Animal, "bear"),
        22 => (Kind::Animal, "zebra"),
        23 => (Kind::Animal, "giraffe"),
        _ => return None,
    })
}

impl Model {
    pub fn load(bytes: &[u8], name: String) -> anyhow::Result<Self> {
        let model = tract_onnx::onnx()
            .model_for_read(&mut std::io::Cursor::new(bytes))?
            .into_optimized()?;
        let shape = model
            .input_fact(0)?
            .shape
            .as_concrete()
            .map(<[usize]>::to_vec);
        let size = match shape.as_deref() {
            Some([1, 3, h, w]) if h == w && *h % 32 == 0 => *h,
            other => anyhow::bail!(
                "{name}: expected a YOLOX model with a fixed square input [1, 3, N, N], found {other:?}"
            ),
        };
        let plan = model.into_runnable()?;
        Ok(Self { plan, size, name })
    }

    /// The objects in an RGB picture (`width × height`, rows packed), before
    /// any setting is applied, above `min_score`.
    pub fn detect(
        &self,
        rgb: &[u8],
        width: usize,
        height: usize,
        min_score: f32,
    ) -> anyhow::Result<Vec<Detection>> {
        anyhow::ensure!(
            width > 0 && height > 0 && rgb.len() >= width * height * 3,
            "picture of {width}×{height} has {} bytes",
            rgb.len()
        );
        let (input, ratio) = letterbox(rgb, width, height, self.size);
        let input = tract_ndarray::Array4::from_shape_vec((1, 3, self.size, self.size), input)?;
        let outputs = self.plan.run(tvec!(Tensor::from(input).into()))?;
        let output = outputs[0].to_plain_array_view::<f32>()?;
        let shape = output.shape().to_vec();
        let rows = shape.get(1).copied().unwrap_or(0);
        let columns = shape.get(2).copied().unwrap_or(0);
        anyhow::ensure!(
            shape.len() == 3 && columns > 5 && rows == anchors(self.size),
            "unexpected output shape {shape:?}"
        );
        let data = output
            .as_slice()
            .ok_or_else(|| anyhow::anyhow!("output is not contiguous"))?;
        let mut candidates = Vec::new();
        for (row, (grid_x, grid_y, stride)) in grid(self.size).enumerate() {
            let values = &data[row * columns..(row + 1) * columns];
            let objectness = values[4];
            if objectness < min_score {
                continue;
            }
            let Some((best, class_score)) = values[5..]
                .iter()
                .enumerate()
                .filter(|(index, _)| class(*index).is_some())
                .max_by(|a, b| a.1.total_cmp(b.1))
            else {
                continue;
            };
            let score = objectness * class_score;
            if score < min_score {
                continue;
            }
            let Some((kind, name)) = class(best) else {
                continue;
            };
            let stride = stride as f32;
            let cx = (values[0] + grid_x as f32) * stride / ratio;
            let cy = (values[1] + grid_y as f32) * stride / ratio;
            let w = values[2].exp() * stride / ratio;
            let h = values[3].exp() * stride / ratio;
            let rect = clamp_rect(
                (cx - w / 2.0) / width as f32,
                (cy - h / 2.0) / height as f32,
                w / width as f32,
                h / height as f32,
            );
            if rect[2] > 0.0 && rect[3] > 0.0 {
                candidates.push((
                    kind.index(),
                    Detection {
                        kind,
                        class: name,
                        score,
                        rect,
                    },
                ));
            }
        }
        Ok(suppress(candidates))
    }
}

/// The detections a camera's settings keep: chosen kinds, confident enough,
/// standing in a watched place.
pub fn keep(detections: Vec<Detection>, settings: &DetectSettings) -> Vec<Detection> {
    let min = f32::from(settings.confidence) / 100.0;
    detections
        .into_iter()
        .filter(|d| d.score >= min && settings.wants(d.kind))
        .filter(|d| {
            let (x, y) = d.foot();
            settings.watched(x, y)
        })
        .collect()
}

fn anchors(size: usize) -> usize {
    STRIDES.iter().map(|s| (size / s) * (size / s)).sum()
}

/// Each output row's grid cell and stride, in the model's order.
fn grid(size: usize) -> impl Iterator<Item = (usize, usize, usize)> {
    STRIDES.into_iter().flat_map(move |stride| {
        let cells = size / stride;
        (0..cells).flat_map(move |y| (0..cells).map(move |x| (x, y, stride)))
    })
}

fn clamp_rect(x: f32, y: f32, w: f32, h: f32) -> [f32; 4] {
    let x0 = x.clamp(0.0, 1.0);
    let y0 = y.clamp(0.0, 1.0);
    let x1 = (x + w).clamp(0.0, 1.0);
    let y1 = (y + h).clamp(0.0, 1.0);
    [x0, y0, x1 - x0, y1 - y0]
}

/// The model's input (planar BGR, 0–255) and the scale applied: the picture
/// scaled to fit, at the top left, the rest grey.
fn letterbox(rgb: &[u8], width: usize, height: usize, size: usize) -> (Vec<f32>, f32) {
    let ratio = (size as f32 / width as f32).min(size as f32 / height as f32);
    let scaled_w = ((width as f32 * ratio).round() as usize).clamp(1, size);
    let scaled_h = ((height as f32 * ratio).round() as usize).clamp(1, size);
    let plane = size * size;
    let mut input = vec![PAD; 3 * plane];
    for y in 0..scaled_h {
        let source_y = ((y as f32 + 0.5) / ratio) as usize;
        let source_y = source_y.min(height - 1);
        for x in 0..scaled_w {
            let source_x = (((x as f32 + 0.5) / ratio) as usize).min(width - 1);
            let pixel = (source_y * width + source_x) * 3;
            let at = y * size + x;
            input[at] = f32::from(rgb[pixel + 2]); // B
            input[plane + at] = f32::from(rgb[pixel + 1]); // G
            input[2 * plane + at] = f32::from(rgb[pixel]); // R
        }
    }
    (input, ratio)
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = (a[0] + a[2]).min(b[0] + b[2]);
    let y1 = (a[1] + a[3]).min(b[1] + b[3]);
    let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Non-maximum suppression within each kind: one box per object, even when
/// the model is torn between `car` and `truck`.
fn suppress(mut candidates: Vec<(usize, Detection)>) -> Vec<Detection> {
    candidates.sort_by(|a, b| b.1.score.total_cmp(&a.1.score));
    candidates.truncate(MAX_CANDIDATES);
    let mut kept: Vec<(usize, Detection)> = Vec::new();
    for (group, detection) in candidates {
        if kept
            .iter()
            .all(|(other, kept)| *other != group || iou(&kept.rect, &detection.rect) < NMS_IOU)
        {
            kept.push((group, detection));
        }
    }
    kept.into_iter().map(|(_, detection)| detection).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_matches_the_models_output_rows() {
        assert_eq!(anchors(416), 52 * 52 + 26 * 26 + 13 * 13);
        assert_eq!(grid(416).count(), anchors(416));
        assert_eq!(grid(416).nth(52), Some((0, 1, 8)));
        assert_eq!(grid(416).last(), Some((12, 12, 32)));
    }

    #[test]
    fn a_wide_picture_sits_at_the_top_left_with_grey_below() {
        let rgb = [200u8, 100, 50].repeat(32 * 16);
        let (input, ratio) = letterbox(&rgb, 32, 16, 64);
        assert_eq!(ratio, 2.0);
        assert_eq!(input[0], 50.0, "blue first");
        assert_eq!(input[64 * 64], 100.0);
        assert_eq!(input[2 * 64 * 64], 200.0);
        assert_eq!(input[40 * 64], PAD, "below the picture");
    }

    #[test]
    fn overlapping_boxes_of_one_kind_keep_the_surest() {
        let detection = |score: f32, x: f32| Detection {
            kind: Kind::Person,
            class: "person",
            score,
            rect: [x, 0.1, 0.2, 0.5],
        };
        let kept = suppress(vec![
            (0, detection(0.6, 0.11)),
            (0, detection(0.9, 0.1)),
            (0, detection(0.8, 0.6)),
            (2, detection(0.7, 0.1)),
        ]);
        let scores: Vec<f32> = kept.iter().map(|d| d.score).collect();
        assert_eq!(scores, vec![0.9, 0.8, 0.7]);
    }

    #[test]
    fn settings_keep_only_chosen_confident_objects_in_watched_places() {
        let found = vec![
            Detection {
                kind: Kind::Person,
                class: "person",
                score: 0.8,
                rect: [0.1, 0.1, 0.1, 0.3],
            },
            Detection {
                kind: Kind::Person,
                class: "person",
                score: 0.3,
                rect: [0.1, 0.1, 0.1, 0.3],
            },
            Detection {
                kind: Kind::Animal,
                class: "dog",
                score: 0.9,
                rect: [0.5, 0.5, 0.1, 0.1],
            },
        ];
        let kept = keep(found, &DetectSettings::default());
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].class, "person");
    }

    #[test]
    fn the_built_in_model_loads_and_finds_nothing_in_a_grey_picture() {
        let model = shared().unwrap();
        assert_eq!(model.size, 416);
        let grey = vec![128u8; 640 * 360 * 3];
        assert!(model.detect(&grey, 640, 360, 0.5).unwrap().is_empty());
    }
}
