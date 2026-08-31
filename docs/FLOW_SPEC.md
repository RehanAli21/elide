# The flow to build (v6)

What actually runs today. Supersedes the flow sections of `BUILD_STEPS.md` and
`PROJECT_HANDOFF.md` wherever they disagree; those remain correct on
architecture, rendering, mastering and verification.

Result on `brainclean_demonstration.mp4`: **19:30 → 14:06, 27.8% removed**,
35 cuts inside speech, 0 clicks, 0 word-clips.

| version | runtime | speech cuts | repeat cuts | what changed |
| --- | --- | --- | --- | --- |
| v3 | 15:10 | 28 | — | first disfluency pass |
| v4 | 14:37 | 27 | — | auto-calibrated VAD threshold |
| v5 | 14:36 | 33 | 3 | quiet-run guard, AI monitor, normalisation |
| **v6** | **14:06** | **35** | **9** | **chunked alignment, keep-last, SNAP 0.35** |

---

## The three changes that made v6

### 1. Whisper was deleting the repeats (the big one)

Transcribing the full 20 minutes in one pass, Whisper collapsed

> *"It will show you the features. **It will show you the features.** It will
> show you how to use it."*

down to a single instance. **19 words became 3** in that span. The disfluency
detector was blind to the repeat because the transcript it reads had already
erased it — the ASR removes exactly the thing this feature exists to find.

The same model on the same 22 seconds *in isolation* keeps every word. So it is
not a model-size problem; it is Whisper's sliding-window decoding suppressing
repetition across a long file.

**Fix: transcribe in independent 30 s chunks with 3 s overlap.** Each window is
decoded with no knowledge of its neighbours, so nothing can be judged redundant.
Keep a word if its midpoint falls in the chunk's own region, which removes seam
duplicates.

```
full-file pass : 1809 words, 211 real gaps, repeats missing
chunked pass   : 1893 words, 215 real gaps, repeats present
```

**Check this on every video.** If a known repeat is absent from the transcript,
nothing downstream can recover it.

### 2. Keep only the last take

When a phrase occurs N times, cut from the **first** occurrence to the **last**,
keeping only the final take. Earlier takes are abandoned attempts; the last is
almost always the intended one.

Pairing first→second (the old behaviour) left every intermediate attempt in
place — on a phrase said three times it removed one and left two.

Two guards, both added after this rule over-reached:

- **Longest match wins.** `"it will show you"` (4 words) also prefixes
  `"it will show you how to use it"`, a *different* sentence. Matching the short
  prefix chained four occurrences and deleted the real payload between them. A
  longer match now claims its span so shorter overlapping ones cannot re-cut it.
- **Consecutive takes must be within 3.0 s.** A restart follows quickly; if two
  occurrences are far apart, the material between them is content, not a stumble.

### 3. SNAP 0.12 → 0.35, now that it is safe

The cut boundary for that repeat had no quiet point within 0.12 s — the nearest
was 0.35 s away, so the cut was silently dropped.

Widening SNAP is what **clipped words** when tried earlier. It is safe now only
because the quiet-run guard exists to catch that. Measured at the boundary:

```
SNAP 0.12 : start 61.01 @ -51.1 dB (quiet run 0.06s)  fail
SNAP 0.25 : start 60.93 @ -51.6 dB (quiet run 0.07s)  fail
SNAP 0.35 : start 60.75 @ -58.5 dB (quiet run 0.16s)  PASS
```

**Do not widen SNAP without the quiet-run guard in place.**

Also added: **fallback splice alignments**. If "cut first→last" has no clean
splice, try end-to-end, then drop-first, then drop-second, and take the first
that passes. Removing N−1 copies by a different alignment beats removing none.

---

## Full step list

### 1. Probe
`ffprobe` for duration, fps, resolution, audio rate.

### 2. Look at it, get the content region
Contact sheet, then measure `crop=W:H:X:Y`. Exclude browser chrome, taskbars,
webcam overlays, anything with a ticking clock. Supplied by hand.

### 3. Check the volume and normalise the ANALYSIS copy

```
ffmpeg -i src.mp4 -af "loudnorm=I=-23:print_format=json" -f null -   # measure
gain = -23 - input_i
ffmpeg -i src.mp4 -vn -ac 1 -ar 16000 -af "volume={gain}dB" -c:a pcm_s16le a16.wav
```

**Every dB threshold used to be absolute, tuned on one recording.** Measured
across four videos the source loudness spans **12.2 dB**, so `-52 dB` meant
21.8 dB below programme on one and 34.0 dB below on another — the same rule
behaving completely differently depending on mic gain.

```
brainclean  -30.22 LUFS      extension  -24.04 LUFS
video_2     -18.36 LUFS      video      -18.02 LUFS
```

Normalising the analysis copy to −23 LUFS and expressing thresholds **relative
to programme loudness** fixes it:

```
QUIET (splice)  -21.78 dB relative  ->  -44.78 dB after normalising
GATE  (edge)    -15.78 dB relative  ->  -38.78 dB after normalising
```

A pure gain shifts every RMS-dB value by exactly that gain, so cached energy
features can be reused by adding the gain rather than recomputing.

**The delivered audio is never normalised here** — only the measurement copy.
Mastering still happens at the end, on the finished programme.

### 4. Compute features
Energy per 10 ms frame. Feeds the edge guard, the splice test and the quiet-run
guard.

### 5. Word alignment — chunked
Transcribe in independent 30 s chunks with 3 s overlap — see change 1. Model
size `base` is sufficient; `small` is ~3x slower for no measured gain here.
Needed only for disfluency removal. Verify the output has real gaps.

### 6. Speech detection — **MONITORED**
Silero VAD, then fixed post-processing:
```
bridge gaps  < 0.35 s -> speech
drop bursts  < 0.25 s -> not speech
pad          0.50 s before, 0.55 s after
```
| | |
| --- | --- |
| Adjustable | `threshold` |
| Score | word coverage − false-alarm rate outside words |

### 7. Freeze detection — not monitored
```
ffmpeg -i src.mp4 -vf "crop=W:H:X:Y,fps=5,freezedetect=n=-58dB:d=2.0" -an -f null -
```

### 8. Threshold calibration — deterministic
Sweep `0.50 … 0.15`, build the **plan** for each, count sped-up sections. The
count is U-shaped.
```
spread >= 2                            -> calibrated, take the minimum
spread == 1 and minimum not at an edge -> weak, provisional
otherwise                              -> NO SIGNAL, fall back to 0.30 and say so
```
Measured on three videos: spread 6 / 1 / 0. **It only worked on one.** The
no-signal branch is not optional.

### 9. Dead-air plan — deterministic
```
dead = (not speech) AND (nothing happening)
bridge dead runs < 2.0 s apart, then re-apply "AND not speech"
runs >= 1.0 s:
    < 4.0 s -> collapse to 0.50 s
   >= 4.0 s -> speed up: target = clamp(d/12, 1.2s, 6.0s), speed = min(20, d/target)
moving-screen silence >= 1.5 s -> collapse to 0.80 s
edge guard: trim run boundaries back from any speech-level energy (GATE)
```

### 10. Disfluency removal — **MONITORED**

Detect fillers, stutters, repeats and false starts, then gate every candidate:

```
1. duration within [0.15 s, 9.5 s]
2. both splice points quieter than QUIET
3. QUIET-RUN GUARD: the contiguous quiet stretch containing each splice
   must be >= 0.10 s
4. no overlap with an already-accepted cut
```

**Step 3 is load-bearing.** The old test only asked "is this point quiet?",
which a stop consonant satisfies mid-word. Measuring the *length* of the quiet
stretch separates a real gap from a consonant.

It must be measured from the **audio**, not from word spans: Whisper stretches
tokens across silence, and a word-span version of this guard rejected a splice
sitting in 1.58 s of pure silence.

| | |
| --- | --- |
| Adjustable | `SNAP` (0.35), `QUIET` (−44.78 normalised) |
| Score | total seconds removed — honest only because the guard makes unsafe cuts unavailable |

### 11. Merge
Subtract accepted cuts from 1× segments only. Emit `plan.json` = segments +
source↔output time map.

### 12. Render
Per-segment encode, concat with `-c copy`. Picture encoded exactly once. Sped
segments get `setpts=PTS/speed` and muted `atempo`.

### 13. Audio master
Measure first, then decide. If `-14 − input_i` would push `input_tp` above
−1.5 dBTP, compression is required.
```
highpass=f=80 → afftdn=nr=12 → acompressor → loudnorm=I=-14:TP=-1.5:LRA=11
```

### 14. Verify — must be able to fail the export

| Check | v6 result |
| --- | --- |
| Splice clicks | **0 / 35** (worst 0.0035 vs p99.9 0.0778) |
| Word-clip guard | **0 / 35** |
| A/V sync | SSIM 0.980–0.993 at 6 of 7 checkpoints |
| Loudness | −13.9 LUFS, LRA 6.0 |
| Faststart | `moov` before `mdat` |

**Investigate the worst checkpoint, not the average.** One read 0.928; chasing
it confirmed the known drift below rather than a new fault.

---

## The monitor loop

Applied to steps 6 and 10.

```
1. run the step
2. emit a diagnostic (numbers only, < 200 tokens)
3. model reviews -> approve, or name ONE parameter + direction
4. re-run the step with that parameter nudged (x1.6 or /1.6)
5. score both deterministically
6. keep the better one, log either way
```

**The model chooses what to try. The measurement chooses what to keep.** If
Ollama is unreachable the review returns `ok=true` and defaults are used — the
pipeline never depends on the model being available.

Model: `qwen2.5:7b`, ~3.6 s per review, JSON-schema constrained. `deepseek-r1:7b`
was benchmarked and is no better on the tasks that matter, at 20–40× the latency.

**Only 2 of 14 steps are monitored, and the blocker is always the same: an
automatic quality score.** Any step you can score, you can monitor. Any step you
cannot score, no model can help with.

---

## Known limits

- **Output runs ~0.3 s long.** Sped segments round to whole frames; measured
  +0.32 s on v6. It accumulates: an early checkpoint matched exactly, one at
  600 s was 0.20 s out. Audio and video stay locked to each other, so the video
  is fine — **captions drift progressively late.** Fix: rescale the time map to
  the measured output duration before generating captions.
- **The disfluency score rewards removing more.** Safe only while the quiet-run
  guard makes damaging cuts unavailable. Weaken the guard and the score starts
  rewarding damage.
- **Nothing measures whether it *sounds* good.** 35 cuts inside speech in a
  14-minute video is a lot; no check here would notice if that reads as
  over-edited.
- **Thresholds are still derived from one recording**, now expressed relatively
  rather than absolutely. That should transfer better, but it is validated on
  one video plus a sanity run on a second.

---

## Suggested module split

| module | owns |
| --- | --- |
| `probe` | ffprobe, duration/fps/resolution/audio rate |
| `audio` | normalise the analysis copy, decode to mono 16 kHz f32 |
| `features` | energy per 10 ms frame |
| `align` | chunked Whisper, word list with timings |
| `vad` | Silero + post-processing, speech mask |
| `freeze` | freezedetect parsing |
| `plan` | dead-air runs, cut-vs-speed decision, source<->output time map |
| `disfluency` | candidates + the four gates |
| `render` | per-segment encode, concat |
| `master` | measure, then loudnorm |
| `verify` | the five checks, any of which can fail the export |
| `monitor` | the propose/re-run/score loop, steps 6 and 10 |

`plan` is the centre. Everything upstream produces evidence for it; everything
downstream consumes its segment list and time map.
