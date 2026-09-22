# elide — handoff, M0 through M14

Started as the M7 handoff; it now covers the whole build. Everything is
verified against two real videos — all five verification checks pass on both.

In order, across sessions: the word-clip check was removed; mastering went
two-pass and then back to **single-pass + limiter**; the duration check was
added; M8 landed as a one-line pre-flight verdict; M4-M7 was split out of
`main.rs` into modules; M4b threshold calibration was built (reads "no signal"
on both files); M5b parallel render was built and then **removed** (no material
speedup — each ffmpeg already saturates the cores); chunked Whisper word
alignment was built; then **M9 captions**, **M11 disfluency removal**, **M12**
(the `DeadTimeSignal` trait), **M12b** (the monitor loop), **M13** (the AI
layer), and **M14** (prompt → policy). M10 was deliberately skipped until there
is a GUI.

Most recently: `PAD_BEFORE_S` was raised 0.50 → 1.50 to stop a word being
clipped, `pause_floor_s` was removed from policy back into `constants.rs`, an
investigation into the repeats Whisper hides was measured and closed as
not-reachable (issue 11), and every `?` was replaced with an explicit `match`
while an audit fixed seven places that swallowed errors silently (issue 12).

The pipeline runs end to end with no outside files: it transcribes itself,
cuts dead air *and* disfluencies, masters, writes captions, and verifies.

This document records what was built, what was *decided and why*, and what the
measured numbers were. The reasoning matters more than the code: several
decisions here were reversed once after measurement, and the reversals are the
useful part.

---

## The tool

Three inputs, per `CLI_AND_PROMPT.md`:

```
elide --input demo.mp4 --output out/ --prompt "demo of my app, for YouTube"
```

The prompt is live since M14 — it is classified into a genre and mapped to a
bounded `Policy` struct, which is logged to `out/policy.json`. Flags:

```
--signal freeze|slides|none    override the signal the prompt implied
--params out/policy.json       replay an exact past run
--no-ai                        skip the model entirely; defaults are used
--transcript words.json        skip whisper (fast re-runs)
```

---

## Where this stands

**BUILD_STEPS is complete.** M0-M9 and M11-M14 are built and verified; M10 was
deliberately skipped until there is a GUI. Both test videos pass all five
verification checks. The pipeline needs no outside files: it transcribes
itself, cuts dead air *and* disfluencies, masters, writes captions, verifies,
and refuses to export if a check fails.

**Next: a UI.** That also unblocks M10 (the revision loop), which was skipped
precisely because there was no interface for a human to revise anything. The
pieces M10 needs already exist — `plan.json` is the edit decision list,
`policy.json` is the parameter set, and `--params` replays it — so the loop is
"show the plan, let the user change it, re-render", not new pipeline work.

Known limits going in, all documented under Open issues:

- ~26.5 s more output than the Python reference on the demo file (issue 2)
- repeats that whisper collapses cannot currently be found at all (issue 11)
- `chapters` is a policy field with nothing behind it (issue 5)
- `--crop` override unbuilt (issue 4); `temp/` never cleaned, ~1 GB/run (issue 6)

---

## Layout

```
src/
  main.rs        orchestration only (imports + main); ~500 lines
  lib.rs         module declarations
  constants.rs   every threshold, documented, with the evidence behind it
  cli.rs         the three inputs
  probe.rs       ffprobe, Probe/Format/Stream structs, parse_fps
  audio.rs       measure_loudness, measure_loudnorm (two-pass stats),
                 extract_audio, Loudnorm, LoudnormStats, Analysis
  features.rs    energy_db, smooth
  align.rs       ensure_model (downloads to models/), transcribe_chunked
  captions.rs    Word, write_srt (SRT re-timed through the plan)
  disfluency.rs  find_cuts (filler/stutter/repeat/false-start + gates),
                 apply_cuts
  dsp.rs         radix-2 FFT, band spectra, similarity (no FFT crate)
  vad.rs         hysteresis, bridge, drop_bursts, pad, longest_silences
  crop.rs        sample_frames, cell_activity, content_mask, blobs,
                 bounding_box, busy_fraction
  freeze.rs      detect_freezes, paint_freezes
  signal.rs      DeadTimeSignal trait: Freeze, Slides, None_
  plan.rs        dead_runs, bridge_dead, decide, build_segments,
                 merge_adjacent, trim_edges, Segment/PlanSegment/Plan,
                 map_to_source (the time map), DeadAir + Pacing (policy the
                 planner is allowed to see)
  policy.rs      Genre, Policy, the genre->policy table, apply_explicit
  render.rs      atempo_chain, render_segments, concat_segments
  master.rs      master (loudnorm + limiter), finalize
  monitor.rs     propose/re-run/score loop, speech_score
  verify.rs      Check + the five checks (duration, faststart, loudness,
                 splice clicks, a/v sync)
  utilities.rs   fmt_time
```

`main.rs` and `lib.rs` are **separate crates**. `main.rs` uses `elide::module::item`,
never `mod`. Modules inside the library use `crate::`.

M4-M7 code was split out into `plan.rs` / `features.rs` / `render.rs` /
`master.rs` / `verify.rs` (follow-up session). The split was verified
behaviour-preserving: both test files produce identical numbers and all five
checks pass, before and after. `plan.rs` is the centre — it owns the `Plan`
struct and the `map_to_source` time map that verify (and M9 captions) read.

---

## Rule A — the time grid

The one thing that is expensive to change later. Settled before M2 and
unchanged since.

- **20 ms grid.** Every mask in the program is a `Vec<bool>` of identical
  length, indexed identically.
- **`grid_len = samples / 320`**, integer division. The trailing partial slice
  is dropped (9 ms on the demo file; too short to contain a decision).
- **Energy is computed at 10 ms**, finer than the grid, because the quiet-run
  guard measures a 0.10 s stretch and 20 ms frames give only 5 samples to judge
  it. Grid slice `i` covers energy frames `2i` and `2i+1`.
- **Span → index truncates, then clamps** to `grid_len - 1`.
- **`grid_len` and `src_duration_s` are separate values and must never be
  substituted for each other.** The grid ends ~40 ms before the video does.
  `grid_len` comes from the audio samples; the last segment's `src_end` comes
  from ffprobe. Naming them distinctly in code is deliberate.

---

## What each milestone does

### M0 — probe
`ffprobe -v error -print_format json -show_format -show_streams`. Parses
duration, resolution, fps (from `r_frame_rate`, a fraction string), audio
sample rate.

Two distinct failure paths that must stay distinct: `.output()` fails only when
the binary can't be *spawned* (ffmpeg not installed); a process that ran and
failed returns `Ok` with `status.success() == false` (file missing, not a
video).

### M1 — audio into memory, normalised

Corrected during the session — `BUILD_STEPS.md` was missing the normalisation
step. Actual sequence:

1. `measure_loudness(input, TARGET_LUFS, &[])` → `input_i`, `input_tp`
2. `gain = TARGET_LUFS - input_i`
3. extract with `-af "volume={gain}dB" -vn -ac 1 -ar 16000 -c:a pcm_f32le`
4. read with hound as `f32`, **no division by 32768**
5. write `analysis.json`

**Why `pcm_f32le` and not `pcm_s16le`:** measured peak after gain on the demo
file was **1.145**, with 23 samples above 1.0. In s16 those would have clipped
permanently. The overshoot comes from resampler ringing — 48 kHz → 16 kHz
anti-aliasing produces sample values above the source's true peak.

The alternative (clamp the gain to preserve headroom) was rejected: it would
make the file sit at something other than −23 LUFS, so `QUIET` and `GATE` would
mean different things on different files, which is exactly what normalising was
for.

**Sanity print:** max absolute sample should be roughly 0.1–1.0. If it reads
~0.00003, the `/32768.0` is still in there.

`loudnorm` prints its JSON block to **stderr**, not stdout, mixed with the
banner. Bracket it with `find('{')` and `rfind('}')`.

### M2 — speech mask

```
samples → probs (512-sample chunks) → hysteresis → paint onto 20 ms grid
        → bridge → drop bursts → pad
```

Uses the `voice_activity_detector` crate (Silero v5 via `ort`).

**Silero does not return speech spans.** `BUILD_STEPS.md` says it does; that's
`get_speech_timestamps()`, a Python helper. The ONNX graph returns one
probability per chunk. Window size is fixed at 512 samples @ 16 kHz — not
configurable.

**Hysteresis was added, and it earns its place.** Neither `predict()` nor
`label()` in the crate does it. Enter 0.30, exit 0.15 (Silero's own
`threshold - 0.15`). Measured: 2416 transitions without → 1638 with. 6.2% of
chunks sat in the dead band.

If `VAD_ENTER` is ever swept downward, floor `VAD_EXIT` at 0.01 — at enter 0.15
a derived exit of 0.00 means speech, once started, never ends.

**Chunk-to-slice painting:** 512-sample chunks vs 320-sample slices realign
every 160 ms. Slice `i` takes the answer from the chunk containing its midpoint
sample: `(i * 320 + 160) / 512`. Truncating, consistent with everything else.

Post-processing order is fixed: **bridge, then drop, then pad.** Bridging first
means a bridged pair counts as one long run rather than two short ones that get
deleted. Padding last, because padding first would create overlaps the other
passes would mangle.

Each pass **clones its input and writes to the clone.** Reading and writing the
same array lets a flip change what the next iteration sees.

Run lengths are compared **in seconds**, not converted to slice counts —
`run_len as f64 * GRID_S < 0.35`. Removes a 17-vs-18 decision that would
otherwise be made independently in three places.

### M3 — content region + freeze mask

Auto-crop was moved *into* M3 rather than deferred past M8, because the two
test videos need incompatible crops (a phone emulator and a full browser), so
manual `--crop` doesn't survive even a two-file test set. `--crop` remains a
documented override but is **not implemented yet**.

```
1. sample 200 frames (fps = 200/duration), 160x90 greyscale
2. per-cell mean absolute frame-to-frame difference
3. if max activity < ACTIVITY_MIN -> no content region, expect_screen=false
   else threshold = ACTIVITY_RATIO * max
4. 4-neighbour connected components, sorted by size
5. drop blobs < MIN_BLOB cells
6. reject blobs busy in > MAX_BUSY of frames  (webcam)
7. largest survivor -> bounding box -> scale to full res -> snap even
8. freezedetect with that crop -> parse -> paint onto the grid
```

**The mask threshold must be relative.** An earlier absolute floor of
`max(0.08*max, 0.5)` silently clipped the mask on densely-sampled files and
shattered the extension video's content region into 125 fragments with a top
blob of 373 cells instead of ~2900. Frame-to-frame activity scales with the gap
between sampled frames, and a fixed 200-frame count makes that gap
duration-dependent. Removing the floor and moving the no-signal test to
`max < 2.0` fixed it.

That is the **third** instance of the same bug class in this project:
absolute dB thresholds across four videos spanning 12.2 dB; the VAD threshold
tuned on un-normalised audio; and this. Any new absolute constant deserves
suspicion.

**Full-frame freezedetect under-fires, it doesn't over-fire.** Measured on the
demo video: with crop 80 blocks / 1042.7 s frozen; full frame 150 blocks /
737.3 s. More blocks, *less* frozen time — the webcam bubble and taskbar clock
chop long frozen stretches into sub-threshold pieces. Without the crop you lose
333 s of real dead air on a video where the finished edit removes ~324 s total.

**Face detection is specified but deferred.** Step 4 of the ranking (face-check
blobs already flagged as webcam-suspect, reject only those with a face) is
documented and unbuilt. Reason: the motion rule alone separates cleanly on both
test files (app 33.7% busy, webcam 100%), so the face check would only confirm
a decision already made correctly. The case it exists for — an app window that
is itself constantly changing (gameplay, video playback, scrolling terminal)
alongside a webcam overlay — is not in the test set.

Under `face_bubble=no`, "all blobs flagged" is indistinguishable from "no screen
content": both produce no survivor and route to audio-only dead air. Printing
the blob table on every run is what makes the two cases distinguishable in
hindsight.

If built: use `rustface` (SeetaFace, pure Rust, no second ML runtime).
Never run it on the full frame — run it per candidate blob crop. Measured cost:
2.60 s full frame vs 0.85 s on a 264x312 crop.

### M4 — the plan

```
dead_runs(speech, frozen)          (!speech AND frozen)
  -> bridge_dead(2.0 s)            merge, then re-apply "AND not speech"
  -> filter to >= MIN_DEAD_S
  -> trim_edges(smoothed energy)   the edge guard
  -> decide()                      Keep / Collapse / Speed
  -> build_segments()
  -> merge_adjacent()
  -> plan.json
```

**Order matters and was got wrong once.** The `min_dead` filter must come
*after* bridging, not before — filtering first removes short runs that would
have merged into qualifying ones.

**`bridge_dead` step 2 is not a formality.** Merging joins two dead runs across
whatever sits between them, which may be speech. Re-splitting on `!speech`
cuts it back out. Note it deliberately does *not* restore the `frozen`
condition — that's how "quiet, but the mouse twitched once" becomes one dead
block rather than three.

**The edge guard (`trim_edges`)** matches the Python reference exactly, after
three rounds of correction:

- works on **10 ms energy frames**, indexing the array directly. An earlier
  version worked on 20 ms slices via a helper that took the *max* of the two
  covering frames — an OR that made "loud" fire roughly twice as often and
  over-trimmed by 70 s.
- reads **smoothed** energy (3-frame boxcar, 30 ms), not raw.
- leading edge takes the **last** loud frame in the first `MAX_SHRINK_S`;
  trailing edge takes the **first** loud frame in the last `MAX_SHRINK_S`.
  Not "walk inward until quiet" — a word can dip mid-syllable.
- trailing window uses the **already-updated** start. Order matters.
- clamps are unconditional: `start <= b - min_dead`, `end >= start + min_dead`.
  So the guard **never deletes a run**. Runs piling up at exactly 1.0 s is the
  design, not a symptom.
- input must be pre-filtered to `>= MIN_DEAD_S` or `b - min_dead` underflows.

**`decide()`:**

```
< 1.0 s   -> Keep (untouched)
1-4 s     -> Collapse to 0.50 s (material discarded)
>= 4.0 s  -> target = clamp(d/12, 1.2, 6.0); speed = min(20, d/target)
```

The `/12` and the clamp interact: **any run between 14.4 s and 72 s gets
exactly 12x**, because the 12 divides out. Below 14.4 s the 1.2 clamp binds;
above 72 s the 6.0 clamp binds. The `min(20, ...)` cap only fires above 120 s
and never fired on the test files.

**`merge_adjacent`** joins consecutive segments with the same speed that are
contiguous in source time. Without it, every collapse splits the timeline into
an artificial pair (45 segments instead of 28), and each boundary is a splice
that carries no edit.

**`plan.json` stores both `src_*` and `out_*`** rather than deriving one from
the other. M7's sync check maps *backwards* from output time to source time,
and doing that from an accumulate-as-you-go list is where off-by-ones live.

### M5 — render

```
per segment  -> temp/segments/seg_NNN.mkv   (28 files)
concat       -> temp/concat.mkv             (-c copy)
```

Per-segment: `-ss {start} -to {end}` **before** `-i` (fast seek), then
`-c:v libx264 -preset fast -crf 19 -profile:v high -pix_fmt yuv420p -g 120`
and `-c:a pcm_s16le -ar 48000 -ac 2`.

Sped segments add `-vf "setpts=PTS/{speed}"` and
`-af "{atempo_chain},volume=0"`.

**The atempo chain is for duration, not for sound.** ffmpeg's `atempo` accepts
0.5–2.0 only, so 12x is `2,2,2,1.5`. Muting alone would leave a 52 s audio
stream under a 4.3 s video segment and every subsequent segment would drift.
`volume=0` is what stops it being chipmunk noise.

The render is **the dominant cost** of the pipeline: ~340-380 s for 28 segments
on the demo file, against ~9 s for VAD.

Segments are `.mkv` because `.mp4` can't hold PCM. PCM is chosen so the concat
has no codec delay or priming samples to re-align at 28 seams.

### M6 — master

```
measure concat.mkv through [highpass, afftdn]  -> master_i, master_tp
gain = MASTER_LUFS - master_i
if master_tp + gain > MASTER_TP: insert compressor
apply: two-pass loudnorm (see below)                     -> temp/master.mkv
finalize: -c:v copy -c:a aac -b:a 192k -movflags +faststart  -> out.mp4
```

**Loudnorm is two-pass** (changed in the follow-up session). Single-pass
loudnorm is a live estimate and is only accurate to ~0.2 LUFS, so the same
chain landed −13.91 on the demo and −14.16 on the extension — the extension
failed the ±0.1 loudness check. Two-pass fixes it: `master()` first measures
the audio *through the full pre-loudnorm chain* (highpass → afftdn →
[compressor]) with `print_format=json`, then applies loudnorm with those
measured values fed back (`measured_I/TP/LRA/thresh`, `offset`, `linear=true`).
Result: demo −14.00, extension −14.06 — both pass, both more accurate.

**The measurement must include the compressor** when it is on, because in the
apply pass loudnorm sits *after* the compressor. This is a different pass from
the compressor-decision measurement above (which is `[highpass, afftdn]` only,
because the compressor is what that measurement is deciding). Two measurement
passes now, for two different questions.

**Mastering measures the concatenated output, never the source.** 245 s were
cut out of the middle; the loudest moment may have been in removed material,
and the silent sped segments shift programme loudness. `input_tp` in
`analysis.json` is a property of the source and is *not* used by M6.

**The measurement pass must carry the same filter prefix as the apply pass**,
minus the compressor (which is what's being decided). Measured on the extension
file: the highpass alone raises true peak by 0.13 dB through filter ringing —
the unsafe direction for a clipping conditional. This is a small correction to
both the Python reference and `FLOW_SPEC.md`, which measure with highpass only
and apply with highpass+afftdn.

**`highpass=f=75`, not 80.** `FLOW_SPEC.md` §13 says 80; the code that produced
the v6 render uses 75.

**`makeup=8` in the compressor is load-bearing.** Bare `acompressor` at 2:1
with no makeup would leave `loudnorm` to close an 8+ dB gap with its true-peak
limiter, which is a brickwall and pumps. With makeup the compressor supplies
most of the gain and loudnorm trims the remainder.

Provenance caveat: the compressor string lives in `BUILD_STEPS.md`,
`EDITING_PLAYBOOK.md` and `PROJECT_HANDOFF.md`, not in `render.py`. The Python
reference stops after the measurement pass; the apply was run by hand. If the
mastered output ever measures wrong, the string is as likely a suspect as the
code.

**Specify `-ar 48000 -ac 2` on the master pass.** Without it, ffmpeg negotiated
192 kHz and the PCM stream came out at 6144 kb/s instead of 1536.

### M7 — verification

Five checks, any of which fails the export (non-zero exit via `bail!`).

| check | how |
| --- | --- |
| duration | ffprobe `out.mp4` duration vs `plan.out_duration_s`, ≤ 0.5 s |
| faststart | byte-search the first 64 KB for `moov` before `mdat` |
| loudness | `measure_loudness` on `out.mp4`, compare to targets |
| splice clicks | sample-to-sample jump at each `out_start`, vs the file's own p99.9 |
| a/v sync | `map_to_source` at 7 checkpoints, grab both frames, ffmpeg `ssim` |

**The duration check** guards the frame-rounding drift: sped segments round to
whole frames, so the file runs slightly long. Measured +0.16 s on the demo,
+0.04 s on the extension — both inside the 0.5 s tolerance. It matters most for
captions (M9), which must rescale the time map to the *measured* duration; that
rescale is still unbuilt because captions are.

**The word-clip check was removed** (follow-up session). It fired when both
20 ms windows either side of a splice were loud, calling that a clipped word.
But loud-on-both-sides is the *normal* result of collapsing dead air between
two spoken phrases — the intended edit. Listening to all three failing points
on the demo confirmed the cut lands exactly where the word ends; nothing was
clipped. Actual clips are a *discontinuity*, already covered by the splice-click
check (0/27). The check could only ever fail on correct edits, so it is gone.
(For the record, its gate history: `GATE_DB = TARGET_LUFS - 15.78` = −38.78 was
calibrated for −23 LUFS audio; the −14 LUFS output is 9 dB louder, and
`MASTER_LUFS - 15.78` = −29.78 still gave 3 false failures. The threshold was
never the real problem — the check's premise was.)

The loudness tolerance on true peak needs ~0.3 dB of slack, not 0.1 — AAC
encoding pushed −1.50 dBTP up to −1.38.

### M8 — pre-flight verdict (built as one line, no separate command)

`BUILD_STEPS.md` M8 specifies a separate audit that runs M1–M3 only and stops
before rendering, to answer "is this worth editing?" cheaply. **We did not build
it that way.** The decision (this session): no `--audit` flag, no early stop.

Reasoning: the full run *already prints* every audit number (speech %, frozen %,
dead %) before it renders — the audit is literally the first part of the normal
run. The only thing missing was the verdict itself. So M8 is a single line
printed on every run, right after `dead`:

```
verdict     worth editing  (337s removable, 28.8%)          # demo
verdict     little to cut — probably not worth it  (2s removable, 2.4%)  # extension
```

- Thresholds (advisory, PROJECT_HANDOFF §15): **≥12% worth editing**, **<5%
  little to cut**, between is "marginal". They change no cut, so they live inline
  at `main.rs`, not in `constants.rs`.
- The number is **raw dead time — a ceiling.** The real edit removes less
  (collapses keep 0.5 s, speed-ups keep compressed time): demo 337 s "removable"
  vs 245 s actually removed. The line says "removable", the code comment says
  why.
- **What we gave up:** there is no fast pre-flight that skips the ~350 s render
  on a video with nothing to cut. You always run whole and read the verdict. If
  that becomes annoying, the fix is small — add an `--audit` flag that `return`s
  right after this line; no refactor needed, because the numbers already print
  here.

---

### M4b — threshold calibration (built; reads "no signal" on both files)

Sweeps `0.50 … 0.15`, rebuilds the speech mask and plan for each, and compares
the **sped-up section list**. The plateau (flat minimum of the U-shaped count)
is the stable threshold; otherwise it falls back to 0.30 **and says so**.

Cheap here: the VAD *probabilities* do not depend on the threshold, so only
`vad::speech_mask` + the plan are rebuilt — no extra VAD passes.

**On both our files it finds no signal**, so it picks the 0.30 default and the
edit is unchanged. Measured on the demo:

```
  0.50  speech 62.3%  5 sped-up  [460, 639, 703, 794, 1021]
  0.30  speech 68.0%  5 sped-up  [460, 639, 709, 794, 1021]
  0.15  speech 72.0%  5 sped-up  [460, 639, 716, 794, 1023]
```

The mask *does* move (62.3% → 72.0%), but the same 5 sections are sped up at
every threshold — only one start drifts a few seconds. The spec's demo showed
spread 6 (counts 7,7,6,6,7,12) because a borderline quiet phrase flipped in and
out of being sped up at high thresholds. **Our pipeline never produces that
spurious speed-up at any threshold** — it protects that region consistently. So
there is genuinely nothing to calibrate here; this is a real difference from the
Python, not a bug. Kept anyway: it is correct, honest, and nearly free.

### M9 — captions

`--transcript` is an optional override; otherwise the tool transcribes itself.
Words → cues (split at sentence ends) → every timing pushed through
`plan::map_to_output` → sentence/clause line-breaking → time shared by character
count → minimum `chars/18` reading time → wrap 44 x 2 → `out/captions.srt`.

**Cues are scaled to the *measured* output duration**, not the plan's. The
render runs slightly long (sped segments round to whole frames), so without this
captions drift progressively late. `scale = measured / planned` ≈ 1.0002.

**The text is raw ASR and reads badly.** Correcting it against on-screen
evidence is the AI layer, not M9. Note the reference `srt.py` has its clean text
**hand-written** as a list of 114 corrected sentences — it only uses the ASR for
*timing*. So the reference SRT is not something an automated M9 can reproduce;
what M9 guarantees is timing and structure. Chapters are deferred (the spec
never says how a chapter boundary is chosen).

### M10 — revision loop: deliberately skipped

It only pays off behind a GUI where a user clicks to undo a cut. No GUI exists,
and CLI edit commands would be clunky. Decision: build it with the GUI, so it
fits what the GUI actually needs.

### Word alignment — chunked Whisper (M11's prerequisite)

`whisper-rs` + `models/ggml-base.bin`, downloaded on first run (`models/` is
gitignored). Runs on the existing 16 kHz analysis audio.

**Chunked, not one pass.** Whisper collapses repeated phrases when decoding a
long file — its sliding window judges the repeat redundant, erasing exactly what
disfluency detection hunts for. So: 30 s chunks, 3 s overlap, `no_context` on,
and a word is kept only if its **midpoint** falls in the chunk's own region.
Measured 1929 words on the demo against the Python chunked pass's 1893.

**Suppress whisper.cpp's logging** with `whisper_rs::install_logging_hooks()`.
Without it the run log was 599 KB of spam — and the printing itself was most of
the cost: transcription dropped from 110 s to 24 s on the extension once
silenced.

Whisper-rs gives *token* timestamps, not words, so tokens are grouped into words
on the leading-space convention (Whisper's own).

### M11 — disfluency removal

**WHAT to cut comes from the words. WHERE to cut comes from the waveform.**

Four detectors: fillers, stutters, repeated phrases (cut first→**last**, keeping
only the final take, longest match claiming its span first), and extended false
starts. Every candidate then passes:

```
duration in [0.22, 3.20]  (9.5 for false starts)
similarity >= 0.55 (repeats) / 0.35 (false starts)
both splice points quieter than QUIET_DB
QUIET-RUN GUARD: >= 0.10 s of contiguous quiet at each splice
wholly inside a 1x segment — never inside a speed-up
```

**The snap windows must not be allowed to cross.** `SNAP_S` is 0.35 s but a
filler is often shorter than that, so searching ±0.35 s around each end found
the *same* quiet frame and the cut collapsed to zero length — 15 of 23
candidates died as "duration 0.00s". Fix: each end searches only its own half of
the cut. Cuts went 3 → 7 immediately. (The Python hit this too and dodged it
with a tight 0.12 s window; we keep the 0.35 guard and bound the search instead.)

**The similarity test needs a spectrum**, so `dsp.rs` carries a small radix-2
FFT rather than pulling in an FFT crate.

Expect most candidates to be rejected — that is the system working. On the demo:
17 accepted, 21 rejected (10 quiet-run, 6 duration, 3 splice, 2 not-in-1x).

### M12 — signal B behind a trait

`DeadTimeSignal` in `signal.rs`, chosen with `--signal freeze|slides|none`.
The trait arrives now and not earlier: with one implementation there was
nothing to generalise over and a plain function was correct.

| signal | genre | mask |
| --- | --- | --- |
| `freeze` | screen recording | freezedetect on the content crop (default, unchanged) |
| `slides` | slide lecture | freeze detection *inverted* |
| `none` | talking head | **all true** |

**`slides` is inverted for a reason.** A slide is a still image, so "picture is
static" is true almost everywhere and carries no information. What carries the
lecture is the slide *change*, so the mask is true everywhere EXCEPT a short
window after each change — the only moment something is happening. It reads the
whole frame, not the content crop: a slide fills the frame and there is no app
window to isolate.

**`none` returns ALL TRUE, and the sign is the whole point.** The decision is
`dead = (not speaking) AND (nothing happening)`, so an all-true mask collapses
it to silence-only — ordinary audio-based behaviour. All-*false* would mean
nothing is ever dead and the tool would silently do nothing on every talking-head
video. Measured on the extension: `none` reports 100.0% of grid, not 0.0%.

Verified behaviour-preserving: with `--signal freeze` the demo is unchanged
(89.1% frozen, 867.4 s, all five checks pass). On the extension all three signals
produce a valid edit — and the same output, because that file has almost nothing
to cut, so signal B barely participates. That is correct, not a bug.

Nothing downstream changed. The plan, time map, render, master, captions and
verification never learn which signal ran.

### M12b — the monitor loop

**The model proposes. The measurement decides.** The model is never a judge and
never sees a pass/fail question. It gets one step's diagnostic, the current
parameter values, and one sentence of *causal* guidance ("Lower = more
sensitive"), and answers with one parameter name and a direction, under a JSON
schema whose `direction` is a hard enum. The code then re-runs the step with
that parameter nudged x1.6 or /1.6, scores both results, and keeps the better.
A wrong proposal costs one extra run and is discarded by the score.

**Asking it to judge does not work, and this was measured.** Given the
verification table, qwen2.5:7b replied "OK" to a corrupted a/v sync of 0.412.
Told the healthy ranges, it caught that — but false-alarmed on clean data
(calling a 0.06 s deviation "more than 0.5 seconds"), called -1.91 dBTP "above
the maximum allowed -1.2" (sign backwards), and called -13.91 LUFS outside a
±0.2 band it is inside. It cannot do threshold arithmetic. Under the
propose/score design it does not have to.

**Verification is not monitored, deliberately.** Only a step with both an
automatic quality score and an adjustable parameter can be monitored.
Verification has neither — it is already deterministic and can already fail the
export. BUILD_STEPS M12b's "flag the right checkpoint" framing asks for the
judge design that does not work; the propose/score design is what shipped in v6.

**Guards are not adjustable here.** The reference lets the model nudge SNAP and
QUIET; this project does not, so the only accepted parameter is the VAD
threshold, which `constants.rs` marks POLICY. A proposal naming anything else is
discarded by the caller before it can do work.

**No-signal detector.** The score is `coverage - false_alarm`, so a value at or
below zero means the mask marks silence as often as speech — no better than
chance, and no basis for preferring one mask to another. Needs no invented
constant; zero is the definition. Measured:

```
demo       score range 0.244 .. 0.286   current 0.256   proposal discarded by score
extension  score range -0.004 .. 0.012  current -0.003  no signal, proposals ignored
```

Without that guard the extension accepted a 0.30 -> 0.48 threshold change on a
0.003 difference — the "tuner that cannot detect its own blindness" failure.

If ollama is unreachable the result is `ok=true`, indistinguishable from
approval, and the pipeline runs on its defaults.

### M13 — the AI layer

**The deterministic pipeline decides. The model only proposes, inside a closed
action space, behind a deterministic gate.** The video is byte-identical whether
this layer runs or not; only text artefacts improve.

`src/ai/` — `provider` (LlmProvider, Ollama, Null), `capabilities` (the probe),
`gates`, `tasks`, `monitor`. `monitor` moved here from core because it *is* an
AI task (AI_ARCHITECTURE §8.6), even though a deterministic score decides what
it changes.

**The boundary is enforced by a test, not a convention.**
`tests/core_has_no_ai.rs` fails the build if any core module references
`crate::ai`, with a second test singling out `verify.rs` — if a model could
reach verification, a model could pass a broken export.

**The probe measures, and an early version of it lied.** First cut asked the
model to echo single words, scored generation 4/4, and promoted it a whole tier
on nothing. Second cut asked for real caption repair but *put the answer in the
prompt* ("the screen shows a field labelled Duration") — still 4/4, because it
only had to copy the question back. With the answers removed:

```
qwen2.5:7b   json true  sel 0.83  gen 0.00  tier Mid  (PROVISIONAL, 10 items)
null         json false sel 0.00  gen 0.00  tier None
```

`gen 1.00 -> 0.00` the moment the answer left the prompt. That reproduces the
reference's finding: this model does selection, not generation — so caption
*correction* is disabled and only proposals-for-review are allowed. A probe that
flatters the model is worse than no probe; it is the same failure as a tuner
that cannot tell it is blind, so the tier is printed as PROVISIONAL below the
documented 30-item minimum.

**`caption.intelligible` is built; `caption.fix` is not.** The first is safest —
it only flags lines for a human, so a wrong answer costs nothing. The gate for
the second exists and is tested (`min` of direct and consonant-skeleton
similarity, never `max` — `max` let `division -> schedules` through at exactly
0.40, the precise hallucination it exists to stop), but applying fixes needs
per-scene OCR vocabulary, and no OCR engine is installed. That is the honest
blocker, not a decision.

Triage runs before any call: only lines that look risky are sent, because at a
few seconds each, sending all of them costs twenty minutes and most are fine.

### M14 — prompt → policy

`--prompt` now does something. It is parsed **once, up front**, into a bounded
`Policy` struct; after that the pipeline is deterministic given that struct. The
resolved struct is written to `out/policy.json` and `--params` replays it.

```
Policy { remove_disfluencies, dead_air: Cut|Speed|Keep, max_speed 1..=20,
         target_lufs -23..=-14, expect_screen, captions, chapters, source }
```

Where each field lands: `expect_screen` skips content-region detection and picks
the `none` signal; `dead_air` + `max_speed` drive `decide()`;
`remove_disfluencies` skips M11; `target_lufs` threads through `master()` and
`check_loudness()`; `captions` skips M9. `chapters` is parsed but unbuilt.
`--signal` and `--params` override the prompt; `--no-ai` skips the model
entirely.

**`pause_floor_s` was a policy field and is not any more.** The spec lists the
pause floor as policy, and it was built that way. It came out again after the
first time it actually mattered: a word was clipped at 6.56 s on the demo,
traced to Silero losing the last syllable — the dead run measured 1.00 s against
a 1.00 s floor and scraped in. Raising the floor would have hidden it. But how
far the VAD undershoots is a *measurement*, not a matter of taste, and a pacing
knob tuned to paper over a detection bug breaks on the next file that
undershoots by a different amount. The floor is `MIN_DEAD_S` in `constants.rs`,
one number, used by both the planner and the `trim_edges` clamp.

**The model is asked ONE question: what kind of video is this?** Everything else
is a table in code. That was not the first design, and the reason it is the
design now is measured:

| version | result |
| --- | --- |
| all 8 fields, one call | `remove_disfluencies: false` on an app demo, **3/3, deterministic** — silently disabled M11, and all five verification checks still passed |
| that same field asked alone | correct **5/5** |
| 7 separate questions | fixed disfluencies; then invented `-16 LUFS` for a description that never mentions loudness |
| 7 questions + explicit "not stated" option | fixed the inventing; but editing the *pause* question's wording flipped the *disfluency* answer, whose text had not changed |
| **1 genre pick** | **30/30 correct, identical across 5 repeats**, on all four spec examples plus both real prompts |

Three lessons, all of them ones this project had already learned once:

1. **We gated a call on a capability we never probed.** The probe measures
   "pick one option by index" (`sel 0.83`). Filling eight fields at once is a
   different task. Claiming the tier covered it is the same failure as the probe
   that flattered the model.
2. **A question with no way to abstain measures what the model will invent, not
   what the description says.** The probe already knew this — index 0 is always
   "leave it alone". The numeric questions had no such option.
3. **Tuning seven prompts against the spec's four worked examples is fitting to
   the test set.** The instability was the tell: it was pattern-matching option
   lists, not reading the description.

So the genre→policy mapping lives in `Policy::from_genre`, is the spec's own
worked-example table, and is checked by `genre_table_matches_the_spec`. It is
reviewable, diffable and identical on every run. Explicit instructions
("no subtitles", "-16 LUFS") are read by `apply_explicit` with no model at all.

`Genre::Other` returns exact defaults, and
`unclassified_is_byte_identical_to_defaults` enforces it: **the model failing
never changes the edit.**

On both test files the prompt classifies as `demo` and the resolved policy
equals the defaults, so both runs reproduce the pre-M14 numbers exactly.

## Measured results

Re-measured after `PAD_BEFORE_S` went 0.50 -> 1.50 (the word-clip fix). Every
number below moved, because that constant changes the speech mask and the speech
mask feeds everything.

### brainclean_demonstration.mp4 (1170.10 s, 1920x1080@60, −30.22 LUFS)

```
gain            +7.22 dB     peak 1.1450, 23 samples over 1.0
grid_len        58503
grid speech     43.2%   -> bridged 48.9% -> dropped 48.6% (21) -> padded 72.8%
crop            384:828:768:108      (hand-measured: 374:820:773:113)
freezes         77 blocks, 1042.7 s (89.1%)      ground truth: 80 / 1047.4
dead            25.3% of grid
energy          117006 frames (= grid_len x 2 exactly)
dead runs       23 raw -> 19 bridged -> 12 filtered -> 12 trimmed (15.9 s)
transcript      1929 words in 112 s (chunked whisper, base)
disfluency      36 cuts (25 filler, 4 stutter, 7 repeat), 45.8 s removed
                36 rejected
segments        54
output          872.2 s (25.5% removed)
master_i        -29.91 LUFS   master_tp -9.09 dBTP   gain +15.91   compressor yes
final           -13.91 LUFS, -1.79 dBTP
```

Verification (all PASS): duration 872.46 s vs plan 872.19 s (**+0.26 s**),
faststart, loudness −13.91 LUFS, splice clicks **0 / 53** (worst 0.0068 vs
p99.9 0.1090, pre-limiter 0.0647), a/v sync **worst 0.967 over 7**.

**0 clicks at 53 splices, 36 of them inside speech**, is the evidence that
cutting inside speech is safe here.

What the pad change cost, measured on this file: 60 -> 54 segments,
867.4 s -> 872.2 s output. Fewer short gaps survive to be collapsed, so ~5 s
less is removed. That is the price of not clipping words.

### brainclean_extension.mp4 (86.77 s, 1920x1080@60, −24.04 LUFS)

```
gain            +1.04 dB     peak 0.9035, 0 over
grid_len        4337
grid speech     74.7%  -> 82.7% -> 82.7% (0) -> 99.7%
crop            1068:792:276:240
freezes         7 blocks, 84.4 s (97.2%)
dead            0.3%
dead runs       1 raw -> 1 bridged -> 0 filtered
transcript      201 words in 101 s
disfluency      1 cut (1 filler), 0.2 s removed, 0 rejected
segments        2
output          86.5 s (0.3% removed)
master_i        -24.17 LUFS   master_tp -4.15 dBTP   gain +10.17   compressor yes
final           -14.20 LUFS, -1.80 dBTP
```

Verification (all PASS): duration 86.60 s vs plan 86.54 s (**+0.07 s**),
faststart, loudness −14.20 LUFS, splice clicks **0 / 1**, a/v sync **worst
0.999 over 7**.

**Watch the loudness here.** −14.20 against a −14.0 target is a deviation of
exactly 0.20, and the tolerance is ±0.2. It passes on the boundary. This file
has drifted upward as other things changed (−14.06 -> −14.17 -> −14.20) and the
next nudge in the same direction fails the export. It is the first thing to
check if this file starts failing.

At 99.7% padded speech there is essentially nothing to cut: the single dead run
does not clear the 1.00 s floor, so the entire edit is one filler cut. The
threshold sweep is flat too (0 sped-up sections at every threshold), so
calibration reports no signal and uses 0.30.

**Python v6 reference produced 86.00 s from this file (0.9% removed).** Two
independent implementations reaching the same conclusion on a file with
essentially nothing to cut.

### Cross-check: energy vs VAD

45.2% of energy frames above `GATE_DB` against 43.2% raw grid speech on the
demo file. Two independent measurements — one a neural VAD, one RMS energy —
landing 2 points apart. The strongest available evidence that the dB thresholds
transfer.

Note the extension file reads **74.9%** above gate against 74.7% grid speech —
also a match, but a much higher figure, because continuous narration pulls
programme loudness up toward the speech level. The edge guard consequently has
less to bite on: it trimmed 15.9 s on the demo file and 0.0 s on the extension.
The guard does the most work exactly where there is the most to trim.

---

## Open issues

### 1. Word-clip check — RESOLVED (check removed)

The three failing points (78.34 s, 291.76 s, 635.73 s, all at collapse points,
so 3 of 17) were listened to. The cut lands **exactly where the word ends** —
nothing is clipped. It was the second hypothesis: the check was too strict, but
more fundamentally its *premise* was wrong. "Loud on both sides of the splice"
is the normal result of collapsing dead air between two phrases, not evidence of
a clipped word. A real clip is a discontinuity, already caught by the
splice-click check. The check could only fail on correct edits, so it was
removed. See the M7 section for the full reasoning.

### 2. Gap to the Python reference

```
                Rust            Python v6      gap
demonstration     872.2 s         845.7 s      26.5 s
extension          86.5 s          86.00 s      0.5 s
```

The extension is effectively exact. The demonstration was 79 s adrift before
M11 and 56 s after it; the current figure is **26.5 s**. Disfluency removal now
takes 45.8 s in 36 cuts against the reference's 27.5 s in 28 — we remove *more*
seconds than the reference here, so the residual gap is dead air, not
disfluency.

Per-kind, against the reference: filler 25 cuts / 12.8 s (ref ~26 / ~17.4 s),
repeat 7 / 25.9 s (ref 9 / ~40.5 s). Fillers are close in count and short in
seconds; repeats are short in both.

**Do not widen a guard to close this gap** — that is exactly the trade the
guards exist to prevent. See issue 11 for why the missing repeats are not
reachable by tuning.

### 3. dts warnings on sped segments

Hundreds of `non monotonically increasing dts` around frame 27258 (~454 s),
adjacent to the 47.2 s speed-up at 457 s. `setpts=PTS/12` lands multiple source
frames on the same output frame.

**Probably benign.** SSIM came back 0.991 at all 7 checkpoints, better than the
v6 reference. The warnings appear when writing to the null muxer. If they ever
matter, the fix is `fps=60` after `setpts` in sped segments, forcing frame-rate
resampling instead of duplicate frames — at the cost of re-rendering the 5 sped
segments.

### 4. `--crop` override not implemented

Auto-crop works. The documented override does not exist yet.

### 5. Prompt is unused — RESOLVED (M14)

Built. See M14 above. One deviation from `CLI_AND_PROMPT.md` §2, and it is
deliberate: the spec describes one model call that fills all eight fields, which
was built first and measured wrong (3/3 deterministic failure on an app demo).
The model now answers one classification question and the field mapping is a
table in code. The observable contract is unchanged — bounded fields, defaults
when the model is unreachable, resolved struct logged to `out/`, `--params`
replays.

Still open here: `chapters` is a parsed field with nothing behind it.

### 6. temp/ is never cleaned

~1 GB per run on the demo file: 75 MB WAV, 28 segments, `concat.mkv` (273 MB),
`master.mkv`, PNG frames from the sync check. Keeping the WAV was a deliberate
choice (open it in Audacity when a mask looks wrong). The rest is undecided.

### 8. Parallel render (M5b) was built, measured, and removed

Scoped threads + an atomic work queue, output byte-identical. But wall clock
went **348 s → 330 s with 12 workers** — about 5%. Each ffmpeg/libx264 encode
already multithreads across every core, so running 12 at once just
oversubscribes; the total CPU work is unchanged. Reverted to serial rather than
keep complexity that buys nothing. A real speedup would need capped
`-threads` per job, a faster preset, or GPU encoding.

### 9. Whisper dominates the runtime

Transcription is now the slowest stage: ~330 s for the 20-minute demo (render is
~470 s with 45 segments). Unresolved by choice — the options are a smaller
model or different settings, and neither has been measured.

### 10. Cosmetic: "-0.0s removed"

The disfluency line prints `-0.0s` when there are zero cuts. Harmless.

### 7. `main.rs` split — RESOLVED

Done in the follow-up session. `plan`, `features`, `render`, `master`, `verify`
are now separate modules; `main.rs` is imports + `main()` only. Verified
behaviour-identical on both test files (same numbers, all five checks pass).

### 11. Whisper hides repeats, and we cannot currently find them

**This one is investigated, measured, and closed as not-reachable. Read this
before trying again — three plausible routes were tested and all three failed.**

The symptom: the speaker says *"...active, pending, **or you just or you just**"*
and abandons the sentence. Whisper writes `"all you just have created"` — the
repeat appears **once**. Our repeat detector matches words, so a repeat that
isn't in the text cannot be found. `MIN_PHRASE` is irrelevant here; the words
are simply not there.

What was tried:

**A bigger model.** Real `medium` (1.5 GB — note the previous `ggml-medium.bin`
in `models/` was a copy of base, same md5, because the download URL and the
filename were written out separately and disagreed; every "medium" run was
really base). Medium fixes the *words* — "tasks" not "data", "active, pending"
not "x-tap, fending" — but still collapses the repeat to one copy. 1726 s vs
160 s for the file. **Model size does not fix this.**

**Shorter whisper chunks.** This *works* — at 10 s chunks both takes survive:
`"or you just that all you just have created"`. Two problems. Accuracy drops
("tiles" for "tasks"), and the cost is prohibitive: whisper pads every chunk to
30 s internally, so cost scales with chunk *count*, not audio length. Measured
per-chunk cost is flat (~17-23 s), so 10 s chunks on the 19-minute file is
~130 chunks ≈ 87 min, and 5 s chunks ≈ 2.9 hrs. It is viable only on short
clips. Worse, the two takes come out spelled *differently* (`"or you just"` /
`"all you just"`), so exact word matching still fails even when both survive.

**Finding repeats in the waveform.** The idea: skip the text, detect repeats
acoustically, re-transcribe only those spans to confirm. Measured against the 7
repeats the text detector already finds, versus 40 samples of ordinary adjacent
speech:

```
                  YES (real repeats)   NO (ordinary speech)   overlap
dsp::similarity   0.664 - 0.837        reaches 0.896          92%
MFCC + DTW cost   34.3 - 43.2          down to 34.3           40%
```

`dsp::similarity` sums the spectrum into 24 bands and averages the span, so it
measures *voice and room*, not words — same speaker, same mic, high score
whatever was said. It is fine as a confirmation for a repeat the words already
found, which is its actual job, and useless as a detector.

MFCC+DTW is the right tool in principle (mel-cepstral coefficients describe
vocal-tract shape rather than pitch or loudness; DTW allows the second take to
run faster) and it did halve the overlap. It still failed. A full-file scan
returned 1 real repeat in 28 candidates, and — decisively — **it could not rank
the 7 known repeats into its own top results.** Something that cannot find
repeats we know are there will not find hidden ones.

Why it failed is worth recording: spans were anchored at word boundaries with
fixed lengths, comparing `[t, t+L]` against `[t+L, t+2L]`. The two takes of a
real repeat have *different durations*, so that split almost never lands on the
actual take boundaries. DTW absorbs speed differences *within* a pair; it
cannot fix a window that is slicing the takes in the wrong places.

The honest next route, if this is ever picked up again, is a proper
self-similarity matrix over the whole file with diagonal-stripe detection —
the standard algorithm for this problem, and a real piece of work with no
guarantee. The exploratory code (`src/mfcc.rs`, `examples/repeat_scan.rs`,
`examples/chunk_test.rs`) was reverted, not kept; the numbers above are the
part worth keeping.

### 12. Error handling — RESOLVED

**No `?` anywhere.** All 113 were rewritten as explicit `match`, each error arm
carrying its own message. `?` was already deterministic — it is exactly
`match x { Ok(v) => v, Err(e) => return Err(e.into()) }` — so this changed no
behaviour: both test files give identical numbers before and after, all five
checks passing with the same values. What it bought is that every failure path
is written out and says in words which step failed.

The symptom that prompted it — "an error happened and the program kept going
with no message" — was **not** caused by `?`, which always stops. It was
caused by code that turned a failure into a default and carried on. An audit
of every `.unwrap_or*`, `.ok()`, `if let Ok` and `None => continue` found these,
now fixed:

| where | what went wrong silently | now |
| --- | --- | --- |
| `freeze.rs` | an unreadable `freeze_end` left its freeze open; `paint_freezes` paints an open freeze **to the end of the file**, so everything after it became dead air to cut | hard error — also for a start while one is open, an end with no start, and a double end |
| `verify.rs` `check_sync` | `worst` folds from 1.0, so with every checkpoint skipped it read a perfect 1.000 and **passed having checked nothing** | zero checkpoints is a FAIL; skipped ones are counted in the detail |
| `verify.rs` `ssim` | the one ffmpeg call that never checked its exit code | checked; the real ffmpeg error is reported |
| `verify.rs` `check_clicks` | `segments.len() - 1` wraps to a huge number in a release build if there are no segments | `saturating_sub` |
| `main.rs` proofread | read `captions.srt` with `unwrap_or_default()`, **and read it even with captions off** — so it could proofread a stale file an earlier run left in the folder | only read when captions are on; a failed read prints a warning |
| `align.rs` | a missing segment, missing token or unreadable token text made the word vanish from the transcript | still skipped (one bad token should not abort a 20-minute run) but prints a warning with the time |
| `policy.rs` | genre classification fell back to defaults without saying why | warning says whether the model was unreachable or replied with something unreadable |

Also fixed on the way: `measure_loudness` reported a failure as "failed to
extract audio", a copy-paste of the wrong message; and the ffprobe error said
"fffmpeg".

Deliberately left as defaults, because the default is the correct answer and
not a hidden failure:

- `paint_freezes` treating the last open freeze as running to the end — the
  video ends frozen. Rewritten as a `match` with a comment, and now safe
  because `detect_freezes` guarantees only the last freeze can be open.
- The AI capability probe (`capabilities.rs`) scoring a failed call as a miss,
  via `if let Ok`, and the monitor's `.unwrap_or(true)` on unreadable output
  (`tasks.rs`). Both are the documented fallbacks and were **left unchanged** —
  they contain no `?`, so the rewrite did not touch them.

The guard against regressions is `#![deny(unused_must_use)]` in both `lib.rs`
and `main.rs`: calling a fallible function and ignoring its `Result` — the
original "forgot the `?`" bug — is now a compile error anywhere in the crate.

---

## Guards vs policy

`CLI_AND_PROMPT.md` §1 is emphatic and it holds:

**Guards are fixed in code, forever.** `QUIET_DB`, `GATE_DB`, `QUIET_RUN_S`,
`SNAP_S`. The disfluency score (M13) rewards removing more seconds; it is only
honest because these make damaging cuts *unavailable*. A prompt that could
widen a guard could talk the pipeline into destroying the audio and would score
itself higher for doing it.

**Policy is what the prompt sets.** Whether to remove disfluencies, how hard to
compress dead air, target loudness, `expect_screen`, `face_bubble`.

`QUIET_DB` and `GATE_DB` are *derived* from `TARGET_LUFS` in `constants.rs`
rather than written as −44.78 and −38.78, so the coupling is in the code
instead of only in someone's head.

Since M14 the line is enforced by the build, not by care:
`tests/prompt_cannot_touch_guards.rs` fails if `policy.rs` names any of the four
guards, if the `Pacing` struct the planner receives exposes one, or if a numeric
policy field stops being clamped.

Two places where the distinction needed a judgement call, both recorded here:

* **`MIN_DEAD_S` is a constant, and the argument for splitting it was wrong.**
  It was briefly split: the planner's floor became policy (`pause_floor_s`)
  while the `trim_edges` clamp stayed a constant. The reasoning sounded fine —
  pacing is taste, the clamp is safety. It did not survive contact with a real
  bug. A clipped word traced back to a dead run of exactly 1.00 s against a
  1.00 s floor, and the tempting fix was to raise the floor. That would have
  been tuning a taste knob to hide a VAD miss. **Test for "is this policy?":
  can a user's description tell you the right value?** For loudness and
  disfluencies, yes. For the pause floor, no — the right value depends on how
  far the VAD undershot on this file, which is measured, not described.
* **`target_lufs` is policy, its tolerance is not.** The prompt picks the
  delivery target within −23..−14. The ±0.2 LUFS that `check_loudness` accepts
  is fixed whatever target is asked for, because it describes what single-pass
  loudnorm actually delivers, not what anyone wants.

---

## Habits that paid off

- **Check numbers against arithmetic, not against plausibility.** 70206 frames
  ÷ 60 fps = 1170.1 s, and 1170.1 s = 19:30, which matches `FLOW_SPEC.md`. That
  is how you know the duration is real.
- **Compare against an artifact, not against your own judgement.** The Python
  produced `brainclean_extension_v6.mp4` at 86.00 s. That number settled a
  question that listening could not.
- **A print that never changes is a print that isn't measuring what you think.**
  The smoothing bug hid for two rounds because the distribution being printed
  read the raw array while the guard read the smoothed one.
- **Every fallible call is a `match`, and an ignored `Result` does not build.**
  A missing `?` on `concat_segments` once made a failing ffmpeg invocation
  silently succeed. The codebase now uses no `?` at all — each call is an
  explicit `match` with its own message in the error arm — and
  `#![deny(unused_must_use)]` sits at the top of **both** `lib.rs` and
  `main.rs`, so dropping a `Result` anywhere is a compile error. (It used to be
  `#[deny]` on `fn main` alone, which covered nothing else in the crate.)
- **`?` was never the thing hiding errors — defaults were.** When a run
  "failed with no error", the cause was `.unwrap_or_default()`, `.ok()`,
  `if let Ok(..)` and `None => continue`: code that turns a failure into a
  default value and carries on. Those are the lines to audit. See
  "Error handling" under Open issues for the ones that were found.
- **rustc's `help:` blocks are local text fixes.** One suggested
  `Result<(), E>` where the real problem was a missing import four lines up.
  Treat them as hints about where the confusion is, not as patches to paste.
- **Measure whether a signal can work before building on it.** The acoustic
  repeat detector was killed by a one-hour test: score the cases you know are
  positive, score the negatives, see whether the two ranges separate. They
  overlapped 92% and 40%. That hour replaced a week of building something that
  could not have worked. The test is cheap because it needs no product code —
  just the measure and two lists.
- **Change one variable at a time, and check that you did.** A `git stash`
  quietly parked an unrelated `SNAP_S` edit along with the change under test,
  so "old code" versus "new code" was really *old code + SNAP 0.35* versus
  *new code + SNAP 0.25*. The conclusion drawn from it was wrong. `git diff
  --stat` before trusting a comparison; a file you did not expect to see in
  that list is the whole answer.
- **A negative result is worth writing down in full.** Issue 11 exists so the
  next person does not re-run three dead ends. What it cost to learn is the
  reason to keep it.
