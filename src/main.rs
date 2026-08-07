use std::path::PathBuf;
use std::process::Command;
use std::{fs, path::Path};

mod cli;

use anyhow::{bail, Context, Result};
use clap::Parser;
use cli::Cli;
use hound::WavReader;
use serde::{Deserialize, Serialize};

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

#[derive(Deserialize, Debug)]
struct Loudnorm {
    input_i: String,
    input_tp: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct Analysis {
    gain_db: f64,
    input_i: f64,
    input_tp: f64,
    sample_rate: u32,
    samples: usize,
    grid_len: usize,
    src_duration_s: f64,
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
        .context("could not run ffprobe. Is fffmpeg installed and on PATH?")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("ffprobe failed on {input}: {}", stderr.trim());
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

fn measure_loudness(input: &str) -> Result<(f64, f64)> {
    let out = Command::new("ffmpeg")
        .args([
            "-i",
            input,
            "-af",
            "loudnorm=I=-23:print_format=json",
            "-f",
            "null",
            "-",
        ])
        .output()
        .context("could not run ffmpeg. Is it installed and on PATH?")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("ffmpeg failed to extract audio: {}", stderr.trim());
    }

    let stderr = String::from_utf8_lossy(&out.stderr);
    let start = stderr.find('{').context("no JSON in loudnorm output")?;
    let end = stderr.rfind('}').context("no JSON in loudnorm output")?;
    let block = &stderr[start..=end];
    let parsed: Loudnorm = serde_json::from_str(block).context("could not parse loudnorm JSON")?;

    let input_i: f64 = parsed.input_i.parse().context("bad input_i")?;
    let input_tp: f64 = parsed.input_tp.parse().context("bad input_tp")?;

    Ok((input_i, input_tp))
}

fn extract_audio(input: &str, wav_path: &Path, gain_db: f64) -> Result<()> {
    let volume = format!("volume={gain_db}dB");

    let out = Command::new("ffmpeg")
        .args([
            "-y",
            "-i",
            input,
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-af",
            &volume,
            "-c:a",
            "pcm_f32le",
        ])
        .arg(wav_path)
        .output()
        .context("could not run ffmpeg. Is it installed and on PATH?")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("ffmpeg failed to extract audio: {}", stderr.trim());
    }

    Ok(())
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

    let temp_dir = PathBuf::from(&args.output).join("temp");
    fs::create_dir_all(&temp_dir)
        .with_context(|| format!("could not create {}", temp_dir.display()))?;

    let wav_path = temp_dir.join("a16.wav");

    let (input_i, input_tp) = measure_loudness(&args.input)?;

    println!("input_i: {}, input_tp{}", input_i, input_tp);

    let gain_db = -23.0 - input_i;

    extract_audio(&args.input, &wav_path, gain_db)?;

    let reader = WavReader::open(&wav_path)
        .with_context(|| format!("could not open {}", wav_path.display()))?;

    let spec = reader.spec();
    let samples: Vec<f32> = reader
        .into_samples::<f32>()
        .collect::<Result<Vec<f32>, _>>()?;

    let secs = samples.len() as f64 / spec.sample_rate as f64;

    println!("samples     {}", samples.len());
    println!("rate        {} Hz", spec.sample_rate);
    println!("duration    {secs:.2} s");

    let peak = samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    println!("peak        {peak:.4}");

    let over = samples.iter().filter(|&&s| s.abs() > 1.0).count();
    println!("over 1.0    {over}");

    let grid_len = samples.len() / 320;

    let analysis = Analysis {
        gain_db,
        input_i,
        input_tp,
        sample_rate: spec.sample_rate,
        samples: samples.len(),
        grid_len,
        src_duration_s: duration,
    };

    let path = temp_dir.join("analysis.json");
    fs::write(&path, serde_json::to_string_pretty(&analysis)?)
        .with_context(|| format!("could not write {}", path.display()))?;

    Ok(())
}
