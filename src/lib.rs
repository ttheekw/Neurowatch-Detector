//! NeuroWatch object detection, as an analytics plugin daemon.
//!
//! NeuroWatch connects to the daemon (`nw-analytics-protocol`), binds
//! cameras and sends their compressed video (or pictures it already
//! decoded); the daemon decodes it, runs a YOLOX model and answers with
//! person, vehicle and animal events, boxes to draw and counts of what is in
//! view.
//!
//! The analysis itself ([`detector`], [`activity`], [`settings`]) is pure:
//! pictures and times in, detections and event actions out.
pub mod access;
pub mod activity;
pub mod decoding;
pub mod detector;
pub mod drawing;
pub mod gstreamer_runtime;
pub mod manifest;
pub mod server;
pub mod settings;
pub mod worker;
