# Build steps — follow this in order, top to bottom

> **`MONITORED_FLOW.md` describes the CURRENT flow (v6) and wins on any
> disagreement about steps or parameters.** This document remains correct on
> architecture, rendering, mastering and verification.


Companion to `PROJECT_HANDOFF.md`. That document is *what* and *why*. This one is
*in what order*, and *how you know a step is finished*.

**Follow it literally. You should not need to ask anyone what to do next.**

---

## Correction to the handoff — read this first

`PROJECT_HANDOFF.md` says to define a `DeadTimeSignal` trait in step 1 and calls
retrofitting it "the one refactor that would genuinely hurt."

**That was overstated. You were right to push back.** Here is the accurate
version:

- Building abstractions before you have two implementations is a mistake. You
  have one. Write a plain function.
- Converting a function into a trait later is a **10-minute change**, not a
  painful refactor — *provided* the data shapes are right.
- **The data shapes are the only thing that is genuinely expensive to change
  later.** Specifically these two:

> **Rule A.** Pick one time grid (20 ms) at the start. Every mask in the program
> is a `Vec<bool>` of the same length, indexed the same way.
>
> **Rule B.** `plan.json` is the only thing rendering, captions, chapters and the
> revision loop are allowed to read. Nothing downstream ever re-reads the audio
> or the masks.

Obey those two and you can add traits, genres and plugins whenever you like.
Ignore them and you will rewrite the middle of the program.

So: **no traits until Milestone 12.** Plain functions. Get it working.

---

## How to use the Claude chat without going in circles

It flip-flopped because it was agreeing with whoever spoke last. Anchor it:

1. Start each session with: *"I am on Milestone N of BUILD_STEPS.md. Here is that
   milestone. Only discuss this milestone."* Paste the milestone text.
2. If it proposes anything outside the current milestone, reply: *"Not in
   Milestone N. Park it."* Do not debate the merits — the ordering is already
   decided.
3. If it contradicts itself, quote the milestone's **Done when** line and ask
   whether its suggestion is required to satisfy that line. If not, it is out of
   scope.
4. Use it for: Rust idioms, crate APIs, error handling, why your code doesn't
   compile, reviewing what you wrote. Do not use it for: what to build next, or
   whether to add an abstraction. That is decided here.
5. It cannot see your code unless you paste it. It cannot run your video. When
   it makes a claim about behaviour, **measure it yourself.**

---

## Milestone 0 — walking skeleton

**Goal:** a binary that runs, takes a file path, and talks to ffmpeg.

**Build:** `clap` CLI with one subcommand `probe <file>`. Shell out to `ffprobe`,
parse duration, fps, resolution, audio sample rate. Print them.

**Done when:** `elide probe demo.mp4` prints the real duration and resolution,
and exits non-zero with a clear message if the file is missing or ffmpeg is not
installed.

**Do not:** add a config file, a logging framework, or any module structure
beyond `main.rs` and one helper file.

---

## Milestone 1 — audio into memory

**Goal:** the raw samples, as floats.

**Build:** shell out to `ffmpeg -i src -vn -ac 1 -ar 16000 -c:a pcm_s16le a16.wav`
into a temp dir. Read it with `hound`. Convert `i16` to `f32` by dividing by
32768.0.

**Done when:** you print sample count, sample rate, and computed duration, and
the duration matches Milestone 0's probe to within 0.1 s.

**Why this file exists:** it is a measurement instrument. The audio you ship
always comes from the original file. Never render from `a16.wav`.

---

## Milestone 2 — the speech mask

**Goal:** `Vec<bool>` at 20 ms, true where someone is speaking.

**Build:**
1. Run Silero VAD (ONNX, via the `ort` crate) over the samples. It returns speech
   spans in seconds.
2. Paint those spans onto your 20 ms grid.
3. Apply the three post-processing steps, **in this order**:
   - bridge gaps shorter than **0.35 s** → speech
   - drop speech bursts shorter than **0.25 s** → not speech
   - pad **0.50 s before**, **0.55 s after** every speech run

**Done when:** you print `speech: NN.N%` and, on a normal talking video, it lands
roughly between 60% and 90%. Print the 10 longest silences with timestamps and
spot-check two of them by scrubbing the video.

**Do not hard-code the VAD threshold.** Calibrate it per video -- see Milestone
4b. Until you build that, use **0.25**, which was correct for one test video and
is not a universal answer.

**Do not skip the post-processing.** Raw Silero excludes gaps *between words*, so
without it you get hundreds of fake micro-silences. Measured: 78.4% → 95.8%
agreement with a known-good mask.

---

## Milestone 3 — the "nothing happening" mask

**Goal:** a second `Vec<bool>`, same length, true where the picture is static.

**Build:** shell out to
`ffmpeg -i src -vf "crop=W:H:X:Y,fps=5,freezedetect=n=-58dB:d=2.0" -an -f null -`
and parse `freeze_start` / `freeze_end` from **stderr**. Paint onto the grid.

Take the crop as a required CLI argument for now: `--crop 374:820:773:113`.

**Done when:** you print `static: NN.N%`, and running it with a deliberately
wrong crop (e.g. the whole frame on a windowed app) visibly changes the number.
That proves you are actually parsing it.

**A plain function is correct here.** Signature roughly:
`fn frozen_mask(src: &Path, crop: &str, grid_len: usize) -> Result<Vec<bool>>`.
One implementation, no trait.

---

## Milestone 4 — the plan (first genuinely useful milestone)

**Goal:** decide the edit without rendering anything.

**Build:**
1. `dead[i] = !speech[i] && frozen[i]`
2. Bridge dead runs separated by less than **2.0 s**, then re-apply
   `&& !speech[i]` — *a bridge must never cover speech*. This was a real bug;
   without the re-apply it swallowed words.
3. Find runs of `dead` at least **1.0 s** long.
4. For each run of length `d`:
   - `d < 4.0 s` → keep only the first **0.50 s**, drop the rest
   - `d >= 4.0 s` → keep it all, but at speed
     `target = clamp(d/12, 1.2, 6.0)`, `speed = min(20, d/target)`
5. Separately: runs where `!speech && !frozen` and length ≥ **1.5 s** → keep the
   first **0.80 s**.
6. Emit `Vec<Segment>` where `Segment { src_start, src_end, speed }`, plus a time
   map `(src_start, src_end, speed, out_start)`.
7. Serialize to `plan.json` with `serde`.

**Done when:** `elide plan demo.mp4 --crop ...` writes `plan.json` and prints:

```
source    19.5 min
speech    74.8%   static 89.6%
segments  23  (6 sped up)
output    15.6 min   removed 233s (19.9%)
```

**Check it by hand before moving on.** Open `plan.json`, pick a sped-up segment,
scrub the source video to that timestamp, confirm it really is a dead wait. If it
is not, your masks are wrong and rendering will only hide that.

**Do not:** render anything yet. This milestone is worth having on its own.

---

## Milestone 4b — calibrate the threshold per video (automatic, no labels)

**Goal:** stop hard-coding a VAD threshold that was tuned on someone else's video.

**Why this milestone exists:** a threshold that suits one recording suits another
badly, and the failure is invisible at the mask level. Measured on one video, the
mask barely moved across the whole threshold sweep (71.4% to 76.5% speech) but
the *plan* changed materially, because the planner amplifies small mask
differences -- one short speech island decides whether a 51-second block becomes
a single speed-up or several.

**The method -- and note *what* is being compared:**

1. Sweep the VAD threshold: `0.50, 0.40, 0.30, 0.25, 0.20, 0.15`.
2. For each, build the **plan** (Milestone 4). Do not render.
3. Record two things: output duration, and the list of sped-up section start
   times rounded to the second.
4. Find the **widest run of adjacent thresholds that produce the same sped-up
   list**. That is the stable plateau.
5. Pick the **highest threshold inside the plateau** -- the most conservative
   setting that is still stable.

**Done when:** on a test video it prints the sweep and its choice, and re-running
gives the same answer. On one measured video:

```
   thr  speech%   output   sped-up sections
  0.50    71.4%   15.03m   [362, 457, 580, 636, 708, 792, 1019]
  0.40    71.6%   15.03m   [362, 457, 580, 636, 708, 792, 1019]
  0.30    72.3%   15.10m   [457, 580, 636, 713, 792, 1019]   <- plateau
  0.25    72.7%   15.11m   [458, 580, 636, 713, 792, 1019]   <- plateau
  0.20    74.1%   15.24m   [458, 636, 713, 794, 806, 834, 1019]  fragmenting
```

0.50/0.40 contain a spurious speed-up at 362 s over a real quiet phrase. 0.20
fragments one dead block into three. The plateau is 0.30-0.25.

### The metric must know when it is blind

This calibration was derived from one video. Tested on two more, it mostly did
not work:

```
brainclean   7, 7, 6, 6, 7, 12   spread 6   usable -- clear minimum at 0.30/0.25
video_2      5, 5, 5, 5, 5, 4    spread 1   weak, and the minimum sits at the
                                            EDGE of the sweep (a false minimum)
video        1, 1, 1, 1, 1, 1    spread 0   no signal at all
```

**One video in three.** So the rule is not "pick the minimum", it is:

```
spread >= 2                          -> calibrated, use the minimum
spread == 1 and minimum not at an edge -> weak, use it but mark provisional
otherwise                             -> no signal: fall back to 0.30 and SAY SO
```

The metric needs **enough sped-up sections for the count to vary**. A short
recording, or one with few waits, cannot produce that. Usefully, the case where
it fails is largely the case where it does not matter -- `video.mp4` had only 8 s
of removable material at *any* threshold.

**A tuner that cannot detect its own blindness is worse than a fixed constant:**
it converts "I don't know" into a confident wrong answer. Every auto-tuned
parameter you add needs this same no-signal check, and it must be visible in the
output, not hidden.

**Cost:** one VAD pass per threshold (~14 s per 20 min of audio) plus trivial plan
arithmetic. About 90 seconds total. Cache the result per file.

**What does NOT work, so you do not waste time on it:** scoring the *mask*
instead of the plan. Sweeping and measuring how cleanly each mask separates the
audio by energy and pitch gave a curve flat to within 1% -- pure noise -- and on
two of three test videos it chose the extreme end of the sweep. **The plan is the
thing to compare.** Same rule as the component-swap lesson below.

---

## Milestone 5 — render

**Goal:** a watchable output file.

**Build:**
1. For each segment, shell out:
   `ffmpeg -ss <start> -t <dur> -i src.mp4 -c:v libx264 -preset fast -crf 19 -profile:v high -pix_fmt yuv420p -g 120 -c:a pcm_s16le -ar 48000 -ac 2 seg/NNN.mkv`
2. Sped segments add `-vf "setpts=PTS/<speed>"` and
   `-af "atempo=...,volume=0"` — chain `atempo` in factors of ≤2.
3. Write `concat.txt`, then
   `ffmpeg -f concat -safe 0 -i concat.txt -c copy joined.mkv`

**Done when:** `joined.mkv` plays, its duration matches the plan's predicted
output to within 0.5 s, and audio stays in sync at the end of the file.

**Do it serially first.** Parallelism is Milestone 5b. Correct before fast.

**Why not one big filter_complex:** 50 trim branches off one input makes ffmpeg
buffer the whole decode graph and memory balloons.

---

## Milestone 5b — parallel render

**Goal:** cut the wall clock.

**Build:** run the per-segment encodes across N workers (`rayon`, or a bounded
thread pool spawning ffmpeg processes). Default N = physical cores, `--jobs` to
override.

**Done when:** output is **byte-identical** to the serial render, and wall clock
drops materially. Compare with a hash. If bytes differ, you have a bug —
per-segment encoding is deterministic.

---

## Milestone 6 — audio mastering

**Goal:** YouTube-legal loudness.

**Build:**
1. Measure: `ffmpeg -i joined.mkv -af "highpass=f=80,loudnorm=I=-14:TP=-1.5:LRA=11:print_format=json" -f null -`
   and parse the JSON from stderr.
2. **Do the arithmetic and print it.** Required gain = `-14 - input_i`. Resulting
   peak = `input_tp + gain`. If that exceeds −1.5 dBTP, a flat gain cannot work
   and compression is required.
3. Apply: `-c:v copy -af "highpass=f=80,afftdn=nr=12:nf=-45,acompressor=threshold=-28dB:ratio=3:attack=10:release=250:makeup=8,loudnorm=I=-14:TP=-1.5:LRA=11" -c:a aac -b:a 192k -ar 48000 -ac 2 -movflags +faststart out.mp4`

**Done when:** re-measuring `out.mp4` with `ebur128` gives **−13.9 to −14.1
LUFS**, and `-c:v copy` means the video was not re-encoded (check the file size
of the video stream is unchanged).

---

## Milestone 7 — verification (do not skip; this is the product)

**Goal:** the tool can prove it did not break the video, and can refuse to export.

**Build these four, in this order of value:**

1. **Duration check.** Output duration matches plan prediction within 0.5 s.
2. **A/V sync via SSIM.** Pick 7 output timestamps. For each, use the time map to
   compute which *source* timestamp should appear there. Extract both frames with
   ffmpeg, compare with ffmpeg's `ssim` filter. **Expect > 0.98.** This single
   test validates the plan, the rendering, the concat and the speed-ups at once.
3. **Loudness.** Within 0.2 LUFS of target.
4. **Faststart.** Read the top-level atoms; `moov` must precede `mdat`.

**Done when:** `elide verify out.mp4` prints a pass/fail table, and you can make
it **fail on purpose** by hand-editing `plan.json` to shift a segment. If you
cannot make it fail, it is not testing anything.

**Investigate your worst SSIM checkpoint, not your average.** Doing that on a
finished edit revealed a real bug: sped-up segments round to whole frames, so the
output ran **+0.3 s** long and the time map drifted **0.2 s** by two-thirds of the
way through. Early checkpoints matched perfectly, which is exactly why an average
hides it. Audio/video stay in sync; **captions go progressively late.**

Add a fifth check: **actual output duration vs planned**, and rescale the time map
to the measured duration before generating captions.

---

## Milestone 8 — the audit command

**Goal:** tell the user whether this tool will help, before it does any work.

**Build:** run Milestones 1–3 only, then report speech %, static %, and total
removable seconds. Recommend: ≥12% removable → worth running; <5% → say so
plainly.

**Done when:** it runs in about a minute on a 20-minute video and correctly
reports "little dead air" on a video where someone talks continuously.

---

## Milestone 9 — captions and chapters

**Goal:** an SRT that stays in sync with the edit.

**Build:** take ASR output (Whisper, Milestone 10, or a supplied transcript for
now), push every timestamp through the plan's time map, break lines on sentence
then clause boundaries, cap at 44 chars and 2 lines, guarantee each cue lasts at
least `chars/18` seconds.

**Done when:** loading the SRT next to the output video, captions land on the
right speech at 3 spot-checked points, including one right after a speed-up.

---

## Milestone 10 — the revision loop

**Goal:** review the output and change your mind cheaply.

**Build:**
1. When planning, record *why* each removal happened: `DeadAir`, `Wait`,
   `Filler`, `Repeat`, `FalseStart`. Store it in the plan.
2. `elide list` — print every removal with its **output** timestamp (from the
   time map), an id, and the reason.
3. `elide undo <id>` / `elide cut <mm:ss-mm:ss>` — amend the plan and re-write it.
4. Re-render **only changed segments**; stream-copy the rest.

**Done when:** an `undo` completes in under a minute on a 15-minute edit, versus
a full render. Measured in Python: 4.3 s to re-encode one segment + 2.3 s to
re-concat + 49 s audio master.

---

## Milestone 11 — disfluency removal (behind a flag)

**Goal:** remove fillers, stutters, repeats and false starts.

**This is the first thing that cuts inside speech.** Different risk class. Do not
start it until Milestone 7 verification is solid.

**Build:**
1. Whisper with **word timestamps** (`whisper-rs`). Check the output: if every
   word's `end` equals the next word's `start`, the timings are useless and you
   cannot proceed.
2. Detectors, gated by the three tests in `PROJECT_HANDOFF.md` §8.
3. Subtract accepted cuts from 1× segments only.
4. **Add the splice test to verification:** max sample-to-sample jump at each
   splice must not exceed the file's own 99.9th percentile.

**Done when:** you listen to 5 cuts and hear no clicks, and the splice test
passes with zero exceedances.

---

## Milestone 12 — abstractions, *now* that you have two of something

**Goal:** make a second genre cheap.

**Only now** convert `frozen_mask()` into a trait, because you are about to write
a second implementation (slide-change detection) and finally have something to
generalise over.

**Done when:** the same binary handles a slide lecture with
`--signal slides` and the decision engine is untouched.

---

## Milestone 12b — the pipeline monitor (cheap, safe, surprisingly useful)

**Goal:** have a model watch the tool's own diagnostics and say "that number
looks wrong, go look at X."

**Build:** every step already prints a small table -- SSIM checkpoints, the
threshold sweep, per-segment loudness, splice jumps. Feed each table to the model
with one question: *is any value anomalous, and what should be measured next?*
Print the answer. **Change nothing based on it.**

**Done when:** on a run with a deliberately corrupted segment, it flags the right
checkpoint.

**Why this is worth doing even with a weak model:** measured on four real tables,
it caught the one-value outlier (the frame-rounding drift -- the single hardest
thing to notice by hand) and correctly stayed quiet on clean data, but missed a
structural problem and misdiagnosed another. A false alarm costs a minute; a miss
leaves you where you already are. Nothing it says can damage the edit.

**Feed it the worst row, not the average.** The bug it caught was invisible in
the average.

---

## Milestone 13 — the AI layer

Read `AI_ARCHITECTURE.md`. Order: provider trait → capability probe →
`caption.intelligible` (safest, only flags) → OCR → `caption.fix` as gated
proposals.

**The core must never import the AI module.** Enforce it in CI.

---

## The "not yet" list — say no to all of these until their milestone

| Thing | Until |
| --- | --- |
| Any trait or plugin system | M12 |
| Any AI or LLM | M13 |
| Config files (TOML/YAML) | you have 3+ params you actually change |
| GUI or web UI | never, until the CLI is used by someone else |
| Auto-detecting the crop | after M8; `--crop` is fine |
| Content presets | M12 |
| Parallelism | M5b, after serial works |
| Custom error types beyond `anyhow` | when a caller needs to match on them |
| Refactoring into many crates | when compile times actually hurt |

---

## One more rule, learned the expensive way

When you swap a component -- a different VAD, a different model, a tuned
threshold -- **do not validate it on that component's own output.**

Silero was compared to the previous detector at mask level: 95.8% frame
agreement, 11/11 labelled regions correct. It looked equivalent. Running the real
planner on both masks produced a **39-second difference** in the finished edit,
and two spans of quiet speech were treated differently.

**Compare `plan.json`.** That is the artifact everything downstream depends on,
and the planner amplifies small mask differences: whether a 12-second span counts
as speech decides whether a 51-second block becomes one big speed-up.

---

## The one habit that matters

Every milestone ends in something you can **run and check**. If you cannot state
what you would observe to know a step worked, you are not ready to write it.

That is the same rule that produced every number in the handoff: **measure, then
decide.** It applies to your own code exactly as much as it applied to the audio.
