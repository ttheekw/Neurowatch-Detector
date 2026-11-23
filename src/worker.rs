//! One bound camera: compressed units in, object events out. Decodes the
//! camera's units itself (or takes NeuroWatch's decoded pictures) into RGB
//! [`ANALYSIS_WIDTH`] wide at the camera's analysis rate, runs the model and
//! reports the classes its settings choose.
use crate::activity::{Action, Activity};
use crate::decoding::{self, RunningFailures};
use crate::detector::{self, Model};
use crate::drawing;
use crate::manifest::{OBJECTS, ZONES};
use crate::objects::Objects;
use crate::scaling::{self, GpuScale, Scaler};
use crate::settings::DetectSettings;
use gstreamer::prelude::*;
use gstreamer_video::prelude::*;
use nw_analytics_protocol::{
    frame::MediaUnit,
    messages::{
        BindingState, BindingStatus, Codec, EventEnd, Overlay, OverlayItem, Status, StreamFormat,
    },
    method,
    rpc::Message,
};
use nw_analytics_sdk::{
    pictures::{Next, Pictures},
    ring::{Picture, PictureIdentity, RawFormat, RawLayout, RingReader},
};
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

/// Units waiting for the decoder; more and the oldest are lost (the worker
/// restarts from the next keyframe).
const QUEUE: usize = 64;
const STATUS_EVERY: Duration = Duration::from_secs(2);
/// How long a drawing stays without a newer one: a little over the gap
/// between analysed pictures, so a stalled analysis leaves nothing stale.
fn drawing_ttl_ms(fps: u8) -> u32 {
    1000 / u32::from(fps.max(1)) + 1000
}
/// Pictures are scaled to this width (height keeps the shape) before the
/// model letterboxes them: the built-in model's input side.
pub const ANALYSIS_WIDTH: usize = 416;
/// A decoder that failed is rebuilt after this pause, from a keyframe.
const RETRY_AFTER: Duration = Duration::from_secs(3);
/// A hardware decoder with no picture to show for itself after this long has
/// failed, whatever it says: the next one takes over.
const DECODER_TRIAL: Duration = Duration::from_secs(10);
/// A running hardware decoder (or its scaler) that has given no picture for
/// this long while units arrive has stalled.
const DECODER_STALL: Duration = Duration::from_secs(10);

/// Which part of the decoding a failure is the fault of; only the decoder's
/// own and the scaler's count against them (a stream restart or a format
/// change never does).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cause {
    Decoder,
    Scaler,
    Input,
}

impl Cause {
    /// From the path of the element that posted an error: the elements the
    /// pipelines name `scale`, `download` and `decode`.
    fn of_source(path: &str) -> Self {
        if path.contains(":scale") || path.contains(":download") {
            Self::Scaler
        } else if path.contains(":decode") {
            Self::Decoder
        } else {
            Self::Input
        }
    }
}

/// Sends JSON-RPC messages to NeuroWatch (shared by a session's workers).
pub trait Sender: Send + Sync + 'static {
    fn send(&self, message: Message);
}

pub enum Input {
    Format(StreamFormat),
    Unit(MediaUnit),
    Moving(bool),
    Settings(DetectSettings),
    Decoded(RingReader),
    Withdrawn,
}

/// Options that apply to every session and worker. The decoder is each
/// camera's own setting ([`DetectSettings::decoder`]).
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// A session that hears nothing for this long is closed (NeuroWatch
    /// pings every 5 s and gives up on a plugin after 15 s itself).
    pub silence_limit: std::time::Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            silence_limit: std::time::Duration::from_secs(15),
        }
    }
}

pub struct Worker {
    tx: Option<mpsc::SyncSender<Input>>,
    lost: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    pub fn start(
        binding: u32,
        camera: String,
        settings: DetectSettings,
        events: Arc<AtomicU64>,
        out: Arc<dyn Sender>,
    ) -> std::io::Result<Self> {
        let model = detector::shared().map_err(std::io::Error::other)?;
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let lost = Arc::new(AtomicBool::new(false));
        let thread = {
            let lost = Arc::clone(&lost);
            std::thread::Builder::new()
                .name(format!("detect-{camera}"))
                .spawn(move || {
                    let mut run = Run::new(binding, settings, model, events, out);
                    run.serve(&rx, &lost);
                    run.finish_activity();
                })?
        };
        Ok(Self {
            tx: Some(tx),
            lost,
            thread: Some(thread),
        })
    }

    /// Pass input on without waiting. A full queue loses the unit; the worker
    /// then restarts from the next keyframe.
    pub fn offer(&self, input: Input) {
        let Some(tx) = self.tx.as_ref() else {
            return;
        };
        match input {
            Input::Unit(_) => {
                if tx.try_send(input).is_err() {
                    self.lost.store(true, Ordering::Release);
                }
            }
            // Rare control input waits for room.
            other => {
                let _ = tx.send(other);
            }
        }
    }

    /// Units were lost before reaching this worker (NeuroWatch's `gap`).
    pub fn lost(&self) {
        self.lost.store(true, Ordering::Release);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Closing the channel ends the worker; it ends its open event.
        self.tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// An RGB picture (rows packed), its width and height, capture time and identity.
type AnalyzedPicture = (Vec<u8>, usize, usize, Option<i64>, Option<u64>);

struct Decoder {
    pipeline: gstreamer::Pipeline,
    src: gstreamer_app::AppSrc,
    sink: gstreamer_app::AppSink,
    bus: gstreamer::Bus,
    name: Arc<std::sync::Mutex<Option<String>>>,
    identities: std::sync::Mutex<BTreeMap<u64, PictureIdentity>>,
}

/// The size the GPU scales to: this wide, the stream's own shape.
const TARGET: (u32, Option<u32>) = (ANALYSIS_WIDTH as u32, None);
/// How far behind schedule a picture's timestamp means a new timeline.
const TIMELINE_JUMP_NS: u64 = 2_000_000_000;

/// At most `fps` pictures a second, judged by their own timestamps alone.
struct RateLimit {
    interval_ns: u64,
    next_ns: Option<u64>,
}

impl RateLimit {
    fn new(fps: u32) -> Self {
        Self {
            interval_ns: 1_000_000_000 / u64::from(fps.max(1)),
            next_ns: None,
        }
    }

    fn admit(&mut self, pts_ns: u64) -> bool {
        // Timestamps far behind what was expected: a new timeline, not an old picture.
        let new_timeline = self
            .next_ns
            .is_some_and(|next| pts_ns.saturating_add(TIMELINE_JUMP_NS) < next);
        match self.next_ns {
            Some(next) if pts_ns < next && !new_timeline => false,
            Some(next) if !new_timeline => {
                // On schedule, so 25 fps becomes a steady 5; far later (a gap)
                // starts the schedule again from this picture.
                self.next_ns = Some(
                    next.saturating_add(self.interval_ns)
                        .max(pts_ns.saturating_add(self.interval_ns / 2)),
                );
                true
            }
            _ => {
                self.next_ns = Some(pts_ns.saturating_add(self.interval_ns));
                true
            }
        }
    }
}

/// Every element the decoding pipelines below name, and the software
/// decoders every camera can fall back to.
pub const REQUIRED_ELEMENTS: [&str; 8] = [
    "appsrc",
    "appsink",
    "h264parse",
    "h265parse",
    "videoscale",
    "videoconvert",
    "avdec_h264",
    "avdec_h265",
];

/// The [`REQUIRED_ELEMENTS`] this GStreamer lacks (`gstreamer::init` first).
pub fn missing_elements() -> Vec<&'static str> {
    REQUIRED_ELEMENTS
        .into_iter()
        .filter(|name| gstreamer::ElementFactory::find(name).is_none())
        .collect()
}

/// What keeps this GStreamer's VA-API from being used, if anything.
pub fn va_blocked() -> Option<String> {
    let plugin = gstreamer::Registry::get().find_plugin("va")?;
    let version = plugin.version();
    decoding::va_leaks(&version)
        .then(|| format!("GStreamer {version}'s VA-API leaks memory (fixed in 1.24.6)"))
}

/// The hardware decoders GStreamer has here, for `--check`.
pub fn hardware_decoders() -> Vec<&'static str> {
    [
        "vah264dec",
        "vah265dec",
        "nvh264dec",
        "nvh265dec",
        "qsvh264dec",
        "qsvh265dec",
        "v4l2slh264dec",
        "v4l2h264dec",
        "d3d11h264dec",
        "d3d12h264dec",
    ]
    .into_iter()
    .filter(|name| gstreamer::ElementFactory::find(name).is_some())
    .collect()
}

/// The GPU scalers GStreamer has here, for `--check` (they are optional:
/// without one the CPU scales).
pub fn gpu_scalers() -> Vec<&'static str> {
    scaling::available(&|name| gstreamer::ElementFactory::find(name).is_some())
}

impl Decoder {
    /// `element`: the decoder [`decoding::pick`] chose, named outright.
    fn new(
        format: &StreamFormat,
        element: &str,
        fps: u8,
        gpu: Option<GpuScale>,
    ) -> anyhow::Result<Self> {
        let (caps, parser) = match format.codec {
            Codec::H264 => ("video/x-h264", "h264parse"),
            Codec::H265 => ("video/x-h265", "h265parse"),
        };
        // Scaled on the GPU before the download; `videoscale` then has nothing to do.
        let gpu = gpu.map_or(String::new(), |gpu| format!("! {} ", gpu.segment()));
        let description = format!(
            "appsrc name=src is-live=true format=time \
               caps=\"{caps},stream-format=byte-stream,alignment=au\" \
             ! {parser} ! {element} name=decode {gpu}\
             ! videoscale ! videoconvert \
             ! video/x-raw,format=RGB,width={ANALYSIS_WIDTH},pixel-aspect-ratio=1/1 \
             ! appsink name=sink max-buffers=1 drop=true sync=false emit-signals=false"
        );
        let decoder = Self::build(&description, fps)?;
        if let Ok(mut name) = decoder.name.lock() {
            *name = Some(element.to_owned());
        }
        Ok(decoder)
    }

    fn raw(layout: RawLayout, fps: u8) -> anyhow::Result<Self> {
        let name = match layout.format {
            RawFormat::Rgb24 => "RGB",
            RawFormat::Nv12 => "NV12",
            RawFormat::Gray8 => "GRAY8",
        };
        Self::build(&format!("appsrc name=src is-live=true format=time caps=\"video/x-raw,format={name},width={},height={},framerate={fps}/1,pixel-aspect-ratio=1/1\" ! videoscale ! videoconvert ! video/x-raw,format=RGB,width={ANALYSIS_WIDTH},pixel-aspect-ratio=1/1 ! appsink name=sink max-buffers=1 drop=true sync=false emit-signals=false", layout.width, layout.height), fps)
    }

    fn build(description: &str, fps: u8) -> anyhow::Result<Self> {
        let pipeline = gstreamer::parse::launch(description)?
            .downcast::<gstreamer::Pipeline>()
            .map_err(|_| anyhow::anyhow!("not a pipeline"))?;
        let src = pipeline
            .by_name("src")
            .and_then(|e| e.downcast::<gstreamer_app::AppSrc>().ok())
            .ok_or_else(|| anyhow::anyhow!("missing appsrc"))?;
        let sink = pipeline
            .by_name("sink")
            .and_then(|e| e.downcast::<gstreamer_app::AppSink>().ok())
            .ok_or_else(|| anyhow::anyhow!("missing appsink"))?;
        let bus = pipeline
            .bus()
            .ok_or_else(|| anyhow::anyhow!("missing bus"))?;
        // The analysis rate, right after the decoder: before the costly scaling
        // and conversion, and a skipped hardware picture is never copied to
        // memory. Not `videorate`: joining a stream already running (large
        // timestamps) or one whose caps have no frame rate, it lets every
        // picture through.
        if let Some(decode) = pipeline.by_name("decode") {
            let limit = std::sync::Mutex::new(RateLimit::new(u32::from(fps)));
            decode
                .static_pad("src")
                .ok_or_else(|| anyhow::anyhow!("the decoder has no output"))?
                .add_probe(gstreamer::PadProbeType::BUFFER, move |_, info| {
                    let admitted = match info.buffer().and_then(|buffer| buffer.pts()) {
                        Some(pts) => limit
                            .lock()
                            .map_or(true, |mut limit| limit.admit(pts.nseconds())),
                        None => true,
                    };
                    if admitted {
                        gstreamer::PadProbeReturn::Ok
                    } else {
                        gstreamer::PadProbeReturn::Drop
                    }
                })
                .ok_or_else(|| anyhow::anyhow!("could not limit the analysis rate"))?;
        }
        let name = Arc::new(std::sync::Mutex::new(None));
        pipeline.set_state(gstreamer::State::Playing)?;
        Ok(Self {
            pipeline,
            src,
            sink,
            bus,
            name,
            identities: std::sync::Mutex::new(BTreeMap::new()),
        })
    }

    fn push(&self, unit: &MediaUnit, base_us: i64) -> anyhow::Result<()> {
        let mut buffer = gstreamer::Buffer::from_slice(unit.data.clone());
        {
            let buffer = buffer
                .get_mut()
                .ok_or_else(|| anyhow::anyhow!("new buffer is shared"))?;
            // The stream's own times when it has them, else arrival times.
            let arrival = || {
                gstreamer::ClockTime::from_useconds(
                    u64::try_from(unit.time_us.saturating_sub(base_us)).unwrap_or(0),
                )
            };
            buffer.set_pts(
                unit.pts_ns
                    .map(gstreamer::ClockTime::from_nseconds)
                    .or_else(|| Some(arrival())),
            );
            buffer.set_dts(unit.dts_ns.map(gstreamer::ClockTime::from_nseconds));
            if !unit.keyframe() {
                buffer.set_flags(gstreamer::BufferFlags::DELTA_UNIT);
            }
        }
        if let Some(pts) = buffer.pts() {
            self.remember(
                pts.nseconds(),
                PictureIdentity {
                    seq: unit.seq,
                    time_us: unit.time_us,
                    pts_ns: unit.pts_ns,
                },
            );
        }
        self.src
            .push_buffer(buffer)
            .map(|_| ())
            .map_err(|flow| anyhow::anyhow!("decoder refused input: {flow:?}"))
    }

    fn remember(&self, pts: u64, identity: PictureIdentity) {
        if let Ok(mut identities) = self.identities.lock() {
            identities.insert(pts, identity);
            while identities.len() > 4096 {
                identities.pop_first();
            }
        }
    }

    fn push_raw(&self, picture: Picture) -> anyhow::Result<()> {
        let format = match picture.layout.format {
            RawFormat::Rgb24 => gstreamer_video::VideoFormat::Rgb,
            RawFormat::Nv12 => gstreamer_video::VideoFormat::Nv12,
            RawFormat::Gray8 => gstreamer_video::VideoFormat::Gray8,
        };
        let info = gstreamer_video::VideoInfo::builder(
            format,
            picture.layout.width,
            picture.layout.height,
        )
        .build()?;
        let mut buffer = gstreamer::Buffer::with_size(info.size())?;
        let pts = u64::try_from(picture.identity.time_us)?.saturating_mul(1000);
        {
            let writable = buffer
                .get_mut()
                .ok_or_else(|| anyhow::anyhow!("new raw buffer shared"))?;
            writable.set_pts(gstreamer::ClockTime::from_nseconds(pts));
            let mut map = writable.map_writable()?;
            let mut start = 0;
            for (i, (width, rows)) in nw_analytics_sdk::ring::plane_sizes(picture.layout)?
                .into_iter()
                .enumerate()
            {
                if rows == 0 {
                    continue;
                }
                let stride = usize::try_from(info.stride()[i])?;
                let offset = *info
                    .offset()
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("raw picture has no plane {i}"))?;
                copy_rows(
                    &picture.data,
                    (start, width),
                    &mut map,
                    (offset, stride),
                    (width, rows),
                )?;
                start += width * rows;
            }
        }
        self.remember(pts, picture.identity);
        self.src
            .push_buffer(buffer)
            .map_err(|flow| anyhow::anyhow!("raw input refused: {flow:?}"))?;
        Ok(())
    }

    /// The next RGB picture, if one is ready, with its size, capture time and identity.
    fn pull(&self) -> anyhow::Result<Option<AnalyzedPicture>> {
        let Some(sample) = self.sink.try_pull_sample(gstreamer::ClockTime::ZERO) else {
            return Ok(None);
        };
        let caps = sample
            .caps()
            .ok_or_else(|| anyhow::anyhow!("missing caps"))?;
        let info = gstreamer_video::VideoInfo::from_caps(caps)?;
        let buffer = sample
            .buffer()
            .ok_or_else(|| anyhow::anyhow!("missing buffer"))?;
        let frame = gstreamer_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)?;
        let (width, height) = (frame.width() as usize, frame.height() as usize);
        let stride = usize::try_from(frame.plane_stride()[0])?;
        let plane = frame.plane_data(0)?;
        let row_bytes = width * 3;
        let mut pixels = vec![0u8; row_bytes * height];
        copy_rows(
            plane,
            (0, stride),
            &mut pixels,
            (0, row_bytes),
            (row_bytes, height),
        )?;
        let identity = buffer
            .pts()
            .and_then(|pts| self.identities.lock().ok()?.get(&pts.nseconds()).copied());
        let time_ms = identity.map(|id| id.time_us / 1000);
        Ok(Some((
            pixels,
            width,
            height,
            time_ms,
            identity.map(|id| id.seq),
        )))
    }

    /// What the pipeline reports gone wrong, and whose fault it was.
    fn failed(&self) -> Option<(String, Cause)> {
        self.bus
            .pop_filtered(&[gstreamer::MessageType::Error, gstreamer::MessageType::Eos])
            .map(|message| match message.view() {
                gstreamer::MessageView::Error(error) => (
                    error.error().to_string(),
                    message
                        .src()
                        .map_or(Cause::Input, |src| Cause::of_source(&src.path_string())),
                ),
                _ => ("the decoder stopped".into(), Cause::Decoder),
            })
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gstreamer::State::Null);
    }
}

struct Run {
    binding: u32,
    settings: DetectSettings,
    events: Arc<AtomicU64>,
    out: Arc<dyn Sender>,
    format: Option<StreamFormat>,
    pictures: Pictures,
    raw_layout: Option<RawLayout>,
    picture_seq: Option<u64>,
    decoder: Option<Decoder>,
    /// Decoders that let this camera down, until its setting changes.
    failed_decoders: Vec<&'static str>,
    /// Why this camera is not on the decoder chosen for it.
    decoder_note: Option<String>,
    /// The hardware decoder on trial: since when, until its first picture.
    decoder_trial: Option<Instant>,
    current_decoder: Option<&'static str>,
    /// Whether the current decoder is a hardware one.
    current_hardware: bool,
    /// The GPU scaler the current decoder runs with (its trial is the decoder's).
    current_scaler: Option<Scaler>,
    /// GPU scalers that let this camera down: its decoder scales on the CPU.
    failed_scalers: Vec<Scaler>,
    /// Why this camera scales on the CPU although it might not have to.
    scaler_note: Option<String>,
    /// Failures of components that had produced pictures.
    running_failures: RunningFailures,
    /// The decoder given up for failing while running, for the note.
    gave_up: Option<&'static str>,
    /// When the current decoder last gave a picture (or started).
    last_picture: Instant,
    /// Arrival time of the first unit of this decoder's run.
    base_us: i64,
    waiting_keyframe: bool,
    retry_at: Option<Instant>,
    moving: bool,
    model: Arc<Model>,
    /// The objects followed over pictures, and the rules on them.
    objects: Objects,
    activity: Activity,
    /// Each open event's protocol number, by its number in `activity`.
    event: BTreeMap<u64, u64>,
    /// Layers that show something now (cleared when that ends).
    drawn: [bool; 2],
    last_ms: Option<i64>,
    state: BindingState,
    detail: Option<String>,
    frames: u64,
    measured: u64,
    meter: Instant,
    fps: f32,
    reported: Instant,
}

impl Run {
    fn new(
        binding: u32,
        settings: DetectSettings,
        model: Arc<Model>,
        events: Arc<AtomicU64>,
        out: Arc<dyn Sender>,
    ) -> Self {
        let now = Instant::now();
        Self {
            binding,
            activity: Activity::new(&settings),
            objects: Objects::new(),
            model,
            settings,
            events,
            out,
            format: None,
            pictures: Pictures::default(),
            raw_layout: None,
            picture_seq: None,
            decoder: None,
            failed_decoders: vec![],
            decoder_note: None,
            decoder_trial: None,
            current_decoder: None,
            current_hardware: false,
            current_scaler: None,
            failed_scalers: vec![],
            scaler_note: None,
            running_failures: RunningFailures::default(),
            gave_up: None,
            last_picture: now,
            base_us: 0,
            waiting_keyframe: true,
            retry_at: None,
            moving: false,
            event: BTreeMap::new(),
            drawn: [false; 2],
            last_ms: None,
            state: BindingState::Waiting,
            detail: None,
            frames: 0,
            measured: 0,
            meter: now,
            fps: 0.0,
            reported: now - STATUS_EVERY,
        }
    }

    fn serve(&mut self, inputs: &mpsc::Receiver<Input>, lost: &AtomicBool) {
        loop {
            match inputs.recv_timeout(Duration::from_millis(20)) {
                Ok(input) => self.input(input, lost),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            // Take everything else already waiting before pulling pictures.
            for _ in 0..QUEUE {
                let Ok(input) = inputs.try_recv() else { break };
                self.input(input, lost);
            }
            self.source();
            self.pictures();
            self.report();
        }
    }

    fn input(&mut self, input: Input, lost: &AtomicBool) {
        match input {
            Input::Format(format) => {
                if self.format.as_ref() != Some(&format) {
                    self.pictures.gap();
                    self.format = Some(format);
                    self.restart();
                }
            }
            Input::Unit(unit) => {
                if lost.swap(false, Ordering::AcqRel) || unit.discontinuity() {
                    self.pictures.gap();
                    if !self.pictures.active() {
                        self.restart();
                    }
                }
                let unit = Arc::new(unit);
                if self.pictures.unit(Arc::clone(&unit)) {
                    self.unit(&unit);
                }
            }
            Input::Moving(moving) => {
                if moving && !self.moving {
                    self.reset_detection();
                }
                self.moving = moving;
            }
            Input::Decoded(ring) => {
                if !self.settings.low_cpu {
                    self.restart();
                    self.pictures.offer(ring);
                }
            }
            Input::Withdrawn => self.pictures.withdraw(),
            Input::Settings(settings) => {
                if settings.low_cpu {
                    self.pictures.withdraw();
                }
                let decoder_changed = settings.decoder != self.settings.decoder;
                let rate_changed = settings.analysis_fps != self.settings.analysis_fps;
                self.settings = settings;
                if rate_changed {
                    self.restart();
                }
                if decoder_changed {
                    self.failed_decoders.clear();
                    self.failed_scalers.clear();
                    self.running_failures.clear();
                    self.gave_up = None;
                    self.decoder_note = None;
                    self.scaler_note = None;
                    self.decoder_trial = None;
                    self.current_decoder = None;
                    self.current_scaler = None;
                    self.retry_at = None;
                    // Desktop pictures need no decoder of this camera's own.
                    if self.raw_layout.is_none() {
                        self.restart();
                    }
                }
                self.reset_detection();
            }
        }
    }

    fn source(&mut self) {
        match self.pictures.poll() {
            Next::Picture(picture) => {
                let layout = picture.layout;
                if self.raw_layout != Some(layout) || self.decoder.is_none() {
                    self.restart();
                    match Decoder::raw(layout, self.settings.analysis_fps) {
                        Ok(decoder) => {
                            self.decoder = Some(decoder);
                            self.raw_layout = Some(layout);
                        }
                        Err(error) => {
                            self.fail(error.to_string(), Cause::Input);
                            self.pictures.withdraw();
                            return;
                        }
                    }
                }
                if let Some(Err(error)) = self
                    .decoder
                    .as_ref()
                    .map(|decoder| decoder.push_raw(picture))
                {
                    self.fail(error.to_string(), Cause::Input);
                    self.pictures.withdraw();
                }
            }
            Next::Fallback(units) => {
                self.restart();
                for unit in units {
                    self.unit(&unit);
                }
            }
            Next::Waiting => {}
        }
    }

    /// A new decoder from the next keyframe, and a fresh detector: what
    /// follows may be a different moment or picture.
    fn restart(&mut self) {
        self.decoder = None;
        // A decoder on trial starts its trial again with the next one.
        self.decoder_trial = None;
        self.current_decoder = None;
        self.current_hardware = false;
        self.current_scaler = None;
        self.raw_layout = None;
        self.picture_seq = None;
        self.waiting_keyframe = true;
        self.reset_detection();
    }

    fn reset_detection(&mut self) {
        self.finish_activity();
        self.clear_layer(OBJECTS);
        self.clear_layer(ZONES);
        self.activity = Activity::new(&self.settings);
        self.objects = Objects::new();
        self.last_ms = None;
    }

    fn unit(&mut self, unit: &MediaUnit) {
        if self.waiting_keyframe && !unit.keyframe() {
            return;
        }
        if self.retry_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        if self.decoder.is_none() {
            let Some(format) = self.format.as_ref() else {
                return;
            };
            let blocked = va_blocked();
            let Some(pick) = decoding::pick(
                self.settings.decoder,
                format.codec,
                &self.failed_decoders,
                blocked.as_deref(),
                &|name| gstreamer::ElementFactory::find(name).is_some(),
            ) else {
                self.fail(
                    "No decoder for this video is installed".to_owned(),
                    Cause::Input,
                );
                return;
            };
            self.decoder_note = match (self.gave_up, pick.note) {
                (Some(element), _) => Some(decoding::gave_up_note(element, !pick.hardware)),
                (None, note) => note,
            };
            let gpu = if pick.hardware {
                scaling::plan(
                    pick.element,
                    (format.width, format.height),
                    TARGET.0,
                    TARGET.1,
                    &self.failed_scalers,
                    &|name| gstreamer::ElementFactory::find(name).is_some(),
                )
            } else {
                None
            };
            self.current_hardware = pick.hardware;
            self.current_scaler = gpu.map(|gpu| gpu.scaler);
            match Decoder::new(format, pick.element, self.settings.analysis_fps, gpu) {
                Ok(decoder) => {
                    tracing::info!(
                        binding = self.binding,
                        decoder = pick.element,
                        scaler = gpu.map_or("cpu", |gpu| gpu.scaler.element()),
                        "Decoding"
                    );
                    self.decoder = Some(decoder);
                    self.current_decoder = Some(pick.element);
                    self.decoder_trial = pick.hardware.then(Instant::now);
                    self.last_picture = Instant::now();
                    self.base_us = unit.time_us;
                    self.retry_at = None;
                }
                Err(error) if pick.hardware => {
                    self.current_decoder = Some(pick.element);
                    self.decoder_trial = Some(Instant::now());
                    self.fail(
                        format!("Could not start {}: {error}", pick.element),
                        Cause::Decoder,
                    );
                    return;
                }
                Err(error) => {
                    self.fail(format!("Could not start a decoder: {error}"), Cause::Input);
                    return;
                }
            }
        } else if self
            .decoder_trial
            .is_some_and(|since| since.elapsed() >= DECODER_TRIAL)
        {
            self.fail("It gave no pictures".to_owned(), Cause::Decoder);
            return;
        } else if self.current_hardware
            && self.decoder_trial.is_none()
            && self.last_picture.elapsed() >= DECODER_STALL
        {
            // Units keep coming and nothing comes out: with a GPU scaler in
            // the way, it is the newer part that is blamed first.
            let cause = if self.current_scaler.is_some() {
                Cause::Scaler
            } else {
                Cause::Decoder
            };
            self.fail("It stopped giving pictures".to_owned(), cause);
            return;
        }
        self.waiting_keyframe = false;
        let pushed = self
            .decoder
            .as_ref()
            .map(|decoder| decoder.push(unit, self.base_us));
        if let Some(Err(error)) = pushed {
            // The pipeline is gone: its own failure.
            self.fail(error.to_string(), Cause::Decoder);
        }
    }

    /// `cause`: whose fault it was, as far as is known.
    fn fail(&mut self, detail: String, cause: Cause) {
        // Nothing yet from a hardware decoder on trial: it will not start now.
        // With a GPU scaler in the pipeline the scaler is blamed first: the
        // same decoder carries on with CPU scaling. Only without one (or when
        // that fails too) the decoder is given up, and the next one takes over.
        if self.decoder_trial.take().is_some() {
            if let Some(scaler) = self.current_scaler.take() {
                tracing::warn!(binding = self.binding, scaler = scaler.element(), %detail, "GPU scaler failed; scaling on the CPU");
                self.failed_scalers.push(scaler);
                self.scaler_note = Some(scaling::given_up_note(scaler, false));
                self.decoder = None;
                self.current_decoder = None;
                self.waiting_keyframe = true;
                return;
            }
            if let Some(element) = self.current_decoder.take() {
                tracing::warn!(binding = self.binding, decoder = element, %detail, "Decoder failed; trying the next");
                self.failed_decoders.push(element);
                self.decoder = None;
                self.waiting_keyframe = true;
                return;
            }
        }
        // One that was working gets the usual retry, until it keeps failing.
        if self.give_up_running(&detail, cause) {
            return;
        }
        tracing::warn!(binding = self.binding, %detail, "Object detection failed; retrying");
        self.decoder = None;
        self.waiting_keyframe = true;
        self.retry_at = Some(Instant::now() + RETRY_AFTER);
        self.state = BindingState::Error;
        self.detail = Some(detail);
        self.reset_detection();
    }

    /// A failure of a decoder or GPU scaler that had produced pictures: its
    /// second within [`decoding::RUNNING_WINDOW`] gives it up and the next
    /// takes over at once. True when it did.
    fn give_up_running(&mut self, detail: &str, cause: Cause) -> bool {
        enum Culprit {
            Scaler(Scaler),
            Decoder(&'static str),
        }
        let culprit = match (cause, self.current_scaler, self.current_decoder) {
            (Cause::Scaler, Some(scaler), _) => Culprit::Scaler(scaler),
            (Cause::Decoder, _, Some(element)) if self.current_hardware => {
                Culprit::Decoder(element)
            }
            _ => return false,
        };
        let key = match culprit {
            Culprit::Scaler(scaler) => scaler.element(),
            Culprit::Decoder(element) => element,
        };
        if !self.running_failures.note(key, Instant::now()) {
            return false;
        }
        match culprit {
            Culprit::Scaler(scaler) => {
                tracing::warn!(binding = self.binding, scaler = key, %detail, "GPU scaler kept failing while running; scaling on the CPU");
                self.failed_scalers.push(scaler);
                self.scaler_note = Some(scaling::given_up_note(scaler, true));
            }
            Culprit::Decoder(element) => {
                tracing::warn!(binding = self.binding, decoder = key, %detail, "Decoder kept failing while running; trying the next");
                self.failed_decoders.push(element);
                self.gave_up = Some(element);
            }
        }
        self.decoder = None;
        self.current_decoder = None;
        self.current_scaler = None;
        self.waiting_keyframe = true;
        self.reset_detection();
        true
    }

    fn pictures(&mut self) {
        let Some(decoder) = self.decoder.as_ref() else {
            return;
        };
        if let Some((problem, cause)) = decoder.failed() {
            self.fail(problem, cause);
            return;
        }
        loop {
            let pulled = match self.decoder.as_ref().map(Decoder::pull) {
                Some(Ok(Some(picture))) => picture,
                Some(Ok(None)) | None => return,
                Some(Err(error)) => {
                    self.fail(error.to_string(), Cause::Input);
                    return;
                }
            };
            self.picture(pulled);
        }
    }

    fn picture(&mut self, (pixels, width, height, time_ms, seq): AnalyzedPicture) {
        if self.raw_layout.is_none() {
            self.decoder_trial = None; // it works
            self.last_picture = Instant::now();
        }
        self.picture_seq = seq;
        self.frames += 1;
        self.measured += 1;
        self.detail = if self.raw_layout.is_none() {
            let notes: Vec<&str> = [&self.decoder_note, &self.scaler_note]
                .into_iter()
                .filter_map(|note| note.as_deref())
                .collect();
            (!notes.is_empty()).then(|| notes.join("; "))
        } else {
            None
        };
        if self.moving {
            self.state = BindingState::Settling;
            self.clear_layer(OBJECTS);
            self.clear_layer(ZONES);
            return;
        }
        self.state = BindingState::Running;
        let Some(now_ms) = time_ms else {
            return;
        };
        if self.last_ms.is_some_and(|last| now_ms < last) {
            // Timestamps went back (a new timeline): start again.
            self.reset_detection();
        }
        self.last_ms = Some(now_ms);
        let thresholds = self.settings.thresholds(&self.model.classes);
        let found = self
            .model
            .detect_classes(&pixels, width, height, &thresholds);
        self.detected(now_ms, found);
    }

    /// What the model found on the analysed picture (`picture_seq`): its
    /// events and its drawings, from that picture's objects only.
    fn detected(&mut self, now_ms: i64, found: anyhow::Result<Vec<detector::Detection>>) {
        match found {
            Ok(found) => {
                let kept = detector::keep(found, &self.settings);
                self.objects.observe(&kept, &self.settings, now_ms);
                let present = self.objects.presence(&self.settings, now_ms);
                let actions = self.activity.observe(&present, now_ms);
                self.emit(actions);
                let drawn = drawing::objects(&self.objects, &self.settings, now_ms);
                self.draw(OBJECTS, drawn);
                self.draw(ZONES, drawing::zones(&self.settings));
            }
            Err(error) => tracing::debug!(binding = self.binding, %error, "Picture skipped"),
        }
    }

    /// Send what `layer` shows for the analysed picture (`picture_seq`): every
    /// picture, with `items: []` when there is nothing to draw, so viewers can
    /// follow this plugin's picture clock. Without a picture identity an empty
    /// layer would only mean "clear", so it is sent once, while something shows.
    fn draw(&mut self, layer: &str, items: Vec<OverlayItem>) {
        let slot = usize::from(layer == ZONES);
        if items.is_empty() && self.picture_seq.is_none() && !self.drawn[slot] {
            return;
        }
        self.drawn[slot] = !items.is_empty() || self.picture_seq.is_some();
        self.out.send(Message::notification(
            method::OVERLAY,
            Overlay {
                binding: self.binding,
                layer: layer.into(),
                items,
                seq: self.picture_seq,
                ttl_ms: Some(drawing_ttl_ms(self.settings.analysis_fps)),
            },
        ));
    }

    /// Clear `layer` (a run resets): an empty overlay without a picture, sent
    /// once, while the layer shows anything.
    fn clear_layer(&mut self, layer: &str) {
        let slot = usize::from(layer == ZONES);
        if !self.drawn[slot] {
            return;
        }
        self.drawn[slot] = false;
        self.out.send(Message::notification(
            method::OVERLAY,
            Overlay {
                binding: self.binding,
                layer: layer.into(),
                items: vec![],
                seq: None,
                ttl_ms: Some(drawing_ttl_ms(self.settings.analysis_fps)),
            },
        ));
    }

    fn finish_activity(&mut self) {
        let actions = self.activity.finish();
        self.emit(actions);
    }

    fn emit(&mut self, actions: Vec<Action>) {
        for action in actions {
            let message = match action {
                Action::Start {
                    event: local,
                    kind,
                    score,
                    alert,
                } => {
                    let event = self.events.fetch_add(1, Ordering::Relaxed) + 1;
                    self.event.insert(local, event);
                    // `alert` is newer than the bundled SDK copy's `EventStart`:
                    // the same fields as JSON, so it builds against either.
                    let mut params = serde_json::json!({
                        "binding": self.binding, "event": event, "kind": kind,
                        "score": score, "alert": alert,
                    });
                    if let Some(seq) = self.picture_seq {
                        params["seq"] = seq.into();
                    }
                    Message::notification(method::EVENT_START, params)
                }
                Action::Update {
                    event: local,
                    peak,
                    bbox,
                    zones,
                    alert,
                } => {
                    let Some(event) = self.event.get(&local).copied() else {
                        continue;
                    };
                    // As `EventUpdate`, with `alert` (see the start above).
                    let mut params = serde_json::json!({
                        "binding": self.binding, "event": event, "score": peak, "alert": alert,
                    });
                    if let Some(bbox) = bbox {
                        params["bbox"] = serde_json::json!(bbox);
                    }
                    if !zones.is_empty() {
                        params["zones"] = serde_json::json!(zones);
                    }
                    Message::notification(method::EVENT_UPDATE, params)
                }
                Action::End { event: local } => {
                    let Some(event) = self.event.remove(&local) else {
                        continue;
                    };
                    Message::notification(
                        method::EVENT_END,
                        EventEnd {
                            binding: self.binding,
                            event,
                        },
                    )
                }
            };
            self.out.send(message);
        }
    }

    fn report(&mut self) {
        let elapsed = self.meter.elapsed();
        if elapsed >= Duration::from_secs(1) {
            self.fps = self.measured as f32 / elapsed.as_secs_f32();
            self.measured = 0;
            self.meter = Instant::now();
        }
        if self.reported.elapsed() < STATUS_EVERY {
            return;
        }
        self.reported = Instant::now();
        if self.decoder.is_none() && self.state == BindingState::Running {
            self.state = BindingState::Waiting;
        }
        let decoder = if self.raw_layout.is_some() {
            Some(scaling::status_line(
                "NeuroWatch's pictures (no decoding here)",
                false,
            ))
        } else {
            self.decoder
                .as_ref()
                .and_then(|d| d.name.lock().ok().and_then(|name| name.clone()))
                .map(|name| {
                    scaling::status_line(
                        &decoding::status_text(&name, self.decoder_note.as_deref()),
                        self.current_scaler.is_some(),
                    )
                })
        };
        let status = Status {
            bindings: vec![BindingStatus {
                binding: self.binding,
                state: self.state,
                detail: self.detail.clone(),
                fps: self.fps,
                frames: self.frames,
                decoder,
                figures: vec![],
            }],
        };
        self.out.send(Message::notification(method::STATUS, status));
    }
}

/// Copy `rows` rows of `width` bytes between two strided buffers; `source` and
/// `destination` are `(first byte, row stride)`. A picture whose declared
/// shape does not fit its bytes is an error, never an out-of-range panic.
fn copy_rows(
    source: &[u8],
    (source_start, source_stride): (usize, usize),
    destination: &mut [u8],
    (destination_start, destination_stride): (usize, usize),
    (width, rows): (usize, usize),
) -> anyhow::Result<()> {
    for row in 0..rows {
        let from = row
            .checked_mul(source_stride)
            .and_then(|at| at.checked_add(source_start))
            .and_then(|at| source.get(at..at.checked_add(width)?));
        let to = row
            .checked_mul(destination_stride)
            .and_then(|at| at.checked_add(destination_start))
            .and_then(|at| destination.get_mut(at..at.checked_add(width)?));
        match (from, to) {
            (Some(from), Some(to)) => to.copy_from_slice(from),
            _ => anyhow::bail!("picture rows do not fit their buffer"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod rate_tests {
    use super::RateLimit;

    #[test]
    fn the_rate_limit_holds_whatever_the_timestamps_start_at() {
        for start_ns in [0, 600_000_000_000, 86_400_000_000_000] {
            let mut limit = RateLimit::new(5);
            let passed = (0..250u64)
                .filter(|i| limit.admit(start_ns + i * 1_000_000_000 / 25))
                .count();
            assert_eq!(passed, 50, "10 s of 25 fps video at 5 fps, from {start_ns}");
        }
        // Keyframes a second apart (low CPU) are all kept.
        let mut limit = RateLimit::new(5);
        let passed = (0..10u64)
            .filter(|i| limit.admit(i * 1_000_000_000))
            .count();
        assert_eq!(passed, 10);
    }

    #[test]
    fn the_rate_limit_survives_a_gap_and_a_new_timeline() {
        let mut limit = RateLimit::new(5);
        assert!(limit.admit(0));
        assert!(!limit.admit(100_000_000));
        // A gap of a minute: no burst to catch up afterwards.
        assert!(limit.admit(60_000_000_000));
        assert!(!limit.admit(60_040_000_000));
        // The camera restarted: timestamps from zero again.
        assert!(limit.admit(0));
        assert!(!limit.admit(40_000_000));
        assert!(limit.admit(200_000_000));
    }
}

#[cfg(test)]
mod shape_tests {
    use super::copy_rows;

    #[test]
    fn rows_are_copied_between_strides() {
        let source = [1, 2, 3, 0, 4, 5, 6, 0];
        let mut packed = [0u8; 6];
        copy_rows(&source, (0, 4), &mut packed, (0, 3), (3, 2)).unwrap();
        assert_eq!(packed, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn a_picture_shorter_than_its_shape_is_refused_not_a_panic() {
        let mut destination = [0u8; 64];
        assert!(copy_rows(&[1, 2, 3], (0, 4), &mut destination, (0, 4), (4, 4)).is_err());
        assert!(copy_rows(&[0; 64], (0, 4), &mut destination[..6], (0, 4), (4, 4)).is_err());
        assert!(copy_rows(&[0; 64], (usize::MAX, 4), &mut destination, (0, 4), (4, 2)).is_err());
        assert!(copy_rows(&[0; 64], (0, usize::MAX), &mut destination, (0, 4), (4, 2)).is_err());
    }
}

#[cfg(test)]
mod timeline_tests {
    use super::RateLimit;

    #[test]
    fn extreme_timestamps_do_not_overflow() {
        let mut limit = RateLimit::new(5);
        limit.admit(0);
        limit.admit(u64::MAX - 1);
        limit.admit(u64::MAX);
        limit.admit(0);
    }
}

#[cfg(test)]
mod pipeline_tests {
    use super::*;

    use nw_analytics_protocol::frame::{flags, MediaKind};

    fn ready() -> bool {
        gstreamer::init().is_ok()
            && gstreamer::ElementFactory::find("x264enc").is_some()
            && gstreamer::ElementFactory::find("avdec_h264").is_some()
    }

    /// A real 1280×720 H.264 clip, as the units NeuroWatch would send.
    fn clip() -> Vec<MediaUnit> {
        let pipeline = gstreamer::parse::launch(
            "videotestsrc num-buffers=60 ! video/x-raw,format=I420,width=1280,height=720,framerate=25/1 \
             ! x264enc tune=zerolatency key-int-max=10 bframes=0 \
             ! video/x-h264,profile=main ! h264parse ! video/x-h264,stream-format=byte-stream,alignment=au \
             ! appsink name=out sync=false",
        )
        .unwrap()
        .downcast::<gstreamer::Pipeline>()
        .unwrap();
        let sink = pipeline
            .by_name("out")
            .unwrap()
            .downcast::<gstreamer_app::AppSink>()
            .unwrap();
        pipeline.set_state(gstreamer::State::Playing).unwrap();
        let mut units = vec![];
        while let Ok(sample) = sink.pull_sample() {
            let buffer = sample.buffer().unwrap();
            let map = buffer.map_readable().unwrap();
            units.push(MediaUnit {
                media: MediaKind::Video,
                flags: if buffer.flags().contains(gstreamer::BufferFlags::DELTA_UNIT) {
                    0
                } else {
                    flags::KEYFRAME
                },
                binding: 1,
                seq: units.len() as u64,
                time_us: 1_000_000 + units.len() as i64 * 40_000,
                pts_ns: buffer.pts().map(|pts| pts.nseconds()),
                dts_ns: buffer.dts().map(|dts| dts.nseconds()),
                data: map.as_slice().to_vec(),
            });
        }
        let _ = pipeline.set_state(gstreamer::State::Null);
        units
    }

    fn format() -> StreamFormat {
        StreamFormat {
            binding: 1,
            codec: Codec::H264,
            width: 1280,
            height: 720,
        }
    }

    /// Decode the clip on `element`; the first analysed picture.
    fn first_picture(element: &str, gpu: Option<GpuScale>) -> anyhow::Result<AnalyzedPicture> {
        let decoder = Decoder::new(&format(), element, 5, gpu)?;
        let units = clip();
        assert!(units.len() > 10, "the clip has pictures");
        for unit in &units {
            decoder.push(unit, 1_000_000)?;
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            if let Some((problem, _)) = decoder.failed() {
                anyhow::bail!(problem);
            }
            if let Some(picture) = decoder.pull()? {
                return Ok(picture);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        anyhow::bail!("no picture")
    }

    #[test]
    fn errors_are_blamed_on_what_posted_them() {
        for (path, cause) in [
            ("/GstPipeline:pipeline0/GstVaPostProc:scale", Cause::Scaler),
            (
                "/GstPipeline:pipeline0/GstVaPostProc:scale:src",
                Cause::Scaler,
            ),
            (
                "/GstPipeline:pipeline0/GstCudaDownload:download",
                Cause::Scaler,
            ),
            ("/GstPipeline:pipeline0/GstVaH264Dec:decode", Cause::Decoder),
            (
                "/GstPipeline:pipeline0/GstVaH264Dec:decode:sink",
                Cause::Decoder,
            ),
            (
                "/GstPipeline:pipeline0/GstH264Parse:h264parse0",
                Cause::Input,
            ),
            ("/GstPipeline:pipeline0/GstAppSrc:src", Cause::Input),
            (
                "/GstPipeline:pipeline0/GstVideoConvert:videoconvert0",
                Cause::Input,
            ),
        ] {
            assert_eq!(Cause::of_source(path), cause, "{path}");
        }
    }

    #[test]
    fn the_cpu_decodes_and_scales_a_real_clip() {
        if !ready() {
            eprintln!("skipped: x264enc/avdec_h264 not available");
            return;
        }
        let (pixels, width, height, _, _) = first_picture("avdec_h264", None).unwrap();
        let _ = &pixels;
        assert_eq!((width, height), (ANALYSIS_WIDTH, 234));
    }

    #[test]
    fn a_missing_scaler_means_cpu_scaling_on_the_same_decoder() {
        if !ready() {
            eprintln!("skipped: x264enc/avdec_h264 not available");
            return;
        }
        let installed = |name: &str| gstreamer::ElementFactory::find(name).is_some();
        // The decoder of this machine, if it has a hardware one with no scaler here.
        for decoder in ["vah264dec", "nvh264dec", "d3d11h264dec", "d3d12h264dec"] {
            if !installed(decoder) {
                continue;
            }
            let gpu = scaling::plan(decoder, (1280, 720), TARGET.0, TARGET.1, &[], &installed);
            match gpu {
                Some(gpu) => {
                    eprintln!(
                        "{decoder}: scaler {} is here (not exercised)",
                        gpu.scaler.element()
                    );
                }
                None => {
                    eprintln!("{decoder}: no GPU scaler here, scaling on the CPU");
                    let (pixels, width, height, _, _) = first_picture(decoder, None).unwrap();
                    let _ = &pixels;
                    assert_eq!((width, height), (ANALYSIS_WIDTH, 234));
                }
            }
        }
    }

    #[test]
    fn a_scaler_that_cannot_be_built_is_an_error_not_a_hang() {
        if !ready() {
            return;
        }
        // A scaler the registry lacks: the pipeline does not build.
        if gstreamer::ElementFactory::find("vapostproc").is_none() {
            let gpu = GpuScale {
                scaler: Scaler::Va,
                width: 416,
                height: 234,
            };
            assert!(Decoder::new(&format(), "avdec_h264", 5, Some(gpu)).is_err());
        }
    }

    #[test]
    fn the_scalers_pipeline_text_parses_where_its_elements_exist() {
        if gstreamer::init().is_err() {
            return;
        }
        for scaler in Scaler::ALL {
            let installed = |name: &str| gstreamer::ElementFactory::find(name).is_some();
            if scaling::scaler_for(
                match scaler {
                    Scaler::Va => "vah264dec",
                    Scaler::Cuda => "nvh264dec",
                    Scaler::D3d11 => "d3d11h264dec",
                    Scaler::D3d12 => "d3d12h264dec",
                },
                &[],
                &installed,
            )
            .is_none()
            {
                eprintln!("{} is not installed here: not parsed", scaler.element());
                continue;
            }
            let text = format!("fakesrc ! {} ! fakesink", scaler.segment(320, 180));
            gstreamer::parse::launch(&text).unwrap_or_else(|error| panic!("{text}: {error}"));
        }
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::*;

    struct Quiet;
    impl Sender for Quiet {
        fn send(&self, _: Message) {}
    }

    fn run() -> Run {
        Run::new(
            1,
            DetectSettings::default(),
            detector::shared().unwrap(),
            Arc::new(AtomicU64::new(0)),
            Arc::new(Quiet),
        )
    }

    /// A hardware decoder, with a GPU scaler or not, as `unit` leaves it.
    fn started(run: &mut Run, scaler: Option<Scaler>, trial: bool) {
        run.current_decoder = Some("vah264dec");
        run.current_hardware = true;
        run.current_scaler = scaler;
        run.decoder_trial = trial.then(Instant::now);
    }

    struct Recorded(std::sync::Mutex<Vec<(String, Option<u64>, usize)>>);
    impl Sender for Recorded {
        fn send(&self, message: Message) {
            if let Message::Notification { params, .. } = message {
                if let Ok(mut all) = self.0.lock() {
                    all.push((
                        params["layer"].as_str().unwrap_or_default().to_owned(),
                        params["seq"].as_u64(),
                        params["items"].as_array().map_or(0, Vec::len),
                    ));
                }
            }
        }
    }

    #[test]
    fn every_analysed_picture_sends_its_layer_even_when_empty() {
        let sent = Arc::new(Recorded(Default::default()));
        let mut run = Run::new(
            1,
            DetectSettings::default(),
            detector::shared().unwrap(),
            Arc::new(AtomicU64::new(0)),
            sent.clone(),
        );
        let layer = OBJECTS;
        let log = || sent.0.lock().unwrap().clone();
        run.picture_seq = Some(4);
        run.draw(layer, vec![]);
        run.picture_seq = Some(5);
        run.draw(layer, vec![]);
        assert_eq!(
            log(),
            vec![
                (layer.to_owned(), Some(4), 0),
                (layer.to_owned(), Some(5), 0)
            ],
            "an empty result for each picture, not suppressed"
        );
        run.clear_layer(layer);
        assert_eq!(
            log()[2],
            (layer.to_owned(), None, 0),
            "a reset clears, without a picture"
        );
        run.clear_layer(layer);
        assert_eq!(log().len(), 3, "cleared once");
        // Without a picture identity an empty layer is only a clear: not repeated.
        run.picture_seq = None;
        run.draw(layer, vec![]);
        assert_eq!(log().len(), 3);
    }

    /// Every box drawn, with its layer and picture.
    type Drawn = Vec<(String, Option<u64>, Vec<[f32; 4]>)>;
    struct Boxes(std::sync::Mutex<Drawn>);
    impl Sender for Boxes {
        fn send(&self, message: Message) {
            if let Message::Notification { params, .. } = message {
                let Some(layer) = params["layer"].as_str() else {
                    return; // an event, not a drawing
                };
                let boxes = params["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|item| item["type"] == "box")
                    .filter_map(|item| {
                        serde_json::from_value::<[f32; 4]>(item["rect"].clone()).ok()
                    })
                    .collect();
                if let Ok(mut all) = self.0.lock() {
                    all.push((layer.to_owned(), params["seq"].as_u64(), boxes));
                }
            }
        }
    }

    fn overlaps(a: [f32; 4], b: [f32; 4]) -> bool {
        a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
    }

    /// The contract every plugin keeps (each tests
    /// it): after a restart (its picture source switching, as the video wall
    /// going between a grid and one camera does) every box drawn for a picture
    /// sits on something in that picture. Detect draws each picture's own
    /// objects; this keeps it so if it ever holds or smooths boxes (Motion's
    /// learned background drew where an object had been, 2026-11-23).
    #[test]
    fn after_a_restart_every_box_is_on_something_in_its_picture() {
        let sent = Arc::new(Boxes(Default::default()));
        let settings = DetectSettings {
            classes: vec![crate::settings::ClassChoice::new("person", 50)],
            ..DetectSettings::default()
        };
        let mut run = Run::new(
            1,
            settings,
            detector::shared().unwrap(),
            Arc::new(AtomicU64::new(0)),
            sent.clone(),
        );
        let person = |x: f32| {
            let rect = [x, 0.3, 0.1, 0.4];
            let detection = detector::Detection {
                class: 0,
                id: "person".into(),
                label: "Person".into(),
                score: 0.9,
                rect,
            };
            (rect, vec![detection])
        };
        let mut at = 0;
        for step in 0..8 {
            run.picture_seq = Some(step);
            run.detected(at, Ok(person(0.1 + 0.02 * step as f32).1));
            at += 200;
        }
        run.restart();
        let mut objects = std::collections::HashMap::new();
        for step in 0..8 {
            let (rect, found) = person(0.6 + 0.02 * step as f32);
            let seq = 100 + step;
            objects.insert(seq, rect);
            run.picture_seq = Some(seq);
            run.detected(at, Ok(found));
            at += 200;
        }
        let after: Vec<_> = sent
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, seq, _)| seq.is_some_and(|seq| seq >= 100))
            .cloned()
            .collect();
        assert!(
            after.iter().any(|(_, _, boxes)| !boxes.is_empty()),
            "the person was boxed after the restart"
        );
        for (layer, seq, boxes) in &after {
            let object = objects[&seq.unwrap()];
            for rect in boxes {
                assert!(
                    overlaps(*rect, object),
                    "{layer} box {rect:?} on picture {seq:?}, the person is at {object:?}"
                );
            }
        }
    }

    /// Every event message sent, as its method and parameters.
    struct Sent(std::sync::Mutex<Vec<(String, serde_json::Value)>>);
    impl Sender for Sent {
        fn send(&self, message: Message) {
            if let Message::Notification { method, params } = message {
                if method.starts_with("event.") {
                    self.0.lock().unwrap().push((method, params));
                }
            }
        }
    }

    #[test]
    fn events_say_whether_they_are_alerts_and_older_recorders_still_read_them() {
        let sent = Arc::new(Sent(Default::default()));
        let settings = DetectSettings {
            classes: vec![
                crate::settings::ClassChoice::new("person", 50),
                crate::settings::ClassChoice::new("dog", 50),
            ],
            min_duration_sec: 0.0,
            ..DetectSettings::default()
        };
        let mut run = Run::new(
            1,
            settings,
            detector::shared().unwrap(),
            Arc::new(AtomicU64::new(0)),
            sent.clone(),
        );
        let seen = |class: usize, id: &str, x: f32| detector::Detection {
            class,
            id: id.into(),
            label: id.into(),
            score: 0.9,
            rect: [x, 0.3, 0.1, 0.3],
        };
        for (step, at) in [0i64, 500].into_iter().enumerate() {
            run.picture_seq = Some(step as u64);
            run.detected(at, Ok(vec![seen(0, "person", 0.1), seen(16, "dog", 0.6)]));
        }
        let all = sent.0.lock().unwrap().clone();
        let starts: Vec<_> = all
            .iter()
            .filter(|(m, _)| m == method::EVENT_START)
            .collect();
        let alert_of = |kind: &str| {
            starts
                .iter()
                .find(|(_, p)| p["kind"] == kind)
                .map(|(_, p)| p["alert"].clone())
        };
        assert_eq!(alert_of("person"), Some(serde_json::json!(true)), "{all:?}");
        assert_eq!(
            alert_of("dog"),
            Some(serde_json::json!(false)),
            "a dog is a detection"
        );
        for (method, params) in &all {
            match method.as_str() {
                method::EVENT_START => {
                    let parsed: nw_analytics_protocol::messages::EventStart =
                        serde_json::from_value(params.clone()).unwrap();
                    assert_eq!(parsed.seq, Some(1));
                }
                method::EVENT_UPDATE => {
                    serde_json::from_value::<nw_analytics_protocol::messages::EventUpdate>(
                        params.clone(),
                    )
                    .unwrap();
                }
                _ => {}
            }
        }
    }

    #[test]
    fn a_scaler_that_gives_no_picture_is_dropped_before_the_decoder() {
        let mut run = run();
        started(&mut run, Some(Scaler::Va), true);
        run.fail("not negotiated".into(), Cause::Decoder);
        assert_eq!(run.failed_scalers, vec![Scaler::Va]);
        assert!(run.failed_decoders.is_empty(), "the decoder is not blamed");
        assert_eq!(run.current_scaler, None);
        assert_eq!(
            run.scaler_note.as_deref(),
            Some("Scaling on the CPU: vapostproc could not scale this video")
        );
        assert!(run.retry_at.is_none(), "rebuilt at once, no wait");
        // The same decoder again, now with CPU scaling; it fails too.
        started(&mut run, None, true);
        run.fail("no pictures".into(), Cause::Decoder);
        assert_eq!(
            run.failed_decoders,
            vec!["vah264dec"],
            "only now the decoder moves on"
        );
        assert_eq!(run.failed_scalers, vec![Scaler::Va]);
    }

    #[test]
    fn a_running_decoder_is_given_up_on_its_second_failure() {
        let mut run = run();
        started(&mut run, None, false);
        run.fail("hiccup".into(), Cause::Decoder);
        assert!(run.failed_decoders.is_empty(), "the first is a retry");
        assert!(run.retry_at.is_some());
        started(&mut run, None, false);
        run.fail("again".into(), Cause::Decoder);
        assert_eq!(run.failed_decoders, vec!["vah264dec"]);
        assert_eq!(run.gave_up, Some("vah264dec"));
    }

    #[test]
    fn input_problems_never_count_against_a_decoder() {
        let mut run = run();
        for _ in 0..5 {
            started(&mut run, Some(Scaler::Va), false);
            run.fail("a bad picture".into(), Cause::Input);
        }
        assert!(run.failed_decoders.is_empty());
        assert!(run.failed_scalers.is_empty());
        // A software decoder's failures are never given up, whoever's fault.
        let mut run = self::run();
        for _ in 0..5 {
            run.current_decoder = Some("avdec_h264");
            run.current_hardware = false;
            run.fail("broken".into(), Cause::Decoder);
        }
        assert!(run.failed_decoders.is_empty());
    }

    #[test]
    fn a_running_scaler_that_keeps_failing_is_dropped_not_the_decoder() {
        let mut run = run();
        started(&mut run, Some(Scaler::Va), false);
        run.fail("scale error".into(), Cause::Scaler);
        started(&mut run, Some(Scaler::Va), false);
        run.fail("scale error".into(), Cause::Scaler);
        assert_eq!(run.failed_scalers, vec![Scaler::Va]);
        assert!(run.failed_decoders.is_empty());
        assert_eq!(
            run.scaler_note.as_deref(),
            Some("Scaling on the CPU: vapostproc kept failing while running")
        );
    }
}
