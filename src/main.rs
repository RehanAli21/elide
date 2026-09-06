use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::Parser;

use elide::align::{ensure_model, transcribe_chunked};
use elide::audio::{extract_audio, measure_loudness, Analysis};
use elide::captions::{write_srt, Word};
use elide::cli::Cli;
use elide::constants::{
    ACTIVITY_MIN, BRIDGE_GAP_S, DEAD_BRIDGE_S, EDGE_SMOOTH_S, ENERGY_S, GATE_DB, GRID_H, GRID_S, GRID_W, HIGHPASS_HZ,
    MASTER_TP, MAX_BUSY, MIN_BLOB, MIN_SPEECH_S, PAD_AFTER_S, PAD_BEFORE_S,
    QUIET_DB, SAMPLES_PER_SLICE, SAMPLE_RATE, TARGET_LUFS, VAD_CHUNK, VAD_ENTER, VAD_EXIT,
};
use elide::crop::{blobs, bounding_box, busy_fraction, cell_activity, content_mask, sample_frames};
use elide::disfluency::{apply_cuts, find_cuts};
use elide::features::{boxcar, energy_db, smooth};
use elide::signal;
use elide::master::{finalize, master};
use elide::ai::provider::{Capabilities, LlmProvider, Ollama};
use elide::ai::{capabilities, monitor, tasks};
use elide::plan::{
    bridge_dead, build_segments, dead_runs, decide, merge_adjacent, speedup_starts, trim_edges,
    Action, DeadAir, Plan, PlanSegment,
};
use elide::policy::Policy;
use elide::probe::{ffprobe_json, parse_fps, Probe};
use elide::render::{concat_segments, render_segments};
use elide::utilities::fmt_time;
use elide::vad::{bridge, drop_bursts, hysteresis, longest_silences, pad, speech_mask};
use elide::verify::verify;
use hound::WavReader;
use voice_activity_detector::{IteratorExt, VoiceActivityDetector};


/// M4b — pick the VAD threshold per video, no labels. Sweep a fixed set of
/// thresholds, count sped-up sections for each (the metric is the *plan*, not
/// the mask), and take the plateau: the flat minimum of the U-shaped count.
/// Returns (chosen threshold, status label, the sweep table).
///
/// The VAD probabilities do not depend on the threshold, so only the speech
/// mask and plan are rebuilt per threshold — the sweep is cheap.
fn calibrate_threshold(
    probs: &[f32],
    frozen: &[bool],
    energy_sm: &[f64],
    grid_len: usize,
) -> (f32, &'static str, Vec<(f32, f64, Vec<i64>)>) {
    let candidates = [0.50f32, 0.40, 0.30, 0.25, 0.20, 0.15];

    let table: Vec<(f32, f64, Vec<i64>)> = candidates
        .iter()
        .map(|&t| {
            let exit = (t - 0.15).max(0.01); // Silero's threshold - 0.15, floored
            let mask = speech_mask(probs, t, exit, grid_len);
            let speech_pct = 100.0 * mask.iter().filter(|&&b| b).count() as f64 / grid_len as f64;
            (t, speech_pct, speedup_starts(&mask, frozen, energy_sm))
        })
        .collect();

    let counts: Vec<usize> = table.iter().map(|(_, _, s)| s.len()).collect();
    let min_count = *counts.iter().min().unwrap();
    let max_count = *counts.iter().max().unwrap();
    let spread = max_count - min_count;

    // candidates run high -> low, so the first index at the minimum is the
    // highest (most conservative) threshold on the plateau.
    let min_idx = counts.iter().position(|&c| c == min_count).unwrap();
    let at_edge = min_idx == 0 || min_idx == candidates.len() - 1;

    // A tuner that cannot tell it is blind is worse than a constant.
    if spread >= 2 {
        (candidates[min_idx], "calibrated", table)
    } else if spread == 1 && !at_edge {
        (candidates[min_idx], "weak (provisional)", table)
    } else {
        (VAD_ENTER, "no signal — default", table)
    }
}

/// Find the content region — the part of the frame that actually changes.
/// Returns None when there isn't one, which is what a talking-head recording
/// looks like: no screen, so no crop and nothing to freeze-detect.
fn detect_crop(input: &str, duration: f64, width: u32, height: u32) -> Result<Option<String>> {
    let frames = sample_frames(input, duration)?;
    println!("frames      {}", frames.len());

    let activity = cell_activity(&frames);
    let max = activity.iter().fold(0.0f32, |m, &a| m.max(a));
    let mean = activity.iter().sum::<f32>() / activity.len() as f32;
    println!("activity    max {max:.2}  mean {mean:.2}");

    let (mask, threshold) = match content_mask(&activity) {
        Some(v) => v,
        None => {
            println!("no content region (max activity below {ACTIVITY_MIN})");
            return Ok(None);
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
            return Ok(None);
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

    Ok(Some(format!("{cw}:{ch}:{x}:{y}")))
}

#[deny(unused_must_use)]
fn main() -> Result<()> {
    let args = Cli::parse();

    // ---- policy, resolved ONCE, up front ------------------------------------
    // THE PROMPT SETS POLICY. THE PROMPT NEVER TOUCHES A GUARD. After this
    // block the pipeline is deterministic given `policy` — no model influence
    // is sprinkled through the fourteen steps that follow, and every field has
    // already been clamped into its documented range.
    let ai: Option<(Ollama, Capabilities)> = if args.no_ai {
        println!("ai          disabled (--no-ai)");
        None
    } else {
        let p = Ollama::new("qwen2.5:7b");
        let c = capabilities::probe(&p);
        println!("ai          {}", capabilities::describe(&c));
        Some((p, c))
    };

    let policy = match &args.params {
        Some(path) => {
            let p = Policy::from_file(path)
                .with_context(|| format!("could not read parameter set {path}"))?;
            println!("policy      replayed from {path}");
            p
        }
        None => Policy::resolve(
            &args.prompt,
            ai.as_ref().map(|(p, c)| (p as &dyn LlmProvider, c)),
        ),
    };
    println!("policy      {}", policy.summary());

    // Log the resolved struct next to the video, so the run is reproducible and
    // "why did it do that?" has an answer that is not a guess.
    fs::create_dir_all(&args.output)
        .with_context(|| format!("could not create {}", args.output))?;
    let policy_path = PathBuf::from(&args.output).join("policy.json");
    fs::write(&policy_path, serde_json::to_string_pretty(&policy)?)
        .with_context(|| format!("could not write {}", policy_path.display()))?;
    println!("            logged to {} (replay: --params)", policy_path.display());

    let pacing = policy.pacing();

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

    let (input_i, input_tp) = measure_loudness(&args.input, TARGET_LUFS, &[])?;

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

    // transcript for captions (and, later, disfluency). A supplied --transcript
    // overrides; otherwise we transcribe with chunked whisper.
    let words: Vec<Word> = if let Some(tpath) = &args.transcript {
        let raw = fs::read_to_string(tpath)
            .with_context(|| format!("could not read transcript {tpath}"))?;
        serde_json::from_str(&raw).context("could not parse transcript JSON")?
    } else {
        let model = ensure_model()?;
        println!("transcribing (chunked whisper)...");
        let t0 = Instant::now();
        let w = transcribe_chunked(&samples, spec.sample_rate, &model)?;
        println!(
            "transcript  {} words in {:.1}s",
            w.len(),
            t0.elapsed().as_secs_f64()
        );
        w
    };

    // keep the transcript so a re-run can skip whisper via --transcript
    let tpath = temp_dir.join("transcript.json");
    fs::write(&tpath, serde_json::to_string_pretty(&words)?)
        .with_context(|| format!("could not write {}", tpath.display()))?;

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

    // expect_screen is policy: a video of a person talking has no screen to
    // crop to and nothing to freeze-detect, so we do not spend the frame
    // sampling looking for one.
    let detected = if policy.expect_screen {
        detect_crop(&args.input, duration, width, height)?
    } else {
        println!("crop        skipped (prompt says no screen)");
        None
    };

    // No content region is not a failure — it is what a talking head looks
    // like. Fall back to the whole frame and let silence decide alone.
    let crop = match &detected {
        Some(c) => {
            println!("crop        {c}");
            c.clone()
        }
        None => format!("{width}:{height}:0:0"),
    };

    // M12 — signal B behind a trait. The only per-genre piece; everything
    // downstream is unchanged whichever implementation runs. --signal overrides
    // the prompt; with neither, expect_screen picks it.
    let signal_name = args.signal.clone().unwrap_or_else(|| {
        if detected.is_some() {
            "freeze".to_string()
        } else {
            "none".to_string()
        }
    });
    let signal = signal::for_name(&signal_name)?;
    let frozen = signal.mask(&args.input, &crop, grid_len, duration)?;

    let frozen_pct = 100.0 * frozen.iter().filter(|&&b| b).count() as f64 / grid_len as f64;
    println!("frozen      {frozen_pct:.1}% of grid  (signal: {})", signal.name());
    let dead = padded
        .iter()
        .zip(&frozen)
        .filter(|&(&s, &f)| !s && f)
        .count();

    let dead_pct = 100.0 * dead as f64 / grid_len as f64;
    let removable_s = dead as f64 * GRID_S;
    println!("dead        {dead_pct:.1}%");

    // M8 pre-flight verdict — advisory only, changes no cut. This is the raw
    // dead time (the ceiling); the real edit removes less, because collapses
    // keep 0.5 s and speed-ups keep compressed time. Thresholds from
    // PROJECT_HANDOFF §15: >=12% worth running, <5% little to cut.
    let verdict = if dead_pct >= 12.0 {
        "worth editing"
    } else if dead_pct < 5.0 {
        "little to cut — probably not worth it"
    } else {
        "marginal — some dead air, but not much"
    };
    println!("verdict     {verdict}  ({removable_s:.0}s removable, {dead_pct:.1}%)");

    let energy = energy_db(&samples, spec.sample_rate);
    println!(
        "energy      {} frames (grid_len x2 = {})",
        energy.len(),
        grid_len * 2
    );

    // Two smoothings, one per job. 30 ms for the disfluency splice test and the
    // quiet-run guard; 100 ms for the edge guard, so a mouse click cannot trip it.
    let energy_sm = smooth(&energy, 0.03);
    let energy_edge = boxcar(&energy, (EDGE_SMOOTH_S / ENERGY_S) as usize);
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

    // M4b — calibrate the VAD threshold from the plan, then rebuild the speech
    // mask if the choice differs from the default it was first built at.
    let (chosen, status, table) = calibrate_threshold(&probs, &frozen, &energy_edge, grid_len);
    println!("\nthreshold sweep:");
    for (t, pct, starts) in &table {
        println!(
            "  {t:.2}  speech {pct:.1}%  {} sped-up  {starts:?}",
            starts.len()
        );
    }
    println!("chosen      {chosen:.2}  ({status})");

    let padded = if (chosen - VAD_ENTER).abs() > 1e-6 {
        let exit = (chosen - 0.15).max(0.01);
        let padded = speech_mask(&probs, chosen, exit, grid_len);
        let pct = 100.0 * padded.iter().filter(|&&b| b).count() as f64 / grid_len as f64;
        println!("recalibrated speech {pct:.1}% (default was {VAD_ENTER:.2})");
        padded
    } else {
        padded
    };

    // M12b — monitor the speech step. The model proposes ONE parameter and a
    // direction; the code re-runs and the SCORE decides which mask survives.
    // Only the VAD threshold is adjustable: SNAP and QUIET are guards, and
    // nothing outside constants.rs may widen a guard — so a proposal naming
    // anything else is discarded here, before it can do any work.
    // How much discriminating power does the score have on THIS file? Measured,
    // not assumed: the same sweep M4b uses, scored. If the whole range is
    // negligible the score cannot tell two masks apart, and accepting a
    // proposal on that is the "tuner that cannot detect its own blindness"
    // failure — worse than leaving the constant alone.
    let sweep_scores: Vec<f64> = [0.50f32, 0.40, 0.30, 0.25, 0.20, 0.15]
        .iter()
        .map(|&t| {
            let m = speech_mask(&probs, t, (t - 0.15).max(0.01), grid_len);
            monitor::speech_score(&m, &words, grid_len)
        })
        .collect();
    let lo = sweep_scores.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = sweep_scores.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    println!("score range {lo:.3} .. {hi:.3}  (span {:.3})", hi - lo);

    let score = monitor::speech_score(&padded, &words, grid_len);
    let longest = longest_silences(&padded, 1)
        .first()
        .map(|&(a, b)| (b - a) as f64 * GRID_S)
        .unwrap_or(0.0);
    let speech_pct = 100.0 * padded.iter().filter(|&&b| b).count() as f64 / grid_len as f64;

    let verdict = monitor::review(
        "speech detection",
        &format!("speech {speech_pct:.1}%\nscore {score:.3}\nlongest silence {longest:.2}s"),
        &format!("{{\"threshold\": {chosen:.2}}}"),
        "Lower = more sensitive. Increase if silence is called speech.",
    );

    // NO-SIGNAL DETECTOR. The score is coverage minus false-alarm, so a value
    // at or below zero means the mask marks silence as often as speech — no
    // better than chance, and no basis for preferring one mask over another.
    // Measured: demo 0.256 (informative), extension -0.003 (degenerate, where
    // an unguarded loop accepted a 0.30 -> 0.48 change on 0.003 of noise).
    // Needs no invented threshold; zero is the definition.
    let informative = score > 0.0;
    if !informative {
        println!("monitor     no signal (score {score:.3} <= 0) — proposals ignored");
    }

    let padded = if informative
        && !verdict.ok
        && verdict.parameter == "threshold"
        && verdict.direction != "none"
    {
        let t2 = if verdict.direction == "increase" {
            chosen * 1.6
        } else {
            chosen / 1.6
        };
        let alt = speech_mask(&probs, t2, (t2 - 0.15).max(0.01), grid_len);
        let alt_score = monitor::speech_score(&alt, &words, grid_len);
        println!(
            "monitor     proposed threshold {} -> {t2:.3}, score {score:.3} vs {alt_score:.3} — {}",
            verdict.direction,
            if alt_score > score { "KEPT" } else { "discarded" }
        );
        if alt_score > score { alt } else { padded }
    } else {
        if informative {
            println!("monitor     ok ({})", verdict.reason);
        }
        padded
    };

    let runs = dead_runs(&padded, &frozen);
    println!("dead runs   {} raw", runs.len());

    let runs = bridge_dead(&runs, &padded, DEAD_BRIDGE_S);
    println!("            {} after bridging", runs.len());

    // The planner's floor is POLICY — a speaker whose pauses are rhetorical
    // wants a higher one. The clamp inside trim_edges stays on MIN_DEAD_S:
    // that one is a safety invariant (never delete a run outright), not pacing.
    let min_dead_slices = (policy.pause_floor_s / GRID_S) as usize;
    let runs: Vec<_> = runs
        .into_iter()
        .filter(|&(a, b)| b - a >= min_dead_slices)
        .collect();
    println!(
        "            {} after pause floor ({:.2}s)",
        runs.len(),
        policy.pause_floor_s
    );

    let before: usize = runs.iter().map(|&(a, b)| b - a).sum();
    let runs = trim_edges(&runs, &energy_edge, GATE_DB);
    let after: usize = runs.iter().map(|&(a, b)| b - a).sum();
    println!(
        "            {} after edge guard, {:.1}s trimmed",
        runs.len(),
        (before - after) as f64 * GRID_S
    );

    let floor = policy.pause_floor_s;
    let split = if policy.dead_air == DeadAir::Speed {
        4.0
    } else {
        f64::INFINITY
    };
    let under = runs
        .iter()
        .filter(|&&(a, b)| (b - a) as f64 * GRID_S < floor)
        .count();
    let cut = runs
        .iter()
        .filter(|&&(a, b)| (floor..split).contains(&((b - a) as f64 * GRID_S)))
        .count();
    let speed = runs
        .iter()
        .filter(|&&(a, b)| (b - a) as f64 * GRID_S >= split)
        .count();

    println!("  < {floor:.2} s  {under}  (ignored)");
    println!("  short      {cut}  (collapse)");
    println!("  long       {speed}  (speed up, max {:.0}x)", policy.max_speed);

    for &(a, b) in &runs {
        let len_s = (b - a) as f64 * GRID_S;
        match decide(len_s, pacing) {
            Action::Keep => {}
            act => println!(
                "  {} {:.1}s -> {:?}",
                fmt_time(a as f64 * GRID_S),
                len_s,
                act
            ),
        }
    }

    let segs = build_segments(&runs, grid_len, duration, pacing);
    let segs = merge_adjacent(segs);

    // M11 — disfluency cuts, subtracted from the 1x segments only. Whether to
    // run at all is policy: on a podcast the natural speech IS the product.
    let (cuts, rejected, clipped) = if policy.remove_disfluencies {
        find_cuts(&words, &samples, spec.sample_rate, &energy_sm, &energy_edge, &segs)
    } else {
        println!("disfluency  off (prompt keeps natural speech)");
        (Vec::new(), Vec::new(), 0)
    };
    if policy.remove_disfluencies {
        let cut_s: f64 = cuts.iter().map(|c| c.end - c.start).sum();
        let fillers = cuts.iter().filter(|c| c.kind == "filler").count();
        let stutters = cuts.iter().filter(|c| c.kind == "stutter").count();
        let repeats = cuts.iter().filter(|c| c.kind == "repeat").count();
        println!(
            "disfluency  {} cuts ({fillers} filler, {stutters} stutter, {repeats} repeat), {cut_s:.1}s removed, {} rejected",
            cuts.len(),
            rejected.len()
        );
        // repeats the rule found but the audio would not allow a cut at
        if clipped > 0 {
            println!("  clipped     {clipped}  (repeat found, no alignment passed)");
        }
        // proposed vs accepted per kind — separates "the detector found few" from
        // "the gates threw many away"
        // seconds per kind, not just counts — the gap has repeatedly turned out to
        // be cut length rather than cut count.
        // Reference (v6): filler ~26 / ~17.4s / avg 0.67s, repeat 9 / ~40.5s / avg 4.5s
        for kind in ["filler", "stutter", "repeat"] {
            let ok = cuts.iter().filter(|c| c.kind == kind).count();
            let no = rejected.iter().filter(|r| r.kind == kind).count();
            let secs: f64 = cuts
                .iter()
                .filter(|c| c.kind == kind)
                .map(|c| c.end - c.start)
                .sum();
            let avg = if ok > 0 { secs / ok as f64 } else { 0.0 };
            println!(
                "  {kind:8}    {ok} accepted of {:2} proposed, {secs:5.1}s  avg {avg:.2}s",
                ok + no
            );
        }
        // per-chain detail for repeats: 2-take chains everywhere means the chain is
        // truncating, which shows up as more repeats each shorter than the reference
        for c in cuts.iter().filter(|c| c.kind == "repeat") {
            println!(
                "    {}  {} takes  span {:5.2}s  {:13}  sim {:.2}  {:?}",
                fmt_time(c.start),
                c.takes,
                c.end - c.start,
                c.align,
                c.sim,
                c.text
            );
        }
        // why candidates died — the first word of each reason, counted
        let mut why_counts: Vec<(String, usize)> = Vec::new();
        for r in &rejected {
            let key = r.why.split_whitespace().next().unwrap_or("?").to_string();
            match why_counts.iter_mut().find(|(k, _)| *k == key) {
                Some((_, n)) => *n += 1,
                None => why_counts.push((key, 1)),
            }
        }
        for (why, n) in &why_counts {
            println!("  rejected {n:3}  {why}");
        }
        for r in rejected.iter().take(8) {
            println!("    {:8.2} {:8} {}", r.start, r.kind, r.why);
        }
    }
    let segs = apply_cuts(segs, &cuts);

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

    let seg_dir = temp_dir.join("segments");
    fs::create_dir_all(&seg_dir)
        .with_context(|| format!("could not create {}", seg_dir.display()))?;

    let mut seg_paths = vec![];
    let t0 = Instant::now();

    for (i, seg) in segs.iter().enumerate() {
        let path = seg_dir.join(format!("seg_{i:03}.mkv"));
        render_segments(&args.input, seg, &path)?;
        seg_paths.push(path);
    }

    println!(
        "rendered    {} segments in {:.1}s",
        seg_paths.len(),
        t0.elapsed().as_secs_f64()
    );

    let list_path = temp_dir.join("concat.txt");
    let mut list = String::new();

    for p in &seg_paths {
        list.push_str(&format!("file '{}'\n", p.display()));
    }

    fs::write(&list_path, list)
        .with_context(|| format!("could not write {}", list_path.display()))?;

    let concat_path = temp_dir.join("concat.mkv");
    concat_segments(&list_path, &concat_path)?;

    // --- M6: master ---
    let highpass = format!("highpass=f={HIGHPASS_HZ}");
    let (master_i, master_tp) = measure_loudness(
        concat_path.to_str().context("non-UTF-8 path")?,
        policy.target_lufs,
        &[&highpass, "afftdn=nr=12:nf=-45"],
    )?;

    let master_gain = policy.target_lufs - master_i;
    let would_clip = master_tp + master_gain > MASTER_TP;

    println!("master_i    {master_i:.2} LUFS");
    println!("master_tp   {master_tp:.2} dBTP");
    println!("gain        {master_gain:+.2} dB");
    println!("compressor  {}", if would_clip { "yes" } else { "no" });

    let master_path = temp_dir.join("master.mkv");
    master(&concat_path, &master_path, would_clip, policy.target_lufs)?;

    let out_path = PathBuf::from(&args.output).join("out.mp4");
    finalize(&master_path, &out_path)?;
    println!("wrote       {}", out_path.display());

    // M9 — captions, scaled to the real output duration (fixes the drift)
    let out_json = ffprobe_json(out_path.to_str().context("non-UTF-8 path")?)?;
    let out_probe: Probe =
        serde_json::from_str(&out_json).context("could not parse ffprobe output for out.mp4")?;
    let measured: f64 = out_probe
        .format
        .duration
        .parse()
        .context("bad out.mp4 duration")?;
    let scale = measured / plan.out_duration_s;

    let srt_path = PathBuf::from(&args.output).join("captions.srt");
    if policy.captions {
        let n = write_srt(&words, &plan, scale, &srt_path)?;
        println!(
            "captions    {n} cues -> {} (scale {scale:.5})",
            srt_path.display()
        );
    } else {
        println!("captions    off (prompt)");
    }

    let checks = verify(&args.input, &out_path, &temp_dir, &plan, policy.target_lufs)?;

    println!("\nverification:");
    for c in &checks {
        println!(
            "  {:<16} {}  {}",
            c.name,
            if c.passed { "PASS" } else { "FAIL" },
            c.detail
        );
    }

    // M13 — the safest AI task: flag caption lines a human should proofread.
    // It edits nothing, so a wrong answer costs nothing. Triage runs first, so
    // only the risky-looking lines cost a call.
    let cap_lines: Vec<String> = fs::read_to_string(&srt_path)
        .unwrap_or_default()
        .split("\n\n")
        .filter_map(|cue| cue.lines().nth(2).map(str::to_string))
        .collect();

    // Reuses the provider probed at the top of the run — one probe, not two.
    if let (false, Some((provider, caps))) = (cap_lines.is_empty(), &ai) {
        let flagged = tasks::proofread_list(provider, caps, &cap_lines);
        if flagged.is_empty() {
            println!("proofread   nothing flagged");
        } else {
            println!("proofread   {} lines worth a human look:", flagged.len());
            for l in flagged.iter().take(10) {
                println!("  {l}");
            }
        }
    }

    if checks.iter().any(|c| !c.passed) {
        bail!("export failed verification");
    }

    Ok(())
}
