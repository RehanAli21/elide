use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;
use std::{fs, path::Path};

mod cli;

use anyhow::{bail, Context, Result};
use clap::Parser;
use cli::Cli;
use hound::WavReader;
use serde::{Deserialize, Serialize};
use voice_activity_detector::{IteratorExt, VoiceActivityDetector};

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

fn hysteresis(probs: &[f32], enter: f32, exit: f32) -> Vec<bool> {
    let mut out = Vec::with_capacity(probs.len());
    let mut speaking = false;

    for &p in probs {
        if speaking {
            if p < exit {
                speaking = false;
            }
        } else if p >= enter {
            speaking = true;
        }

        out.push(speaking);
    }

    out
}

fn bridge(speech: &[bool], max_gap_s: f64) -> Vec<bool> {
    let mut out = speech.to_vec();
    let mut i = 0;

    while i < speech.len() {
        if speech[i] {
            i += 1;
            continue;
        }

        let start = i;
        while i < speech.len() && !speech[i] {
            i += 1;
        }
        let end = i;

        let has_speech_before = start > 0;
        let has_speech_after = end < speech.len();
        let len_s = (end - start) as f64 * 0.02;

        if has_speech_before && has_speech_after && len_s < max_gap_s {
            for j in start..end {
                out[j] = true;
            }
        }
    }

    out
}

fn drop_bursts(speech: &[bool], min_speech_s: f64) -> (Vec<bool>, usize) {
    let mut out = speech.to_vec();
    let mut dropped = 0;
    let mut i = 0;

    while i < speech.len() {
        if !speech[i] {
            i += 1;
            continue;
        }

        let start = i;
        while i < speech.len() && speech[i] {
            i += 1;
        }
        let end = i;

        let len_s = (end - start) as f64 * 0.02;

        if len_s < min_speech_s {
            for j in start..end {
                out[j] = false;
            }
            dropped += 1;
        }
    }

    (out, dropped)
}

fn pad(speech: &[bool], before_s: f64, after_s: f64) -> Vec<bool> {
    let before = (before_s / 0.02) as usize;
    let after = (after_s / 0.02) as usize;

    let mut out = speech.to_vec();
    let mut i = 0;

    while i < speech.len() {
        if !speech[i] {
            i += 1;
            continue;
        }

        let start = i;
        while i < speech.len() && speech[i] {
            i += 1;
        }
        let end = i;

        let from = start.saturating_sub(before);
        let to = (end + after).min(speech.len());

        for j in from..to {
            out[j] = true;
        }
    }

    out
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

    let mut vad = VoiceActivityDetector::builder()
        .sample_rate(16000)
        .chunk_size(512usize)
        .build()
        .context("could not build VAD")?;

    let t0 = Instant::now();
    let probs: Vec<f32> = samples
        .iter()
        .copied()
        .predict(&mut vad)
        .map(|(_check, p)| p)
        .collect();

    let elapsed = t0.elapsed();

    println!("vad         {:.1} s", elapsed.as_secs_f64());
    println!("chunk 1875  {:.4}  (60.0 s, expect high)", probs[1875]);
    println!("chunk 18125 {:.4}  (580.0 s, expect low)", probs[18125]);

    let total = probs.len();
    let above = probs.iter().filter(|&&p| p >= 0.30).count();
    let below = probs.iter().filter(|&&p| p < 0.15).count();
    let dead = total - above - below;

    println!("chunks      {total}");
    println!("  >= 0.30   {above}");
    println!("  0.15-0.30 {dead}");
    println!("  <  0.15   {below}");

    let chunk_speech = hysteresis(&probs, 0.30, 0.15);

    let naive: Vec<bool> = probs.iter().map(|&p| p >= 0.30).collect();

    let transitions = |m: &[bool]| m.windows(2).filter(|w| w[0] != w[1]).count();

    println!(
        "hyst speech {:.1}%",
        100.0 * chunk_speech.iter().filter(|&&b| b).count() as f64 / chunk_speech.len() as f64
    );
    println!(
        "transitions naive {} -> hyst {}",
        transitions(&naive),
        transitions(&chunk_speech)
    );

    let mut speech = vec![false; grid_len];

    for i in 0..grid_len {
        let mid_sample = i * 320 + 160;
        let chunk = mid_sample / 512;

        if chunk < chunk_speech.len() {
            speech[i] = chunk_speech[chunk];
        }
    }

    let grid_speech_pct = 100.0 * speech.iter().filter(|&&b| b).count() as f64 / grid_len as f64;
    println!("grid_len    {grid_len}");
    println!("grid speech {grid_speech_pct:.1}%");

    let bridged = bridge(&speech, 0.35);
    let pct = 100.0 * bridged.iter().filter(|&&b| b).count() as f64 / bridged.len() as f64;
    println!("bridged     {pct:.1}%");

    let (dropped_mask, n_dropped) = drop_bursts(&bridged, 0.25);

    let pct =
        100.0 * dropped_mask.iter().filter(|&&b| b).count() as f64 / dropped_mask.len() as f64;
    println!("dropped     {pct:.1}%  ({n_dropped} bursts)");

    let padded = pad(&dropped_mask, 0.50, 0.55);
    let pct = 100.0 * padded.iter().filter(|&&b| b).count() as f64 / padded.len() as f64;
    println!("padded      {pct:.1}%");

    Ok(())
}
