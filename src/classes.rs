//! What a model can detect: its class file, a JSON document beside the ONNX
//! (`yolox_nano.json` for `yolox_nano.onnx`) naming each output class in the
//! model's output order. NeuroWatch learns the classes from the plugin's
//! manifest, so any model with a class file works.
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The class file of the built-in YOLOX-Nano.
pub const BUILT_IN: &str = include_str!("../models/yolox_nano.json");
/// The confidence a class starts at when its class file gives none.
pub const DEFAULT_CONFIDENCE: u8 = 50;
/// The range a class's confidence may take, in percent.
pub const CONFIDENCE_RANGE: std::ops::RangeInclusive<u8> = 10..=95;

#[derive(Debug, Deserialize)]
struct RawFile {
    name: String,
    classes: Vec<RawClass>,
}

#[derive(Debug, Deserialize)]
struct RawClass {
    id: String,
    label: String,
    #[serde(default)]
    default: bool,
    confidence: Option<u8>,
}

/// One class the model can find.
#[derive(Debug, Clone, PartialEq)]
pub struct Class {
    /// The event kind and the settings' value: `[a-z0-9_]`, 1–32 characters.
    pub id: Arc<str>,
    /// What people read: `Traffic light`.
    pub label: Arc<str>,
    /// Chosen on a new camera.
    pub default: bool,
    /// Where its confidence starts, in percent.
    pub confidence: u8,
}

/// A model's classes, in its output order (index `i` is output column `5 + i`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ClassList {
    pub name: String,
    classes: Vec<Class>,
    by_id: HashMap<Arc<str>, usize>,
}

impl ClassList {
    /// Parse and check a class file.
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: RawFile =
            serde_json::from_str(text).map_err(|error| format!("invalid class file: {error}"))?;
        if file.classes.is_empty() {
            return Err("the class file lists no classes".into());
        }
        let mut classes = Vec::with_capacity(file.classes.len());
        let mut by_id = HashMap::new();
        for raw in file.classes {
            let valid_id = (1..=32).contains(&raw.id.len())
                && raw
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            if !valid_id {
                return Err(format!(
                    "class id {:?} must be 1–32 characters of a–z, 0–9 and _",
                    raw.id
                ));
            }
            if !(1..=64).contains(&raw.label.chars().count()) {
                return Err(format!(
                    "class {:?} needs a label of 1–64 characters",
                    raw.id
                ));
            }
            let confidence = raw.confidence.unwrap_or(DEFAULT_CONFIDENCE);
            if !CONFIDENCE_RANGE.contains(&confidence) {
                return Err(format!(
                    "class {:?}: confidence must be 10–95, found {confidence}",
                    raw.id
                ));
            }
            let id: Arc<str> = Arc::from(raw.id.as_str());
            if by_id.insert(Arc::clone(&id), classes.len()).is_some() {
                return Err(format!("class id {:?} appears twice", raw.id));
            }
            classes.push(Class {
                id,
                label: Arc::from(raw.label.as_str()),
                default: raw.default,
                confidence,
            });
        }
        Ok(Self {
            name: file.name,
            classes,
            by_id,
        })
    }

    /// The built-in model's classes.
    pub fn built_in() -> Result<Self, String> {
        Self::parse(BUILT_IN)
    }

    /// Read a class file.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            anyhow::anyhow!("cannot read class file {}: {error}", path.display())
        })?;
        Self::parse(&text)
            .map_err(|error| anyhow::anyhow!("class file {}: {error}", path.display()))
    }

    pub fn len(&self) -> usize {
        self.classes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Class> {
        self.classes.get(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Class> {
        self.classes.iter()
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.by_id.get(id).copied()
    }

    pub fn find(&self, id: &str) -> Option<&Class> {
        self.index_of(id).and_then(|index| self.classes.get(index))
    }
}

/// `models/yolox_tiny.onnx` → `models/yolox_tiny.json`.
pub fn beside(model: &Path) -> PathBuf {
    model.with_extension("json")
}
