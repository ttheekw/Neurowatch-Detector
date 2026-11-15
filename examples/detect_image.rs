//! Run the detector on one picture and print what it finds:
//! `cargo run --release --example detect_image -- PICTURE [--model PATH] [--min 0.3]`.
//! Any picture GStreamer can decode (JPEG, PNG…), at its own size.
use gstreamer::prelude::*;
use gstreamer_video::prelude::*;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut picture = None;
    let mut model = None;
    let mut min = 0.3f32;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" => model = args.next().map(std::path::PathBuf::from),
            "--min" => min = args.next().unwrap_or_default().parse()?,
            other => picture = Some(other.to_owned()),
        }
    }
    let picture = picture.ok_or_else(|| anyhow::anyhow!("which picture?"))?;
    gstreamer::init()?;
    let pipeline = gstreamer::parse::launch(&format!(
        "filesrc location=\"{picture}\" ! decodebin ! videoconvert ! video/x-raw,format=RGB ! appsink name=sink"
    ))?
    .downcast::<gstreamer::Pipeline>()
    .map_err(|_| anyhow::anyhow!("not a pipeline"))?;
    let sink = pipeline
        .by_name("sink")
        .and_then(|e| e.downcast::<gstreamer_app::AppSink>().ok())
        .ok_or_else(|| anyhow::anyhow!("no appsink"))?;
    pipeline.set_state(gstreamer::State::Playing)?;
    let sample = sink
        .pull_sample()
        .map_err(|_| anyhow::anyhow!("could not decode {picture}"))?;
    let info = gstreamer_video::VideoInfo::from_caps(
        sample.caps().ok_or_else(|| anyhow::anyhow!("no caps"))?,
    )?;
    let buffer = sample
        .buffer()
        .ok_or_else(|| anyhow::anyhow!("no buffer"))?;
    let frame = gstreamer_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)?;
    let (width, height) = (frame.width() as usize, frame.height() as usize);
    let stride = frame.plane_stride()[0] as usize;
    let plane = frame.plane_data(0)?;
    let mut rgb = Vec::with_capacity(width * height * 3);
    for row in 0..height {
        rgb.extend_from_slice(&plane[row * stride..row * stride + width * 3]);
    }
    let _ = pipeline.set_state(gstreamer::State::Null);
    let model = nw_detect::detector::install(model.as_deref())?;
    let started = std::time::Instant::now();
    let found = model.detect(&rgb, width, height, min)?;
    println!(
        "{picture}: {width}×{height}, {} in {} ms",
        model.name,
        started.elapsed().as_millis()
    );
    for d in found {
        println!(
            "  {:<10} {:<8} {:>3.0}%  x {:.2} y {:.2} w {:.2} h {:.2}",
            d.class,
            d.kind.id(),
            d.score * 100.0,
            d.rect[0],
            d.rect[1],
            d.rect[2],
            d.rect[3]
        );
    }
    Ok(())
}
