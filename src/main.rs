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

fn sample_frames(input: &str, duration: f64) -> Result<Vec<Vec<u8>>> {
    let fps = 200.0 / duration;
    let filter = format!("fps={fps},scale=160:90,format=gray");

    let out = Command::new("ffmpeg")
        .args([
            "-i", input, "-vf", &filter, "-an", "-f", "rawvideo", "-pix_fmt", "gray", "-",
        ])
        .output()
        .context("could not run ffmpeg. Is it installed and on PATH?")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("ffmpeg failed to sample frames: {}", stderr.trim());
    }

    let frames: Vec<Vec<u8>> = out
        .stdout
        .chunks_exact(160 * 90)
        .map(|c| c.to_vec())
        .collect();

    Ok(frames)
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

fn longest_silences(speech: &[bool], n: usize) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
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
        runs.push((start, i));
    }

    runs.sort_by_key(|&(start, end)| std::cmp::Reverse(end - start));
    runs.truncate(n);
    runs
}

fn fmt_time(t: f64) -> String {
    let mins = (t / 60.0).floor();
    let secs = t - mins * 60.0;
    format!("{mins:02.0}:{secs:04.1}")
}

fn cell_activity(frames: &[Vec<u8>]) -> Vec<f32> {
    let cells = 160 * 90;
    let mut sums = vec![0.0f32; cells];

    for pair in frames.windows(2) {
        for c in 0..cells {
            let diff = (pair[1][c] as i16 - pair[0][c] as i16).abs();
            sums[c] += diff as f32;
        }
    }

    let n = (frames.len() - 1) as f32;
    sums.iter().map(|s| s / n).collect()
}
fn content_mask(activity: &[f32]) -> Option<(Vec<bool>, f32)> {
    let max = activity.iter().fold(0.0f32, |m, &a| m.max(a));

    if max < 2.0 {
        return None;
    }

    let threshold = 0.08 * max;
    let mask = activity.iter().map(|&a| a > threshold).collect();
    Some((mask, threshold))
}

fn blobs(mask: &[bool], w: usize, h: usize) -> Vec<Vec<usize>> {
    let mut seen = vec![false; mask.len()];
    let mut out = Vec::new();

    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }

        let mut blob = Vec::new();
        let mut stack = vec![start];
        seen[start] = true;

        while let Some(c) = stack.pop() {
            blob.push(c);
            let (x, y) = (c % w, c / w);

            if x > 0 {
                push_if(&mut stack, &mut seen, mask, c - 1);
            }
            if x + 1 < w {
                push_if(&mut stack, &mut seen, mask, c + 1);
            }
            if y > 0 {
                push_if(&mut stack, &mut seen, mask, c - w);
            }
            if y + 1 < h {
                push_if(&mut stack, &mut seen, mask, c + w);
            }
        }

        out.push(blob);
    }

    out.sort_by_key(|b| std::cmp::Reverse(b.len()));
    out
}

fn push_if(stack: &mut Vec<usize>, seen: &mut [bool], mask: &[bool], c: usize) {
    if mask[c] && !seen[c] {
        seen[c] = true;
        stack.push(c);
    }
}

fn bounding_box(blob: &[usize], w: usize) -> (usize, usize, usize, usize) {
    let mut x0 = usize::MAX;
    let mut y0 = usize::MAX;
    let mut x1 = 0;
    let mut y1 = 0;

    for &c in blob {
        let (x, y) = (c % w, c / w);
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }

    (x0, y0, x1, y1)
}

fn busy_fraction(blob: &[usize], frames: &[Vec<u8>]) -> f64 {
    let mut busy = 0;

    for pair in frames.windows(2) {
        let mut sum = 0.0f64;
        for &c in blob {
            sum += (pair[1][c] as i16 - pair[0][c] as i16).abs() as f64;
        }
        let mean = sum / blob.len() as f64;

        if mean > 0.5 {
            busy += 1;
        }
    }

    busy as f64 / (frames.len() - 1) as f64
}

fn detect_freezes(input: &str, crop: &str) -> Result<Vec<(f64, Option<f64>)>> {
    let filter = format!("crop={crop},fps=5,freezedetect=n=-58dB:d=2.0");

    let out = Command::new("ffmpeg")
        .args(["-i", input, "-vf", &filter, "-an", "-f", "null", "-"])
        .output()
        .context("could not run ffmpeg. Is it installed and on PATH?")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("freezedetect failed: {}", stderr.trim());
    }

    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut freezes: Vec<(f64, Option<f64>)> = Vec::new();

    for line in stderr.lines() {
        if let Some(v) = line.split("freeze_start: ").nth(1) {
            if let Ok(t) = v.trim().parse::<f64>() {
                freezes.push((t, None));
            }
        } else if let Some(v) = line.split("freeze_end: ").nth(1) {
            if let Ok(t) = v.trim().parse::<f64>() {
                if let Some(last) = freezes.last_mut() {
                    last.1 = Some(t);
                }
            }
        }
    }

    Ok(freezes)
}

fn paint_freezes(freezes: &[(f64, Option<f64>)], grid_len: usize, duration: f64) -> Vec<bool> {
    let mut frozen = vec![false; grid_len];

    for &(start, end) in freezes {
        let end = end.unwrap_or(duration);
        let i0 = ((start / 0.02) as usize).min(grid_len - 1);
        let i1 = ((end / 0.02) as usize).min(grid_len);
        for j in i0..i1 {
            frozen[j] = true;
        }
    }

    frozen
}

fn main() -> Result<()> {
    let args = Cli::parse();

    let json = ffprobe_json(&args.input)?;

    let probe: Probe = serde_json::from_str(&json)
        .with_context(|| format!("could not parse ffprobe output for {}", args.input))?;

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

    println!("fps         {fps:.3}");

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

    let gain_db = -23.0 - input_i;

    println!("input_i     {input_i:.2} LUFS");
    println!("input_tp    {input_tp:.2} dBTP");
    println!("gain        {gain_db:+.2} dB");

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

    let total = probs.len();
    let above = probs.iter().filter(|&&p| p >= 0.30).count();
    let below = probs.iter().filter(|&&p| p < 0.15).count();
    let dead = total - above - below;

    println!("chunks      {total}");
    println!("  >= 0.30   {above}");
    println!("  0.15-0.30 {dead}");
    println!("  <  0.15   {below}");

    let chunk_speech = hysteresis(&probs, 0.30, 0.15);

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
    println!("speech:      {pct:.1}%");

    println!("\nlongest silences:");
    for (rank, &(start, end)) in longest_silences(&padded, 10).iter().enumerate() {
        let t0 = start as f64 * 0.02;
        let t1 = end as f64 * 0.02;
        println!(
            "{:3}  {} - {}   {:.1} s",
            rank + 1,
            fmt_time(t0),
            fmt_time(t1),
            t1 - t0
        );
    }

    let frames = sample_frames(&args.input, duration)?;
    println!("frames      {}", frames.len());

    let activity = cell_activity(&frames);
    let max = activity.iter().fold(0.0f32, |m, &a| m.max(a));
    let mean = activity.iter().sum::<f32>() / activity.len() as f32;
    println!("activity    max {max:.2}  mean {mean:.2}");
    let (mask, threshold) = match content_mask(&activity) {
        Some(v) => v,
        None => {
            println!("no content region (max activity below 2.0)");
            // expect_screen = false, skip freeze detection
            return Ok(());
        }
    };

    let passing = mask.iter().filter(|&&b| b).count();
    println!("threshold   {threshold:.2}  ({passing} cells pass)");

    let bs = blobs(&mask, 160, 90);
    println!("blobs       {}", bs.len());

    const MIN_BLOB: usize = 50;
    const MAX_BUSY: f64 = 0.90;

    let mut content: Option<&Vec<usize>> = None;

    for b in &bs {
        if b.len() < MIN_BLOB {
            continue;
        }
        let busy = busy_fraction(b, &frames);
        println!(
            "  {:5} cells   {:.1}% busy{}",
            b.len(),
            100.0 * busy,
            if busy > MAX_BUSY {
                "   [webcam, rejected]"
            } else {
                ""
            }
        );
        if busy > MAX_BUSY {
            continue;
        }
        if content.is_none() {
            content = Some(b);
        }
    }

    let content = match content {
        Some(b) => b,
        None => {
            println!("no content region — all blobs rejected or too small");
            return Ok(());
        }
    };

    let (cx0, cy0, cx1, cy1) = bounding_box(content, 160);

    let sx = width as f64 / 160.0;
    let sy = height as f64 / 90.0;

    let x = ((cx0 as f64 * sx) as usize) & !1;
    let y = ((cy0 as f64 * sy) as usize) & !1;
    let x1 = (((cx1 + 1) as f64 * sx).ceil() as usize).min(width as usize);
    let y1 = (((cy1 + 1) as f64 * sy).ceil() as usize).min(height as usize);
    let cw = (x1 - x + 1) & !1;
    let ch = (y1 - y + 1) & !1;

    println!("crop        {cw}:{ch}:{x}:{y}");

    let crop = format!("{cw}:{ch}:{x}:{y}");
    println!("crop        {crop}");

    let freezes = detect_freezes(&args.input, &crop)?;

    let total: f64 = freezes
        .iter()
        .map(|&(s, e)| e.unwrap_or(duration) - s)
        .sum();

    println!(
        "freezes     {} blocks, {:.1} s frozen ({:.1}%)",
        freezes.len(),
        total,
        100.0 * total / duration
    );

    let frozen = paint_freezes(&freezes, grid_len, duration);

    let frozen_pct = 100.0 * frozen.iter().filter(|&&b| b).count() as f64 / grid_len as f64;
    println!("frozen      {frozen_pct:.1}% of grid");
    let dead = padded
        .iter()
        .zip(&frozen)
        .filter(|&(&s, &f)| !s && f)
        .count();

    println!("dead        {:.1}%", 100.0 * dead as f64 / grid_len as f64);
    Ok(())
}
