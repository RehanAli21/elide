use std::process::Command;

mod cli;

use anyhow::{bail, Context, Result};
use clap::Parser;
use cli::Cli;
use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct Probe {
    format: Format,
    streams: Vec<Stream>,
}

#[derive(Deserialize, Debug)]
struct Format {
    duration: String,
}

#[derive(Deserialize, Debug)]
struct Stream {
    codec_type: String,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    sample_rate: Option<String>,
}

fn ffprobe_json(input: &str) -> Result<String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(input)
        .output()
        .context("coulff not run ffprobe. Is fffmpeg installed and on PATH?")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("ffffprobe failed on {input}: {}", stderr.trim());
    }

    String::from_utf8(out.stdout).context("ffprobe returned invalid UTF-8")
}

fn parse_fps(s: &str) -> Option<f64> {
    let (num, den) = s.split_once('/')?;
    let num: f64 = num.parse().ok()?;
    let den: f64 = den.parse().ok()?;
    if den == 0.0 {
        return None;
    }
    Some(num / den)
}

fn main() -> Result<()> {
    let args = Cli::parse();

    println!("{:#?}", args);

    let json = ffprobe_json(&args.input)?;

    let probe: Probe = serde_json::from_str(&json)
        .with_context(|| format!("could not parse ffprobe output for {}", args.input))?;

    println!("{:#?}", probe);
    let video = probe
        .streams
        .iter()
        .find(|s| s.codec_type == "video")
        .with_context(|| format!("no video stream in {}", args.input))?;

    let duration: f64 = probe
        .format
        .duration
        .parse()
        .with_context(|| format!("bad duration {:?}", probe.format.duration))?;

    let fps = video
        .r_frame_rate
        .as_deref()
        .and_then(parse_fps)
        .context("could not read frame rate")?;

    println!("{:#?}", fps);

    let width = video.width.context("video stream has no width")?;
    let height = video.height.context("video stream has no height")?;

    println!("duration    {duration:.2} s");
    println!("resolution  {width}x{height}");

    match probe.streams.iter().find(|s| s.codec_type == "audio") {
        Some(a) => match a.sample_rate.as_deref() {
            Some(rate) => println!("audio       {rate} Hz"),
            None => println!("audio       present, sample rate unknown"),
        },
        None => println!("audio       none"),
    }

    Ok(())
}
