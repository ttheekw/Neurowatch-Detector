//! Object detection with a YOLOX model (Apache-2.0, trained on COCO) run by
//! tract, a pure-Rust ONNX runtime: no native libraries to ship.
//!
//! One model is loaded per process and shared by every camera's worker
//! ([`install`], [`shared`]). A picture goes in as RGB of any size; it is
//! letterboxed to the model's square input as YOLOX was trained (top-left,
//! grey 114 padding, BGR, 0–255), and the raw grid output is decoded and
//! thinned with per-class non-maximum suppression. What the model's classes are comes
//! from its class file ([`ClassList`]).
use crate::classes::ClassList;
use crate::settings::DetectSettings;
use std::path::PathBuf;
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
    /// What its output columns mean.
    pub classes: Arc<ClassList>,
}

static SHARED: OnceLock<Arc<Model>> = OnceLock::new();

/// Load the model once for the whole process: the built-in one, or a YOLOX
/// ONNX export at `path` with its class file (`classes`, else `path` with
/// the extension `.json`).
pub fn install(
    path: Option<&std::path::Path>,
    classes: Option<&std::path::Path>,
) -> anyhow::Result<Arc<Model>> {
    if let Some(model) = SHARED.get() {
        return Ok(Arc::clone(model));
    }
    // One load at a time: threads that arrive together share the first's.
    static LOADING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _loading = LOADING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(model) = SHARED.get() {
        return Ok(Arc::clone(model));
    }
    let model = Arc::new(match path {
        Some(path) => {
            let class_file = classes.map_or_else(|| crate::classes::beside(path), PathBuf::from);
            let list = ClassList::load(&class_file)?;
            Model::load(&std::fs::read(path)?, path.display().to_string(), list)?
        }
        None => {
            let list = match classes {
                Some(file) => ClassList::load(file)?,
                None => {
                    ClassList::built_in().map_err(|e| anyhow::anyhow!("built-in classes: {e}"))?
                }
            };
            Model::load(BUILT_IN, "YOLOX-Nano (built in)".into(), list)?
        }
    });
    Ok(Arc::clone(SHARED.get_or_init(|| model)))
}

/// The process's model, loading the built-in one if none was installed.
pub fn shared() -> anyhow::Result<Arc<Model>> {
    install(None, None)
}

/// The process's classes (empty if the model cannot load).
pub fn shared_classes() -> Arc<ClassList> {
    shared().map_or_else(
        |_| Arc::new(ClassList::default()),
        |model| Arc::clone(&model.classes),
    )
}

/// One object the detector found, in coordinates normalised to the picture.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    /// The class's index in the model's output.
    pub class: usize,
    /// The class's id (`car`, `dog`…): the event kind.
    pub id: Arc<str>,
    pub label: Arc<str>,
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

impl Model {
    pub fn load(bytes: &[u8], name: String, classes: ClassList) -> anyhow::Result<Self> {
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
        // Output columns: x, y, w, h, objectness, then one score per class.
        let columns = model
            .output_fact(0)?
            .shape
            .as_concrete()
            .filter(|shape| shape.len() == 3)
            .map(|shape| shape[2]);
        if let Some(columns) = columns {
            anyhow::ensure!(
                columns >= 5 && columns - 5 == classes.len(),
                "{name}: the model has {} classes but its class file lists {}",
                columns.saturating_sub(5),
                classes.len()
            );
        }
        let plan = model.into_runnable()?;
        Ok(Self {
            plan,
            size,
            name,
            classes: Arc::new(classes),
        })
    }

    /// Every class's objects in an RGB picture (`width × height`, rows
    /// packed) above `min_score`.
    pub fn detect(
        &self,
        rgb: &[u8],
        width: usize,
        height: usize,
        min_score: f32,
    ) -> anyhow::Result<Vec<Detection>> {
        self.detect_classes(
            rgb,
            width,
            height,
            &vec![Some(min_score); self.classes.len()],
        )
    }

    /// The objects of the classes with a threshold (`thresholds[i]` for class
    /// `i`, `None`: not wanted), each at or above its own.
    pub fn detect_classes(
        &self,
        rgb: &[u8],
        width: usize,
        height: usize,
        thresholds: &[Option<f32>],
    ) -> anyhow::Result<Vec<Detection>> {
        let Some(lowest) = thresholds.iter().flatten().copied().reduce(f32::min) else {
            return Ok(vec![]);
        };
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
            shape.len() == 3 && columns == 5 + self.classes.len() && rows == anchors(self.size),
            "unexpected output shape {shape:?} for {} classes",
            self.classes.len()
        );
        let data = output
            .as_slice()
            .ok_or_else(|| anyhow::anyhow!("output is not contiguous"))?;
        let mut candidates = Vec::new();
        for (row, (grid_x, grid_y, stride)) in grid(self.size).enumerate() {
            let values = &data[row * columns..(row + 1) * columns];
            let objectness = values[4];
            if objectness < lowest {
                continue;
            }
            let mut best: Option<(usize, f32)> = None;
            for (index, class_score) in values[5..].iter().enumerate() {
                let score = objectness * class_score;
                let wanted = thresholds
                    .get(index)
                    .copied()
                    .flatten()
                    .is_some_and(|threshold| score >= threshold);
                if wanted && best.is_none_or(|(_, top)| score > top) {
                    best = Some((index, score));
                }
            }
            let Some((class, score)) = best else {
                continue;
            };
            let Some(info) = self.classes.get(class) else {
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
                candidates.push(Detection {
                    class,
                    id: Arc::clone(&info.id),
                    label: Arc::clone(&info.label),
                    score,
                    rect,
                });
            }
        }
        Ok(suppress(candidates))
    }
}

/// The detections a camera's settings keep: chosen classes, each confident
/// enough for its own setting, standing in a watched place.
/// The sightings worth tracking: a chosen class at or above its confidence,
/// with an allowed size and shape, not standing in an ignored area. Whether
/// a watched area is needed is decided per object, later (`crate::objects`):
/// an object outside one is still followed and drawn.
pub fn keep(detections: Vec<Detection>, settings: &DetectSettings) -> Vec<Detection> {
    detections
        .into_iter()
        .filter(|d| {
            settings.class(&d.id).is_some_and(|choice| {
                d.score >= f32::from(choice.confidence) / 100.0 && choice.fits(d.rect)
            })
        })
        .filter(|d| {
            let (x, y) = d.foot();
            !settings.ignored(x, y)
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

/// Non-maximum suppression within each class: one box per object of a class.
fn suppress(mut candidates: Vec<Detection>) -> Vec<Detection> {
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    candidates.truncate(MAX_CANDIDATES);
    let mut kept: Vec<Detection> = Vec::new();
    for detection in candidates {
        if kept.iter().all(|other| {
            other.class != detection.class || iou(&other.rect, &detection.rect) < NMS_IOU
        }) {
            kept.push(detection);
        }
    }
    kept
}

#[cfg(test)]
pub(crate) mod tests {
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

    pub(crate) fn found(class: usize, id: &str, score: f32, rect: [f32; 4]) -> Detection {
        Detection {
            class,
            id: Arc::from(id),
            label: Arc::from(id),
            score,
            rect,
        }
    }

    #[test]
    fn overlapping_boxes_of_one_class_keep_the_surest() {
        let detection =
            |class: usize, score: f32, x: f32| found(class, "c", score, [x, 0.1, 0.2, 0.5]);
        let kept = suppress(vec![
            detection(0, 0.6, 0.11),
            detection(0, 0.9, 0.1),
            detection(0, 0.8, 0.6),
            detection(2, 0.7, 0.1),
        ]);
        let scores: Vec<f32> = kept.iter().map(|d| d.score).collect();
        assert_eq!(scores, vec![0.9, 0.8, 0.7]);
    }

    #[test]
    fn settings_keep_only_chosen_classes_at_their_own_confidence() {
        let rect = [0.1, 0.1, 0.1, 0.3];
        let found = vec![
            found(0, "person", 0.8, rect),
            found(0, "person", 0.3, rect),
            found(16, "dog", 0.9, rect),
            found(2, "car", 0.6, rect),
        ];
        let settings = DetectSettings {
            classes: vec![
                crate::settings::ClassChoice::new("person", 50),
                crate::settings::ClassChoice::new("car", 70),
            ],
            ..DetectSettings::default()
        };
        let kept = keep(found, &settings);
        assert_eq!(kept.len(), 1);
        assert_eq!(&*kept[0].id, "person");
    }

    #[test]
    fn a_class_keeps_only_boxes_of_its_size_and_shape() {
        let person = crate::settings::ClassChoice {
            min_size: 1.0,
            max_size: 20.0,
            min_ratio: 0.2,
            max_ratio: 1.0,
            ..crate::settings::ClassChoice::new("person", 50)
        };
        let settings = DetectSettings {
            classes: vec![person],
            ..DetectSettings::default()
        };
        let kept =
            |rect: [f32; 4]| !keep(vec![found(0, "person", 0.9, rect)], &settings).is_empty();
        assert!(
            kept([0.1, 0.1, 0.1, 0.3]),
            "3% of the picture, 0.33 wide per high"
        );
        assert!(!kept([0.1, 0.1, 0.05, 0.1]), "0.5%: too small");
        assert!(!kept([0.0, 0.0, 0.5, 0.6]), "30%: too large");
        assert!(!kept([0.1, 0.1, 0.3, 0.1]), "3 wide per high: too wide");
        assert!(
            !kept([0.1, 0.1, 0.02, 0.5]),
            "0.04 wide per high: too narrow"
        );
    }

    #[test]
    fn an_object_outside_the_watched_areas_is_kept_but_not_in_an_ignored_one() {
        let square = |x0: f32, x1: f32| vec![[x0, 0.0], [x1, 0.0], [x1, 1.0], [x0, 1.0]];
        let zone = |id: &str, kind, points| crate::settings::Zone {
            id: id.into(),
            name: String::new(),
            kind,
            points,
            classes: vec![],
            frames: 3,
            loiter_sec: 0.0,
            min_speed_kmh: 0.0,
            sides_m: [0.0; 4],
            events: true,
            alerts: true,
        };
        let settings = DetectSettings {
            classes: vec![crate::settings::ClassChoice::new("person", 50)],
            zones: vec![
                zone("road", crate::settings::ZoneKind::Include, square(0.0, 0.5)),
                zone("tree", crate::settings::ZoneKind::Exclude, square(0.8, 1.0)),
            ],
            ..DetectSettings::default()
        };
        let at = |x: f32| found(0, "person", 0.9, [x, 0.2, 0.05, 0.3]);
        let kept = keep(vec![at(0.1), at(0.6), at(0.9)], &settings);
        let xs: Vec<f32> = kept.iter().map(|d| d.rect[0]).collect();
        assert_eq!(
            xs,
            vec![0.1, 0.6],
            "outside the watched area: still followed"
        );
    }

    #[test]
    fn a_class_file_that_does_not_match_the_model_is_refused() {
        let classes = ClassList::parse(
            r#"{"name": "T", "classes": [{"id": "a", "label": "A"}, {"id": "b", "label": "B"}]}"#,
        )
        .unwrap();
        let error = Model::load(BUILT_IN, "m".into(), classes)
            .err()
            .map(|e| e.to_string());
        assert!(error.is_some_and(|e| e.contains("80 classes")), "refused");
    }

    #[test]
    fn the_built_in_model_loads_and_finds_nothing_in_a_grey_picture() {
        let model = shared().unwrap();
        assert_eq!(model.size, 416);
        assert_eq!(model.classes.len(), 80);
        let grey = vec![128u8; 640 * 360 * 3];
        assert!(model.detect(&grey, 640, 360, 0.5).unwrap().is_empty());
        assert!(model
            .detect_classes(&grey, 640, 360, &vec![None; 80])
            .unwrap()
            .is_empty());
    }
}
