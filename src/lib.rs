//! NeuroWatch object detection, as an analytics plugin daemon.
//!
//! NeuroWatch connects to the daemon (`nw-analytics-protocol`), binds
//! cameras and sends their compressed video (or pictures it already
//! decoded); the daemon decodes it, runs a YOLOX model and answers with
//! events for the classes the model knows and the camera's settings choose,
//! and a labelled box for each object in view.
//!
//! The analysis itself ([`detector`], [`activity`], [`settings`]) is pure:
//! pictures and times in, detections and event actions out.

// The settings schema is one large `serde_json::json!` literal.
#![recursion_limit = "256"]

pub mod access;
pub mod activity;
pub mod classes;
pub mod decoding;
pub mod detector;
pub mod drawing;
pub mod gstreamer_runtime;
pub mod manifest;
pub mod objects;
pub mod scaling;
pub mod server;
pub mod settings;
pub mod worker;
