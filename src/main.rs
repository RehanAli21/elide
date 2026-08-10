use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;

use elide::audio::{extract_audio, measure_loudness, Analysis};
use elide::cli::Cli;
use elide::constants::{
    ACTIVITY_MIN, BRIDGE_GAP_S, GRID_H, GRID_S, GRID_W, MAX_BUSY, MIN_BLOB, MIN_SPEECH_S,
    PAD_AFTER_S, PAD_BEFORE_S, SAMPLES_PER_SLICE, SAMPLE_RATE, TARGET_LUFS, VAD_CHUNK, VAD_ENTER,
    VAD_EXIT,
};
use elide::crop::{blobs, bounding_box, busy_fraction, cell_activity, content_mask, sample_frames};
use elide::freeze::{detect_freezes, paint_freezes};
use elide::probe::{ffprobe_json, parse_fps, Probe};
use elide::vad::{bridge, drop_bursts, hysteresis, longest_silences, pad};

use hound::WavReader;

use voice_activity_detector::{IteratorExt, VoiceActivityDetector};

fn fmt_time(t: f64) -> String {
    let mins = (t / 60.0).floor();
    let secs = t - mins * 60.0;
    format!("{mins:02.0}:{secs:04.1}")
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

    let gain_db = TARGET_LUFS - input_i;

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

    let grid_len = samples.len() / SAMPLES_PER_SLICE;

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
        .sample_rate(SAMPLE_RATE)
        .chunk_size(VAD_CHUNK)
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
    let above = probs.iter().filter(|&&p| p >= VAD_ENTER).count();
    let below = probs.iter().filter(|&&p| p < VAD_EXIT).count();
    let dead = total - above - below;

    println!("chunks      {total}");
    println!("  >= {VAD_ENTER:.2}   {above}");
    println!("  {VAD_EXIT:.2}-{VAD_ENTER:.2} {dead}");
    println!("  <  {VAD_EXIT:.2}   {below}");

    let chunk_speech = hysteresis(&probs, VAD_ENTER, VAD_EXIT);

    let mut speech = vec![false; grid_len];

    for (i, slot) in speech.iter_mut().enumerate() {
        let mid_sample = i * 320 + 160;
        let chunk = mid_sample / VAD_CHUNK;

        if chunk < chunk_speech.len() {
            *slot = chunk_speech[chunk];
        }
    }

    let grid_speech_pct = 100.0 * speech.iter().filter(|&&b| b).count() as f64 / grid_len as f64;
    println!("grid_len    {grid_len}");
    println!("grid speech {grid_speech_pct:.1}%");

    let bridged = bridge(&speech, BRIDGE_GAP_S);
    let pct = 100.0 * bridged.iter().filter(|&&b| b).count() as f64 / bridged.len() as f64;
    println!("bridged     {pct:.1}%");

    let (dropped_mask, n_dropped) = drop_bursts(&bridged, MIN_SPEECH_S);

    let pct =
        100.0 * dropped_mask.iter().filter(|&&b| b).count() as f64 / dropped_mask.len() as f64;
    println!("dropped     {pct:.1}%  ({n_dropped} bursts)");

    let padded = pad(&dropped_mask, PAD_BEFORE_S, PAD_AFTER_S);
    let pct = 100.0 * padded.iter().filter(|&&b| b).count() as f64 / padded.len() as f64;
    println!("speech:      {pct:.1}%");

    println!("\nlongest silences:");
    for (rank, &(start, end)) in longest_silences(&padded, 10).iter().enumerate() {
        let t0 = start as f64 * GRID_S;
        let t1 = end as f64 * GRID_S;
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
            println!("no content region (max activity below {ACTIVITY_MIN})");
            // expect_screen = false, skip freeze detection
            return Ok(());
        }
    };

    let passing = mask.iter().filter(|&&b| b).count();
    println!("threshold   {threshold:.2}  ({passing} cells pass)");

    let bs = blobs(&mask, GRID_W, GRID_H);
    println!("blobs       {}", bs.len());

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

    let (cx0, cy0, cx1, cy1) = bounding_box(content, GRID_W);

    let sx = width as f64 / GRID_W as f64;
    let sy = height as f64 / GRID_H as f64;

    let x = ((cx0 as f64 * sx) as usize) & !1;
    let y = ((cy0 as f64 * sy) as usize) & !1;
    let x1 = (((cx1 + 1) as f64 * sx).ceil() as usize).min(width as usize);
    let y1 = (((cy1 + 1) as f64 * sy).ceil() as usize).min(height as usize);
    let cw = (x1 - x + 1) & !1;
    let ch = (y1 - y + 1) & !1;

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
