use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;

use elide::audio::{extract_audio, measure_loudness, Analysis};
use elide::cli::Cli;
use elide::constants::{
    ACTIVITY_MIN, BRIDGE_GAP_S, DEAD_BRIDGE_S, EDGE_MARGIN_S, ENERGY_S, GATE_DB, GRID_H, GRID_S,
    GRID_W, MAX_BUSY, MAX_SHRINK_S, MIN_BLOB, MIN_DEAD_S, MIN_SPEECH_S, PAD_AFTER_S, PAD_BEFORE_S,
    QUIET_DB, SAMPLES_PER_SLICE, SAMPLE_RATE, TARGET_LUFS, VAD_CHUNK, VAD_ENTER, VAD_EXIT,
};
use elide::crop::{blobs, bounding_box, busy_fraction, cell_activity, content_mask, sample_frames};
use elide::freeze::{detect_freezes, paint_freezes};
use elide::probe::{ffprobe_json, parse_fps, Probe};
use elide::utilities::fmt_time;
use elide::vad::{bridge, drop_bursts, hysteresis, longest_silences, pad};
use hound::WavReader;

use serde::Serialize;
use voice_activity_detector::{IteratorExt, VoiceActivityDetector};

fn dead_runs(speech: &[bool], frozen: &[bool]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut i = 0;

    while i < speech.len() {
        if speech[i] || !frozen[i] {
            i += 1;
            continue;
        }

        let start = i;
        while i < speech.len() && !speech[i] && frozen[i] {
            i += 1;
        }
        runs.push((start, i));
    }

    runs
}

fn bridge_dead(runs: &[(usize, usize)], speech: &[bool], max_gap_s: f64) -> Vec<(usize, usize)> {
    if runs.is_empty() {
        return Vec::new();
    }

    // 1. merge runs separated by less than max_gap_s
    let mut merged: Vec<(usize, usize)> = Vec::new();
    let mut cur = runs[0];

    for &(start, end) in &runs[1..] {
        let gap_s = (start - cur.1) as f64 * GRID_S;
        if gap_s < max_gap_s {
            cur.1 = end;
        } else {
            merged.push(cur);
            cur = (start, end);
        }
    }
    merged.push(cur);

    // 2. re-apply "AND not speech" — a merge may have swallowed a word
    let mut out = Vec::new();

    for (start, end) in merged {
        let mut i = start;
        while i < end {
            if speech[i] {
                i += 1;
                continue;
            }
            let s = i;
            while i < end && !speech[i] {
                i += 1;
            }
            out.push((s, i));
        }
    }

    out
}

#[derive(Debug)]
enum Action {
    Keep,
    Collapse { to_s: f64 },
    Speed { factor: f64 },
}

fn decide(len_s: f64) -> Action {
    if len_s < 1.0 {
        Action::Keep
    } else if len_s < 4.0 {
        Action::Collapse { to_s: 0.50 }
    } else {
        let target = (len_s / 12.0).clamp(1.2, 6.0);
        Action::Speed {
            factor: (len_s / target).min(20.0),
        }
    }
}

#[derive(Debug, Serialize)]
struct Segment {
    src_start: f64,
    src_end: f64,
    speed: f64,
}

fn build_segments(runs: &[(usize, usize)], grid_len: usize, src_duration_s: f64) -> Vec<Segment> {
    let mut segs = Vec::new();
    let mut cursor = 0usize;

    for &(start, end) in runs {
        let len_s = (end - start) as f64 * GRID_S;
        let action = decide(len_s);

        if matches!(action, Action::Keep) {
            continue;
        }

        // normal material before this run
        if start > cursor {
            segs.push(Segment {
                src_start: cursor as f64 * GRID_S,
                src_end: start as f64 * GRID_S,
                speed: 1.0,
            });
        }

        match action {
            Action::Collapse { to_s } => segs.push(Segment {
                src_start: start as f64 * GRID_S,
                src_end: start as f64 * GRID_S + to_s,
                speed: 1.0,
            }),
            Action::Speed { factor } => segs.push(Segment {
                src_start: start as f64 * GRID_S,
                src_end: end as f64 * GRID_S,
                speed: factor,
            }),
            Action::Keep => unreachable!(),
        }

        cursor = end;
    }

    // tail
    if cursor < grid_len {
        segs.push(Segment {
            src_start: cursor as f64 * GRID_S,
            src_end: src_duration_s,
            speed: 1.0,
        });
    }

    segs
}

#[derive(Debug, Serialize)]
struct PlanSegment {
    src_start: f64,
    src_end: f64,
    speed: f64,
    out_start: f64,
    out_end: f64,
}

#[derive(Debug, Serialize)]
struct Plan {
    src_duration_s: f64,
    out_duration_s: f64,
    grid_len: usize,
    crop: String,
    segments: Vec<PlanSegment>,
}

fn merge_adjacent(segs: Vec<Segment>) -> Vec<Segment> {
    let mut out: Vec<Segment> = vec![];

    for s in segs {
        match out.last_mut() {
            Some(prev) if prev.speed == s.speed && (prev.src_end - s.src_start).abs() < 1e-9 => {
                prev.src_end = s.src_end;
            }
            _ => out.push(s),
        }
    }

    out
}

fn energy_db(samples: &[f32], sample_rate: u32) -> Vec<f64> {
    let frame = (sample_rate as f64 * ENERGY_S) as usize; // 160 samples
    let n = samples.len() / frame;
    let mut out = Vec::with_capacity(n);

    for i in 0..n {
        let chunk = &samples[i * frame..(i + 1) * frame];
        let sum: f64 = chunk.iter().map(|&s| (s as f64) * (s as f64)).sum();
        let rms = (sum / frame as f64).sqrt();
        out.push(20.0 * rms.max(1e-10).log10());
    }

    out
}

fn smooth(energy: &[f64], window_s: f64) -> Vec<f64> {
    let mut k = (window_s / ENERGY_S) as usize; // 0.03 / 0.01 = 3
    if k.is_multiple_of(2) {
        k += 1; // must be odd, so there's a centre
    }
    let half = k / 2; // 1

    let mut out = Vec::with_capacity(energy.len());

    for i in 0..energy.len() {
        let lo = i.saturating_sub(half); // one before, or 0 at the start
        let hi = (i + half + 1).min(energy.len()); // one after, or the end
        let sum: f64 = energy[lo..hi].iter().sum();
        out.push(sum / (hi - lo) as f64); // average
    }

    out
}

fn trim_edges(runs: &[(usize, usize)], energy: &[f64], gate_db: f64) -> Vec<(usize, usize)> {
    let max_shrink = (MAX_SHRINK_S / ENERGY_S) as usize; // 300 energy frames
    let margin = (EDGE_MARGIN_S / ENERGY_S) as usize; // 15
    let min_dead = (MIN_DEAD_S / ENERGY_S) as usize; // 100

    let mut out = Vec::new();

    for &(a_slice, b_slice) in runs {
        // work in energy-frame indices
        let a = a_slice * 2;
        let b = (b_slice * 2).min(energy.len());

        let mut start = a;
        let mut end = b;

        // leading edge: last loud frame in the first max_shrink frames
        let window_end = (a + max_shrink).min(b);
        let mut last_loud = None;
        for i in a..window_end {
            if energy[i] > gate_db {
                last_loud = Some(i);
            }
        }
        if let Some(i) = last_loud {
            start = (i + margin).min(b - min_dead);
        }

        // trailing edge: first loud frame in the last max_shrink frames
        let window_start = b.saturating_sub(max_shrink).max(start);
        let mut first_loud = None;
        for i in window_start..b {
            if energy[i] > gate_db {
                first_loud = Some(i);
                break;
            }
        }
        if let Some(i) = first_loud {
            end = i.saturating_sub(margin).max(start + min_dead);
        }

        // back to grid slices — never drop
        out.push((start / 2, (end / 2).min(b_slice)));
    }

    out
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
        let mid_sample = i * SAMPLES_PER_SLICE + (SAMPLES_PER_SLICE / 2);
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

    let energy = energy_db(&samples, spec.sample_rate);
    println!(
        "energy      {} frames (grid_len x2 = {})",
        energy.len(),
        grid_len * 2
    );

    let energy_sm = smooth(&energy, 0.03);
    println!(
        "smooth energy      {} frames (grid_len x2 = {})",
        energy_sm.len(),
        grid_len * 2
    );

    let quiet = energy_sm.iter().filter(|&&e| e < QUIET_DB).count();
    let mid = energy_sm
        .iter()
        .filter(|&&e| (QUIET_DB..GATE_DB).contains(&e))
        .count();
    let loud = energy_sm.iter().filter(|&&e| e >= GATE_DB).count();
    let n = energy_sm.len() as f64;

    println!(
        "  < {QUIET_DB:.1}  {quiet}  ({:.1}%)",
        100.0 * quiet as f64 / n
    );
    println!(
        "  {QUIET_DB:.1}..{GATE_DB:.1}  {mid}  ({:.1}%)",
        100.0 * mid as f64 / n
    );
    println!(
        "  >= {GATE_DB:.1}  {loud}  ({:.1}%)",
        100.0 * loud as f64 / n
    );

    let runs = dead_runs(&padded, &frozen);
    println!("dead runs   {} raw", runs.len());

    let runs = bridge_dead(&runs, &padded, DEAD_BRIDGE_S);
    println!("            {} after bridging", runs.len());

    let min_dead_slices = (MIN_DEAD_S / GRID_S) as usize;
    let runs: Vec<_> = runs
        .into_iter()
        .filter(|&(a, b)| b - a >= min_dead_slices)
        .collect();
    println!("            {} after min_dead filter", runs.len());

    let before: usize = runs.iter().map(|&(a, b)| b - a).sum();
    let runs = trim_edges(&runs, &energy_sm, GATE_DB);
    let after: usize = runs.iter().map(|&(a, b)| b - a).sum();
    println!(
        "            {} after edge guard, {:.1}s trimmed",
        runs.len(),
        (before - after) as f64 * GRID_S
    );

    let under_1 = runs
        .iter()
        .filter(|&&(a, b)| (b - a) as f64 * GRID_S < 1.0)
        .count();
    let cut = runs
        .iter()
        .filter(|&&(a, b)| (1.0..4.0).contains(&((b - a) as f64 * GRID_S)))
        .count();
    let speed = runs
        .iter()
        .filter(|&&(a, b)| (b - a) as f64 * GRID_S >= 4.0)
        .count();

    println!("  < 1.0 s   {under_1}  (ignored)");
    println!("  1-4 s     {cut}  (collapse)");
    println!("  >= 4.0 s  {speed}  (speed up)");

    for &(a, b) in &runs {
        let len_s = (b - a) as f64 * GRID_S;
        match decide(len_s) {
            Action::Keep => {}
            act => println!(
                "  {} {:.1}s -> {:?}",
                fmt_time(a as f64 * GRID_S),
                len_s,
                act
            ),
        }
    }

    let segs = build_segments(&runs, grid_len, duration);
    let segs = merge_adjacent(segs);

    let out_len: f64 = segs
        .iter()
        .map(|s| (s.src_end - s.src_start) / s.speed)
        .sum();

    println!("segments    {}", segs.len());
    println!(
        "output      {:.1} s ({:.1}% removed)",
        out_len,
        100.0 * (1.0 - out_len / duration)
    );

    let mut out_cursor = 0.0;
    let mut plan_segs = vec![];

    for s in &segs {
        let out_len = (s.src_end - s.src_start) / s.speed;
        plan_segs.push(PlanSegment {
            src_start: s.src_start,
            src_end: s.src_end,
            speed: s.speed,
            out_start: out_cursor,
            out_end: out_cursor + out_len,
        });
        out_cursor += out_len;
    }

    let plan = Plan {
        src_duration_s: duration,
        out_duration_s: out_cursor,
        grid_len,
        crop: crop.clone(),
        segments: plan_segs,
    };

    let path = temp_dir.join("plan.json");
    fs::write(&path, serde_json::to_string_pretty(&plan)?)
        .with_context(|| format!("could not write {}", path.display()))?;

    Ok(())
}
