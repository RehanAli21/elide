# Project handoff — a verified, AI-assisted video editor

> **`MONITORED_FLOW.md` describes the CURRENT flow (v6) and wins on any
> disagreement about steps or parameters.** This document remains correct on
> architecture, rendering, mastering and verification.


**A genre-extensible editing engine. Screen recordings are the first implemented
genre, not the intended limit.**

**Complete briefing. Self-contained. Every number here was measured, not estimated.**

The engine is built on a **two-signal decision model**: signal A is "is the
speaker talking" (universal), signal B is "is anything happening" (per-genre,
pluggable). Everything downstream -- the plan, rendering, audio mastering,
captions, chapters, the revision loop and the verification suite -- is
genre-independent and works unchanged for any video type.

Adding a genre means writing **one detector** that returns a boolean mask on the
plan's time grid. Nothing else changes.

> **Build order lives in `BUILD_STEPS.md`, and that file wins over this one.**
> This document describes the finished design. It is *not* the order to build it
> in. Do not introduce a trait or abstraction because this document describes one
> — write plain functions until there are two implementations to generalise over.

---

## 0. Instructions for the assistant reading this

You are guiding **Rehan** to build this software **in Rust**.

- **Do not write code.** Guide, explain, review, and answer questions. He writes
  the implementation. If he asks for code, prefer describing the approach, the
  data structures, the crate to use, and the edge cases — then let him write it.
- **This document is the spec.** Every threshold, formula and measurement here
  came from actually building and testing this pipeline in Python against a real
  20-minute recording. Treat the numbers as ground truth for that recording, and
  as *starting points* for others.
- **The governing philosophy is: measure, then decide.** This project's every
  good decision came from a measurement, and every bad one from an assumption.
  When Rehan proposes something, ask what measurement would confirm it. §13 is a
  list of assumptions that turned out wrong — most of them were mine.
- **He is porting a working Python pipeline, not inventing one.** The design is
  settled. The open work is Rust implementation, generalization to more video
  types, and the parts marked UNBUILT.
- **Do not let the reference footage narrow the architecture.** Every measured
  number here comes from one screen recording, because that is what was available
  to test against. The *design* is deliberately genre-extensible and Rehan
  intends to use it on as many video types as possible. When advising on
  structure, favour the abstraction (`SpeechSignal`, `DeadTimeSignal`,
  `Plan`, `LlmProvider`) over the screen-recording specifics. If you find
  yourself suggesting something that only makes sense for an app demo, flag it
  as such.
- **Read the reasoning, not just the parameters.** Almost every threshold here
  exists because a simpler alternative was tried and measurably failed. Section
  1b is the chronology of those failures, section 13 is the list of them, and
  each pipeline stage carries a **"what breaks without this"** note. If Rehan
  proposes dropping a step, that note is the answer to "what happens if we skip
  it" -- and if it does not justify the step for *his* footage, the step may
  genuinely be droppable. The numbers are evidence, not scripture.

---

## 1. What the software does

Takes a raw, unedited recording of someone talking and produces a publish-ready
edit: removes dead air, compresses long waits, removes speech disfluencies,
masters the audio, generates captions and chapters, and **proves it didn't break
anything**.

### What is genre-independent (most of it)

| Capability | Applies to |
| --- | --- |
| Speech detection + mask post-processing | any video with speech |
| The plan and source/output time map | any video |
| Disfluency removal (fillers, stutters, repeats, false starts) | any **spoken-word** video -- works today |
| Rendering (segment encode + concat) | any video |
| Audio mastering | any video |
| Captions and chapters | any spoken video |
| The revision loop | any video |
| **The verification suite** | any video |
| The AI layer (gated selector, probe, fallbacks) | not even video-specific |

### What is per-genre (one thing)

**Signal B -- the evidence that nothing is happening.** Silence alone cannot tell
you whether a quiet stretch is dead air or the speaker letting something play.
Each genre needs its own detector:

| Genre | Signal B | Status |
| --- | --- | --- |
| Screen recording / app demo | frozen picture | **implemented** |
| Lecture with slides | slide-change detection | ~a day; it is freeze detection inverted |
| Interview / podcast | speaker diarization | off-the-shelf models exist |
| Talking head to camera | *none exists* -- return an **all-true** mask | runs today, audio-only, less differentiated |
| Gameplay | loading-screen / HUD state | bespoke, hard |
| Scripted / narrative / musical | *none -- and the premise fails* | should refuse; see section 15 |

**Note the all-true case.** The decision is `dead = (not speaking) AND (nothing
happening)`. A genre with no signal B must return a mask that is **always true**,
which collapses the rule to silence-only -- the ordinary audio-based behaviour.
Returning all-*false* would mean nothing is ever dead and the tool would silently
do nothing. Easy to get backwards; the sign matters.

**This is the single most important structural fact in the document** — but it is
a fact about the *shape of the data*, not about when to write a trait.

What is actually expensive to change later is only this:

> **Rule A.** One fixed time grid (20 ms). Every mask is a `Vec<bool>` of the
> same length, indexed identically.
> **Rule B.** `plan.json` is the sole contract for everything downstream —
> rendering, captions, chapters, revision. Nothing downstream re-reads audio or
> masks.

Obey those and a plain `fn frozen_mask(...) -> Vec<bool>` becomes a trait in ten
minutes, whenever a second genre actually exists. **Write the plain function
first.** An earlier draft of this document said to define the trait on the first
commit; that was wrong, and it is corrected in `BUILD_STEPS.md`.

### Origin

Built by editing one real video three times, each version fixing what the last
got wrong:

| Version | Result | Approach |
| --- | --- | --- |
| v1 | 19:30 → **14:52** (23.8% cut) | energy median + ASR transcript as a speech guard |
| v2 | 19:30 → **15:38** (19.9% cut) | audio-only detection, transcript dropped entirely |
| v3 | 19:30 → **15:10** (22.2% cut) | v2 + disfluency removal (cuts inside speech) |

v2 is *longer* than v1 and that is correct: v1 sped up a passage where the
speaker was genuinely making voiced sound, because the transcript claimed
otherwise. v2 keeps it. **Longer but right beats shorter but wrong.**

---

## 1b. How this design was arrived at (and why that matters)

Nothing here was designed up front. Each version exists because a specific claim
was tested and failed. The chronology tells you which parts are load-bearing and
which are historical.

**Start.** The brief was one sentence: *"edit this video and make it ready for
YouTube."* Three scoping questions were asked -- reframing, trim aggressiveness,
captions. Rehan chose: keep the original framing, trim tightly, produce an SRT.
The framing choice matters downstream: the app occupies 15% of the frame and was
left that way **deliberately**, so do not "fix" that without asking.

**v1 -- silence detection plus the transcript as a speech guard.** Delivered
14:52. An ASR transcript shipped with the video and was used to protect speech
from being cut.

**v1 to v2 was triggered by Rehan**, who said: *use the audio data directly
rather than the transcription.* He was right, and testing showed why. The
transcript claimed a full spoken sentence -- *"It is blocked. And the other
application works, once again..."* -- across a stretch measuring **-70.2 dB at
42% periodicity**, acoustically identical to confirmed dead air. It also emitted
`" Okay."` timed 722.00 to 750.00 over pure silence. **v1 had protected 6 seconds
of nothing because the transcript lied.** v2 dropped the transcript from the cut
decision entirely and became measurably more defensible -- while being 46 s
*longer*. Longer but right beats shorter but wrong.

**v2 to v3 was triggered by Rehan** asking why false starts, stutters and
repeated sentences were not removed. The honest answer: it had never been
offered. The first scoping question framed "tight" as silence-only and was never
revisited. Building it required re-aligning the audio, because the supplied word
timings were contiguous -- every word's `end` equalled the next word's `start`,
**zero** recorded pauses -- and therefore could not locate a 0.3 s filler.

**v3 was then corrected by Rehan again**, who noticed a sentence beginning
*"I have added"* was still repeated. It *had* been detected, then discarded by
two of my own guards (`MAX_TAKE_GAP` 0.6 s, `MAX_CUT` 3.2 s). Both exist for good
reasons -- they stop the tool deleting real content sitting between two matching
phrases. But here that content *was* the false start. Hence the separate
extended-false-start detector with its stall-evidence requirement (section 8).

**The meta-lesson, and the reason this section exists:** three times, a
confidently-delivered result was wrong in a way only the person who made the
video could see. The verification suite catches *mechanical* breakage -- lost
speech, drift, clicks -- but it cannot catch *"you did not remove the thing I
wanted removed."* **Design for review, not for autonomy.** That is why the
revision loop (section 16) is not a nice-to-have.

### Human context

- **Rehan** built **BrainClean**, an Android app that blocks distracting apps via
  timed tasks, schedules and wellness reminders. The video is a product demo plus
  a roadmap segment, aimed at YouTube.
- **He is not a native English speaker.** This is the direct cause of the ASR
  problems throughout: the transcript rendered "duration" as *"division"*,
  "reading task" as *"leading task"*, the app name as *"PowerPoint"*, and the
  20-20-20 rule as *"2020, 2022"*. Any caption feature must treat accented speech
  and unreliable ASR as the normal case, not the exception.
- The recording is unscripted and unrehearsed, with genuine hesitation. That is
  what makes disfluency removal worthwhile, and also what makes it risky.

---

## 2. The source recording (the one everything was measured against)

```
container   MP4, 196,626,505 bytes, 1,344 kbps
duration    1170.100 s  (19 min 30 s)
video       H.264 High, 1920x1080, 60 fps, yuv420p, 1,140,539 bps
audio       AAC-LC, 48 kHz, stereo, 192,042 bps
recorder    OBS
content     Android emulator running an app, on a Windows desktop,
            with a circular webcam overlay
```

**Critical layout fact:** the phone screen occupied only **374 × 837 px of a
1920 × 1080 frame — about 15%**. The rest was Windows wallpaper, taskbar and a
webcam bubble. Content region: `crop=374:820:773:113`. The emulator was on
screen until **1109.0 s**, then a browser.

This matters because freeze detection (section 6) must run on the content region
only. On the full frame it never fires -- the webcam always moves and the OS
clock always ticks.

**Why the "look at the video first" step exists:** this was discovered only by
generating a contact sheet before touching anything. Every later decision -- the
crop, whether a cut is safe, what the video even *is* -- depends on it.

**What breaks without it:** freeze detection silently returns almost nothing, the
decision engine finds no dead time, and the tool reports "nothing to cut" on a
video that is 22% removable. It fails *quietly*, which is the worst kind.

---

## 3. Pipeline overview

```
source.mp4
    │
    ├─► probe (duration, fps, resolution, audio rate)
    ├─► contact sheet + content-region crop        [user-drawn in v1 of the tool]
    │
    ├─► SIGNAL A: extract mono 16 kHz PCM ──► VAD ──► speech mask
    │            (universal -- every genre)
    │
    ├─► SIGNAL B: DeadTimeSignal trait   ──────────► "nothing happening" mask
    │            screen recording -> freezedetect on content crop
    │            slide lecture     -> slide-change detection      [UNBUILT]
    │            interview         -> diarization                 [UNBUILT]
    │            talking head      -> none; mask is all-TRUE (see note)
    │
    ├─► DECISION ENGINE  (A × B) ─────────────────► silence/wait plan
    │
    ├─► Whisper DTW word alignment ──► disfluency detection ──► speech cuts
    │
    ├─► MERGED PLAN  =  [(src_start, src_end, speed)] + source↔output time map
    │
    ├─► render (per-segment encode → concat) ──► audio master ──► final.mp4
    ├─► captions (re-timed through the map)
    ├─► chapters (re-timed through the map)
    │
    └─► VERIFICATION SUITE  (pass/fail, blocks export)
```

**The plan is the central abstraction.** A list of segments plus a bidirectional
time map. Video, audio, captions, chapters and the revision loop all derive from
it. In Rust this should be a `serde`-derived struct — making it a typed,
compile-time-checked artifact is a genuine improvement over the Python.

---

## 4. Audio extraction

```
ffmpeg -i src.mp4 -vn -ac 1 -ar 16000 -c:a pcm_s16le a16.wav
```

**This file is a measurement instrument only. It is never the delivered audio** —
that always comes from the original stream inside `src.mp4`.

Why these settings:
- **mono** — one mic, one speaker
- **16 kHz** — all speech energy and pitch is below 8 kHz, so Nyquist says 16 kHz
  loses nothing. It is also what Whisper resamples to internally
- **PCM** — exact sample values, no decode artefacts
- ~12× smaller than 48 kHz stereo, and every sample gets touched

Converting to float: 16-bit signed PCM is -32768..32767, so divide by 32768.0 for
+/-1.0 range. In Rust use the `hound` crate to read WAV.

**What breaks without the separation:** if the analysis file is ever used as the
delivered audio, you ship 16 kHz mono to YouTube. The rule -- *measure on
`a16.wav`, render from `src.mp4`* -- must hold everywhere, including in the
revision loop.

---

## 5. Speech detection

### What to build: Silero VAD

**Use pretrained Silero VAD, not a hand-tuned classifier.** This was measured:

| | raw Silero | Silero + post-processing | hand-tuned 5-feature LDA |
| --- | --- | --- | --- |
| Labelled regions correct | 10/11 | **11/11** | 11/11 |
| All 5 dead regions | 0.0% | 0.0% | 0.0% |
| Frame agreement with LDA | 78.4% | **95.8%** | — |
| Labelling required | none | none | 11 hand-marked spans |
| Speed | 14 s for 1170 s audio (83× realtime, CPU) | | |

Silero is MIT, ~2 MB, ships as ONNX (the files are inside the pip package -- no
download). In Rust: the `ort` crate (ONNX Runtime).

### Calibrate the threshold per video -- do not hard-code it

**The 95.8% figure above is mask-level agreement, and it was a misleading way to
validate the swap.** Running the *same planner* on both masks produces materially
different edits:

```
                output   removed  sped-up sections
LDA (labelled)  15.63m    232.5s        6
Silero @0.50    14.98m    271.5s        7      <- 39s shorter
Silero @0.25    15.07m    266.0s        6
```

A ~4% mask difference became a **39-second** plan difference, because whether a
12-second span counts as speech decides whether a 51-second block merges into one
big speed-up. **Small differences in a mask are amplified by the planner.**

Two spans the hand-labelled detector preserved and Silero does not:

| Span | Measured | LDA | Silero @0.5 | Silero @0.25 |
| --- | --- | --- | --- | --- |
| `"And now,"` 364.9-366.4 | 56% maleF0, −50.9 dB | kept | **sped 3×** | **kept** |
| quiet voicing 487-499 | 52% maleF0, −61.9 dB | kept | **sped 12×** | **still sped 12×** |

Threshold 0.25 recovers the first with **zero** dead-air leakage. But 0.25 is not
a universal answer either -- it was tuned on this video, which is the same n=1
mistake in miniature. **Calibrate per video by plan-level stability** (see
`BUILD_STEPS.md` Milestone 4b): sweep the threshold, build the plan for each,
and take the highest threshold inside the widest plateau where the sped-up
section list stops changing. Fully automatic, no labels, ~90 s.

Scoring the *mask* rather than the plan was tried and does not work: the
separation curve was flat to within 1% across the whole sweep, and on two of
three test videos it selected the edge of the range.

The second is **not recoverable**. Sweeping the threshold to 0.20 does not find
it, and a pitch-based veto was tested and rejected -- at any gate catching that
span meaningfully, dead-air leakage reached 17%. That content sits genuinely
between the speech and dead-air references, and Silero cannot hear it.

**The trade, stated plainly:** you lose the labelling requirement, which is what
makes the tool shippable. You accept that ~12 s of quiet muttering during a timer
wait gets sped up. On this video that is defensible -- v1 sped it up and the
result was fine. But it *is* a behavioural difference, not a free swap, and the
strict rule "never speed up anything that might be speech" no longer holds
absolutely.

### The post-processing is not optional

Silero is a **word-level** VAD — it excludes gaps *between words*. Used raw it
fragments continuous speech into hundreds of islands. These three steps turn it
into an editing mask:

```
bridge gaps  < 0.35 s   ->  speech      (gaps inside a sentence)
drop bursts  < 0.25 s   ->  not speech  (stray noise)
pad          0.50 s before, 0.55 s after
```

Padding is **asymmetric on purpose**: an utterance decays more slowly than it
starts, so tails need more protection than onsets.

**What breaks without the post-processing:** raw Silero scored 10/11 on the
labelled regions but only **78.4%** frame agreement with a known-good mask,
because it splits continuous speech at every inter-word gap. The decision engine
then sees hundreds of tiny "silences" and either cuts inside sentences or, with
`MIN_DEAD` protecting them, does nothing at all. Post-processing raised agreement
to **95.8%**. The VAD is off-the-shelf; **this glue is the part you own.**

### Historical note (do not rebuild this)

v2 used a Fisher LDA over 5 hand-computed features. It worked but required 11
hand-labelled regions per recording. Recorded here only because the measurements
are informative:

Six features were computed at 32 ms window / 10 ms hop: `energy_db`, `voicing`
(autocorrelation 70–400 Hz), `malef0` (autocorrelation 80–260 Hz), `flatness`,
`bandratio` (300–3400 Hz), `centroid`.

**Only five fed the classifier** — `centroid` was measured, found uninformative,
and left out. And the window-sweep table below says *4-feature* because it was
run before `malef0` was added. Three different counts appear in this section and
all three are correct at their own moment: **6 computed, 5 used, 4 at the time of
the sweep.**

Median values that separate the classes:

```
region                 energy  voicing   maleF0    flat    band   centr
SPEECH   104-124        -48.9    0.531    0.580  0.0003   0.668     466
QUIET-SP  567-577       -53.3    0.565    0.596  0.0002   0.795     474
DEAD      715-760       -70.6    0.416    0.476  0.0036   0.248     299
DEAD     1030-1070      -71.4    0.404    0.460  0.0035   0.309     311
```

**The single most important finding from that work, which still applies:**

> These features do not separate per-frame. At *any* threshold, ~39% of dead-air
> frames get called speech. They only separate once **aggregated over time**.

Measured separation vs smoothing window (balanced accuracy, and
leave-one-region-out cross-validation):

```
  win   energy only   4-feature LDA   LDA cross-validated
  0.7        82.3%           92.1%              90.3%
  1.0        86.1%           94.9%              92.6%
  1.5        90.6%           97.1%              94.9%
  2.0        93.1%           97.9%              96.2%
  3.0        96.4%           99.5%              97.7%
  4.0        99.2%          100.0%             100.0%
```

Two lessons that survive the switch to Silero:

1. **Energy alone is the strongest single feature.** Every spectral feature was
   individually *worse* at every window length. Do not assume clever features
   beat the obvious one.
2. **Combining features buys temporal precision, not accuracy.** The combination
   reached at 2.0 s what energy needed 4.0 s for — halving the smoothing, so cut
   boundaries land twice as precisely.

---

## 6. Freeze detection (the second signal)

```
ffmpeg -i src.mp4 -vf "crop=W:H:X:Y,fps=5,freezedetect=n=-58dB:d=2.0" -an -f null -
```

Parse `freeze_start` / `freeze_end` from stderr.

Three deliberate choices:
- **crop to the content region** — on the full frame it never fires
- **fps=5** — 12× less decoding than 60 fps, and something still for 2 s is still
  at 5 fps too
- **d=2.0** — must be static 2 s to count. `n=-58dB` is the per-pixel threshold

**UNBUILT — auto-detecting the crop.** Currently the user must supply it. The
approach: sample ~200 frames, build a per-pixel temporal variance map, then
classify regions by *burstiness* — near-zero variance is static chrome, constant
moderate variance is a webcam (a face always moves), bursty variance (long still
periods then a big change) is the app. Ship user-drawn rectangle first.

---

## 7. The decision engine

### Core rule

> A stretch is **dead** when the speaker isn't talking **AND** the picture is
> frozen.

Both terms are required, and each protects a case the other destroys:
- talking over a still screen (explaining a settings page) -> speech term
- working silently while the UI animates -> freeze term

**Why not audio alone, which is what every other tool does:** silence cannot
distinguish "dead air" from "letting an animation play". Audio-only tools get
this wrong in *both* directions on screen recordings -- they delete footage that
carries the demonstration, and they keep timer waits where nothing happens. The
second signal is the entire reason this pipeline is worth building instead of
using an off-the-shelf silence remover.

### The decision tree

```
                    is the speaker talking?
                        /            \
                     YES              NO
                      |                |
              KEEP AT 1x        is the picture frozen?
                                   /          \
                                YES            NO
                                 |              |
                              "WAIT"         "PAUSE"
                                 |              |
                        run length?        run length?
                        /    |    \          /      \
                    <1.0s  1-4s  >=4s     <1.5s   >=1.5s
                      |      |     |        |        |
                    KEEP  COLLAPSE SPEED   KEEP   COLLAPSE
                          to 0.50s  UP            to 0.80s
```

### Parameters

```
MIN_DEAD     1.00 s   below this it's speech rhythm, not dead air
COLLAPSE_TO  0.50 s   what a trimmed wait becomes
SPEED_ABOVE  4.00 s   the cut/speed boundary
MAX_SPEED    20x
BRIDGE       2.00 s   two dead runs a beat apart are one dead stretch
PAUSE_MIN    1.50 s   moving-screen silence: higher bar to touch it
PAUSE_TO     0.80 s   and a gentler collapse
plan grid    0.02 s
```

### The 4-second rule — the one real editorial judgement

It turns on **what the dead time is evidence of**:

- **Under 4 s** — dead air is just dead air. **Delete it.** A speed-up here is
  worse than a cut: it draws attention to a moment carrying no information.
- **4 s and over** — the dead time is usually *itself the demonstration*. If the
  claim is "this app blocks YouTube for two minutes", those two minutes **are the
  proof**. Cut them and the demo stops demonstrating. **Speed them up** — the
  clock visibly advances, the blocked state visibly persists, the proof survives,
  and it costs six seconds instead of two minutes.

> **Cut what is empty. Compress what is evidence.**

Secondary reason: a speed-up is self-announcing — viewers read fast-forward as
"time passed". An invisible 80-second cut can read as the app doing nothing.

### The speed curve

```
target = clamp(duration / 12, 1.2 s, 6.0 s)      // on-screen duration
speed  = min(20, duration / target)
```

**The on-screen duration is clamped, not the speed.** Four regimes result:

| Source run | Speed | On screen | Regime |
| --- | --- | --- | --- |
| 4.0 s | 3.3× | 1.20 s | output floored at 1.2 s |
| 10.0 s | 8.3× | 1.20 s | ” |
| 14.4 s | 12.0× | 1.20 s | ” |
| 30.0 s | 12.0× | 2.50 s | natural 12× band |
| 72.0 s | 12.0× | 6.00 s | ” |
| 78.0 s | 13.0× | 6.00 s | output capped at 6.0 s |
| 120.0 s | 20.0× | 6.00 s | ” |
| 200.0 s | 20.0× | 10.00 s | speed capped at 20× |

Rationale for each bound:
- **1.2 s floor** — shorter reads as a glitch, not a fast-forward
- **12× band** — fast enough to compress, slow enough that a countdown stays trackable
- **6 s ceiling** — nobody wants more than ~6 s of fast-forward
- **20× cap** — beyond this motion is unreadable, so very long waits get *more
  screen time* rather than more speed

### Result on the real video

```
     source span      len   speed    out
  457.6-   465.0     7.4s    6.2x   1.2s
  481.6-   485.6     4.0s    3.3x   1.2s
  635.6-   657.5    21.9s   12.0x   1.8s
  708.4-   786.1    77.8s   13.0x   6.0s
  791.6-   856.2    64.6s   12.0x   5.4s
 1020.4-  1075.6    55.2s   12.0x   4.6s

230.9 s of source -> 20.2 s on screen (211 s saved, 11.4x average)
```

**Six speed-ups did 81% of all time saved in the entire edit** (210.7 s of the
260.1 s removed). Disfluency cuts accounted for 27.5 s and every collapsed pause
in the whole video for only 21.8 s. **The long waits are where the runtime is.**

### The edge guard

The classifier's weak spot is the *boundary* of a dead run — a quiet word
starting just before speech is confirmed gets swallowed.

A plain energy gate applied everywhere is far worse: measured, it fires on **21%
of dead air**, which is full of mouse clicks. Applied **only to run edges** it is
safe, because interior clicks can no longer trigger it.

**Implementation warning:** do *not* walk inward frame by frame. It stops at the
first dip, and speech is full of inter-syllable dips. Instead find the **last**
loud frame near the head and the **first** near the tail, and pull the boundaries
inside them.

Gate chosen by sweep: `-46 dB`, probe window 0.20 s, max shrink 3.00 s.

```
GATE   output    words affected  real clusters  real-in-speedup  sped sections
-46 dB 15.63 min       26              1              0               6   <- chosen
-52 dB 15.86 min       23              1              0               5
-55 dB 16.03 min       23              1              0               5
```

**These runtimes are from the silence-only plan**, before disfluency removal —
which is why they read longer than the final 15:10. The column that decided it is
`real-in-speedup`: at every gate no genuine speech ended up inside a speed-up, so
the tie was broken on runtime and on keeping the most speed-ups.

---

## 8. Disfluency removal (cutting inside speech)

**This inverts the pipeline's core invariant.** Everything else cuts inside
silence, where being 100 ms off is unnoticeable. This cuts inside speech, where
100 ms clips a consonant and every junction risks a click. Gate it behind a flag.

### Word alignment is a hard prerequisite

Run Whisper with **word timestamps** (DTW over cross-attention). In Rust:
`whisper-rs` (bindings to whisper.cpp). The `base` model was adequate;
`condition_on_previous_text=false` matters — it stops the model looping on
repeated phrases, which is exactly what you are trying to detect.

**Why you cannot use a pre-existing transcript.** The one supplied with this
video had *contiguous* word timings — every word's `end` equalled the next word's
`start`. 89 filler tokens, **zero** recorded gaps. It carries no pause
information and cannot locate a 0.3 s word. A first attempt on that data produced
**80 "cuttable" fillers that were all illusory** — the search window around a
word's start and its end kept collapsing onto the same energy minimum, giving
spans of 0.00 s.

Proper alignment gave **1809 words, 211 with a real gap.**

### The governing principle

> **WHAT to cut comes from the word sequence. WHERE to cut comes from the
> waveform.** Never the other way round.

Every candidate must clear **three independent tests**:

| Test | Rule |
| --- | --- |
| Alignment | words repeat, or a filler sits between real pauses |
| Acoustic | for repeats, the two takes must *sound* alike (spectral correlation ≥ 0.55) |
| Splice | both boundaries snap to a point quieter than −52 dB |

**Anything failing is dropped, not forced.** A cut that clips a consonant is
worse than no cut.

**Why three independent tests instead of one good one:** each catches what the
others cannot see. Alignment alone proposed cutting 2.86 s because `"all the"`
appeared twice. The acoustic test alone rejected two `"it is"` repeats at
similarity 0.40 and 0.25 -- matching text, different audio. The splice test knows
nothing about words at all. Together, 73 proposals became 28 safe ones, and
**24 rejections were purely because there was no quiet point to cut at** -- a
constraint neither of the other two can detect.

### Four detectors

**1. Stutters** — identical adjacent words (`"Thursday Thursday Thursday"`). Keep
the last.

**2. Immediate repeats** — a phrase said twice back to back.
- **3+ words minimum.** 2-word matches (`"all the"`, `"it is"`) recur constantly
  in ordinary speech. Without this the first pass proposed cutting **2.86 s**
  because `"all the"` appeared twice, deleting the different words in between.
- Second take must begin within **0.6 s** of the first ending, or the span
  between them holds real content.

**3. Extended false starts** — speaker gets several words in, stalls, trails off,
pauses, restarts:

```
"...the first thing you will see,"
   I have added some ... [1.0s stall] ... languages to some. So--   <- abandoned
   [1.4s pause]
   I have added some images so that it is more or less...           <- retake
```

Detector 2 cannot see this (7 s gap, not 0.6 s). Allowing a long gap is only safe
with **positive evidence of a stall**:
- 4+ word phrase restarted within 9 s
- a **real pause** (≥0.4 s below −60 dB) between the takes — people hesitate
  before abandoning a sentence; continuous speech means the middle is content
- cut ends at the **latest** point still as quiet as the quietest one, so the
  abandoned take and the hesitation both go

Two counter-intuitive details, both learned the hard way:
- **Relax similarity for this class, don't tighten it.** A false start is
  hesitant by nature and will *not* match the clean retake — this pair scored
  **0.45**. The 0.55 used for immediate repeats rejects exactly what you want.
  Compensate with the longer required phrase.
- **Exclude enumerators.** A repeated phrase introduced by
  *second/third/another/next* is a **list**, not a retake. Without this guard the
  rule proposed deleting a listed option.

**4. Fillers** — sentence-initial *so/okay/now/also/then/and*, only where
detachable: ≥0.18 s of space before, ≤0.85 s long, remove at most 0.6 s of the
trailing pause.

### Parameters

**These are the v3 values. v6 changed four of them — see `MONITORED_FLOW.md`:**
`SNAP` 0.12 → **0.35**; `QUIET` −52 dB absolute → **−21.78 dB relative to
programme**; repeats now cut first→**last**, not first→second; and a
**quiet-run guard** (≥0.10s of contiguous quiet at each splice) was added,
which is what makes the wider SNAP safe.

```
SNAP            0.12 s    search window for the splice point   [v6: 0.35]
QUIET          -52 dB     required quietness at a splice       [v6: -21.78 relative]
MIN_CUT         0.22 s
MAX_CUT         3.20 s
KEEP_GAP        0.14 s
SIM_MIN         0.55      immediate repeats
MIN_PHRASE      3 words   immediate repeats
MAX_TAKE_GAP    0.60 s    immediate repeats
FS_MIN_PHRASE   4 words   false starts
FS_MAX_GAP      9.0 s     false starts
FS_PAUSE        0.40 s    false starts
FS_MAX_CUT      9.5 s     false starts
FS_SIM_MIN      0.35      false starts
```

### Expect most candidates to be rejected

Of 73 proposals, 45 were rejected: **25 because no quiet splice point existed**
and 20 on duration. The splice rejections are mostly fillers -- the speaker runs
them straight into the next word, so the boundary sits at −34 to −51 dB,
mid-voice, and cutting there clips the following word's onset.

**Roughly a third of everything proposed dies at the splice test.** That is the
system working: it is the only test that knows whether a cut can be made
*cleanly*, independent of whether it *should* be made. Accept the smaller clean
edit rather than forcing the rest.

Final: 73 proposed → **28 accepted** (23 filler, 2 repeat, 2 stutter, 1 false
start), 27.5 s removed.

---

## 9. Rendering

```
per segment:
ffmpeg -ss <start> -t <dur> -i src.mp4 \
  -c:v libx264 -preset fast -crf 19 -profile:v high -pix_fmt yuv420p -g 120 \
  -c:a pcm_s16le -ar 48000 -ac 2  seg/NNN.mkv

sped-up segments add:
  -vf "setpts=PTS/<speed>" -af "atempo=2,atempo=2,atempo=<rest>,volume=0"

then:
ffmpeg -f concat -safe 0 -i concat.txt -c copy joined.mkv
```

Five decisions worth preserving:

- **Segment-by-segment, not one giant filter_complex.** 50 `trim` branches off a
  single input makes ffmpeg split the decoded stream 50 ways and buffer frames
  for branches consumed much later. Memory balloons. Per-segment keeps it flat
  and isolates failures.
- **`-ss` before `-i`.** Modern ffmpeg input seeking is frame-accurate (decodes
  from the preceding keyframe and discards) — fast *and* exact.
- **The picture is encoded exactly once.** Segments encode; concat is `-c copy`;
  the audio pass is `-c:v copy`. One lossy generation.
- **PCM audio in intermediates** (hence MKV) so audio isn't generation-lossed
  before its single AAC encode.
- **`atempo` chained in factors ≤2**, and sped sections **muted** — 12×
  time-stretched room tone is noise, and a clean drop reads as intentional.

**Rust opportunity:** this stage is embarrassingly parallel and the Python did it
serially (~35 min for 50 segments). Spawning N ffmpeg processes with `rayon` or a
bounded task pool should cut that several-fold on a multicore machine. This is
probably the single biggest practical win from the port.

---

## 10. Audio mastering

### Measure first, then decide

```
ffmpeg -i joined.mkv -af "highpass=f=80,loudnorm=I=-14:TP=-1.5:LRA=11:print_format=json" \
  -f null - 2> loudness.txt
```

Measured on this recording: `input_i = -29.97 LUFS`, `input_tp = -9.11 dBTP`.

**Do the arithmetic before choosing a chain.** YouTube normalises to ≈ −14 LUFS.
From −29.97 that needs **+15.9 dB** — but true peak is −9.11 dBTP, so flat gain
lands at **+6.8 dBTP**, badly clipped. The most a linear gain can do is +7.6 dB,
leaving the programme at −22.3 LUFS, far too quiet.

**Therefore compression is required, not optional.** Re-check this on every
recording rather than assuming — a louder source may not need it.

### The chain

```
highpass=f=80
afftdn=nr=12:nf=-45
acompressor=threshold=-28dB:ratio=3:attack=10:release=250:makeup=8
loudnorm=I=-14:TP=-1.5:LRA=11
→ AAC-LC 192k 48 kHz stereo, -movflags +faststart
```

| Stage | Why |
| --- | --- |
| highpass 80 Hz | desk rumble below the voice |
| afftdn nr=12 | gentle denoise — needed *because* the chain adds ~16 dB, which would lift the noise floor from −70 to −54 dB. Kept mild to avoid watery artefacts |
| acompressor | closes the peak-to-average gap so the target is reachable |
| loudnorm | final normalisation with true-peak limiting |
| +faststart | index at the front so playback starts before full download |

Achieved: **−13.9 LUFS, LRA 6.9, peak −1.34 dB.**

---

## 11. Captions and chapters

Both derive from the plan's time map, so they stay in sync for free.

**Correct the text against on-screen evidence.** ASR on accented speech is
unreliable — this transcript called the app *"PowerPoint"*, "duration"
*"division"*, "reading task" *"leading task"*, and the 20-20-20 rule *"2020,
2022"*. Anchor every correction to text visible on screen (menu labels, buttons,
page titles).

**Flag, never invent.** Nine caption lines were genuinely unintelligible. They
were marked for proofreading rather than presented as transcription. A
confidently wrong caption is worse than a flagged one.

**Line breaking:** split on sentence boundaries first, then clause boundaries
(`, ; : —`, or before *and/but/so/because/which/that/then/or*), word-wrap only as
a last resort. Apportion time by character count. Guarantee each cue lasts at
least `length / 18` seconds.

Targets: ≤44 chars/line, ≤2 lines, median ≈10 chars/sec, no overlaps.

**Chapters:** first marker at `0:00`, monotonically increasing, ≥10 s apart, ≥3
of them.

---

## 12. The verification suite — this is the moat

**Nobody else ships this.** For an automated editor, "prove you didn't break it"
*is* the trust problem, and it is the reason someone would let a tool touch a
20-minute recording unattended. Build it from day one; it should be able to
**fail the export**.

| Check | Method | Achieved |
| --- | --- | --- |
| Speech preserved | every tightly-timed word vs the segment list | v1: 0 lost of 1815; v3: 26 touched (1.4%), 8/9 clusters confirmed ASR hallucinations |
| No speech sped up | peak level inside each speed-up | all ≤ −45 dB, well below speech |
| **A/V sync** | for N output timestamps, use the time map to predict the source timestamp, extract both frames, compare | **SSIM 0.981–0.993** |
| Splices don't click | max sample-to-sample jump at each splice vs the file's own 99.9th percentile | **0 of 28** exceeded it (worst 0.0234 vs 0.0777) |
| Loudness | ebur128 | −13.9 LUFS |
| Faststart | top-level atom order | `ftyp → moov → … → mdat` |
| Captions on speech | audio level at each cue midpoint | 132/133 |
| Caption sanity | overlaps, line length, reading speed | 0 overlaps, ≤44 ch, median 10 cps |

**The SSIM sync test is the most valuable single check.** It validates the cut
plan, segment rendering, concat and speed-ups in one measurement. If output 750 s
correctly shows source 973.2 s, no drift accumulated across 50 joins.

**Why this suite exists at all:** an automated editor asks the user to let it
modify 20 minutes of their work unattended. The only thing that makes that
acceptable is being able to *prove* nothing broke. Every check here was added
after a real failure or near-failure during development -- the speech
preservation check caught a bridging bug that had swallowed the words *"I have
added"*, and the splice test was written specifically because cutting inside
speech was new and untrusted.

**The splice test must compare against the file's own distribution**, not an
absolute threshold.

---

## 13. Assumptions that turned out wrong

Every one of these actually happened. They are the real content of this project.

1. **A supplied transcript's timings.** Check for gaps first. If every word's
   `end` equals the next `start`, it carries no pause information.
2. **A transcript's *content* over the audio.** Whisper transcribed a full
   sentence across a stretch measuring **−70.2 dB at 42% periodicity** —
   indistinguishable from confirmed dead air. It also emits one short token
   stretched over 30 s (`" Okay."` timed 722.00 → 750.00) when it hears nothing.
   **Where they disagree, the audio wins.**
3. **Per-frame classification.** ~39% false positives at any threshold.
4. **That spectral features beat energy.** They don't. They buy temporal
   precision.
5. **A bridging step that paved over speech.** The first version marked the whole
   span between two dead runs as dead, including speech inside it. Re-apply the
   speech mask *after* bridging.
6. **Forcing a cut to a minimum length.** Stretching short cuts up to `MIN_CUT`
   produced 8 splices of exactly 0.12 s that removed no word — pure risk, zero
   benefit. Reject instead.
7. **Walking inward frame by frame to find a boundary.** Stops at the first dip.
8. **A global energy gate.** Fires on ~21% of dead air. Only safe at run edges.
9. **Cutting on 2-word repeats.**
10. **Requiring high similarity for false starts.** They're hesitant by nature.
11. **Cutting a repeated phrase introduced by an enumerator.** It's a list.
12. **`max()` in the phonetic gate.** Let `division → schedules` through at
    exactly 0.40 — the precise hallucination it existed to stop. Use `min()` of
    direct and consonant-skeleton similarity.
13. **A capability probe that didn't use the production prompt.** Added "if
    unsure choose 0"; the model abstained on 4/6 and the probe scored **0.33** —
    measuring the prompt's bias, not the model.
14. **Estimating before measuring.** Promised "~14:30 and 60+ cuts" for
    disfluency removal, delivered 15:10 and 28, because most fillers had no quiet
    splice point.
15. **Assuming the rendered output matches the plan exactly.** It does not.
    Each sped-up segment is rounded to a whole number of frames, so the finished
    file runs slightly long: measured **+0.32 s (v3)** and **+0.29 s (v4)**, both
    with 6 sped segments -- about 3 frames each at 60 fps. The drift
    **accumulates**: an A/V sync checkpoint at output 150 s matched exactly,
    while one at 620 s was **0.2 s** out. Audio and video stay in sync with each
    other, so the video is fine, but **captions drift progressively late**.
    *Fix:* after rendering, measure the real output duration and rescale the time
    map before generating captions -- or compute segment lengths in exact frame
    counts when building the plan. This was only found by chasing the lowest SSIM
    in the sync test instead of accepting the average.
16. **Assuming a self-tuning metric generalises.** The threshold calibration
    (count of sped-up sections, pick the minimum) was derived from one video and
    then tested on two more: **spread 6 / spread 1 / spread 0.** It was
    informative on one video in three. On the flat one it would have picked an
    arbitrary value with full confidence. Any auto-tuned parameter needs a
    **no-signal detector** and a documented fallback -- a tuner that cannot tell
    it is blind is worse than a constant.
17. **Validating a component swap at the component's own level.** Silero was
    checked against the hand-tuned detector at **mask** level (95.8% frame
    agreement, 11/11 labelled regions) and declared equivalent. Running the real
    planner on both masks showed a **39-second** difference in the finished edit
    and two spans treated differently. **Validate a substitution on the artifact
    you ship, not on the component you replaced.** For this pipeline that means:
    compare `plan.json`, not the mask.

---

## 14. The AI layer

### Core principle

> **The deterministic pipeline decides. The model only proposes, inside a closed
> action space, behind a deterministic gate.**

The pipeline must work fully with `--no-ai`. AI is an **enhancement layer, never
a dependency**. Enforce in CI that the core module does not import the AI module.

### Three laws

**Law 1 — Never let the model decide anything you can measure.** Cut points,
speed factors, timings, loudness, splice positions: all arithmetic. The model
never decides where to speed up.

**Law 2 — Convert generation into selection, one decision per call.**
Deterministic code builds the candidate list; the model returns an index. Always
include `0 = keep as-is` so abstaining is expressible.

**Law 3 — Gate every output deterministically, and ignore self-reported
confidence.**

### Measured on qwen2.5:7b (Ollama, CPU)

| Design | Result |
| --- | --- |
| Free generation ("fix this caption line") | **1/4** — and `confident: true` on every error |
| Selection from candidates | **3/5** — failures were "keep as heard", i.e. safe |
| Selection + on-screen evidence | **4/6** |
| Whole 7-field config in one call | **0/5** — returned `unknown` for everything |
| Single multiple-choice ("which of 6 content types?") | **5/5** |
| Yes/no judgement ("are pauses deliberate here?") | **3/4** — failed on the app demo |
| JSON compliance with native schema | **100%** |
| Latency, warm | **~3.6 s/call**, inputs 56–98 tokens, outputs 6–7 tokens |

**Nothing exceeds ~100 tokens of input.** The model never sees the video, the
transcript, or the plan — so there is nothing to chunk. The decomposition is into
*decisions*, not into segments of video.

Budget for a 20-minute video: ~58 gated calls ≈ **3.5 minutes**, versus ~35
minutes of rendering.

### Capability probe and tiers

Probe each model once and cache: JSON compliance, throughput, reliable context
(measured, not advertised), selection **precision** (when it acts, is it right)
and **coverage** (how often it acts). Tier on *precision* — abstaining is free
because the gate falls back; acting wrongly is what costs.

```
precision >= 0.85 and generation >= 0.75  -> large : all tasks
precision >= 0.75                         -> mid   : select + classify
precision >= 0.60                         -> small : select only, strict gates
below                                     -> none  : deterministic fallbacks
```

qwen2.5:7b measured **precision 0.50 / coverage 0.67 / generation 0.00**. With a
6-item probe that is one item from the "small" threshold — **build the probe to
30+ validated items before trusting tier assignment.**

### The tasks

| Task | Kind | Input | Gate | Fallback |
| --- | --- | --- | --- | --- |
| `intent.classify` | select | user's sentence + 6 types | index in range | ask user |
| `caption.fix` | select | one line + candidates | keep-as-heard always OK; else single word AND phonetic ≥0.40 | keep ASR text |
| `disfluency.adjudicate` | select | ~40 words around a repeat | "unsure" ⇒ don't cut | enumerator heuristic |
| `caption.intelligible` | classify | one line | none — advisory only | flag nothing |
| `chapters.title` | generate | 2 sentences per section | length, non-empty | "Section N" |

**Everything downstream of content type is a lookup table, not a second
question.** Asking the model whether pauses are deliberate scored 3/4 and failed
on the app demo — it said they were deliberate, exactly backwards.

| content_type | dead air a defect | speed up waits | second signal |
| --- | --- | --- | --- |
| app_demo | yes | yes | freeze detection |
| lecture_slides | yes | yes | slide-change detection |
| interview | yes | no | diarization |
| talking_head | yes | no | *none* — audio only |
| gameplay | yes | loading screens only | bespoke |
| event_footage | **no** | no | *none — refuse* |

### Triage, not chunking

At ~3.6 s/call, running all 133 caption lines is wasteful. Flag lines
deterministically first — low ASR token probability, or a token phonetically near
an on-screen word but not equal to it — and escalate only the ~18 ambiguous ones.

### The phonetic gate

`min(direct_similarity, consonant_skeleton_similarity)`:

```
division   -> duration    0.50  accept   (correct)
blockboard -> block       0.67  accept   (correct)
division   -> schedules   0.24  reject   (hallucination)
start us   -> Schedules   0.33  reject   (hallucination)
2020       -> BrainClean  0.00  reject   (hallucination)
PowerPoint -> BrainClean  0.30  reject   (correct but phonetically distant)
```

The last row is a known limitation — product-name substitutions need a separate
deterministic rule (user supplies the app name), not this gate.

---

## 15. Genre coverage — what is implemented, and how to add more

**The architecture is general. Exactly one detector is written.** This section is
the map for extending it, not a statement of limits.

| Layer | Generality |
| --- | --- |
| AI architecture (gated selector, probe, fallbacks) | universal — not even video-specific |
| Plan + time map, render, audio master, verification | any video |
| Speech detection | any video with speech |
| Disfluency removal | any **spoken-word** video — works today across genres |
| **Freeze detection + silent-AND-frozen rule** | **screen recordings only** |
| **Cut-vs-speed-up at 4 s** | **demos/tutorials with waits** |

**Even within one genre, value is not predictable from the genre label — it is a
measurable property**: how much of the video is simultaneously silent and
"nothing happening". Two screen recordings measured **25.7%** and **4.6%**
removable. Same genre, six-fold difference. This is why the pre-flight audit
below matters more than genre classification.

```
video       length  speech  frozen  removable
video_2       176s   61.4%   65.2%     25.7%   -> pipeline delivers
brainclean   1170s   74.8%   89.6%     ~22%    -> pipeline delivers
video        1168s   95.6%   56.4%      4.6%   -> almost nothing to cut
```

The third case narrates continuously — there is genuinely no dead air. **The
tool did nothing rather than something wrong**, which is the correct failure
mode.

**Ship a pre-flight audit** that reports these three numbers in ~1 minute, before
any editing, so the tool is never judged on footage it cannot help.
**Rule of thumb: ≥12% removable is worth running; below ~5% it will do nothing.**

**Where it would actively damage:** the pipeline assumes dead air is a defect.
False for scripted video, interviews where a beat carries meaning, comedy where
timing *is* the content, and anything musical. It cannot infer intent. Restrict
to unscripted instructional content.

---

## 16. The revision loop

Because the edit is a **declarative plan**, "watch it, then say what to change"
is nearly free.

```
$ revise list
 id     at kind          removed  detail
  3   0:31 false_start      7.5s  'i have added some'
 33  10:38 speedup         71.8s  77.8s wait -> 6.0s at 13x

$ revise undo 3
after: 15:17 (+7.5s), re-encode 1 of 50 segments (2%) -- rest stream-copied
```

Three properties make it work, all inherited:
1. Removals are addressable in **output** time via the time map
2. Amendments edit the plan, not the analysis — no VAD/Whisper re-runs
3. Rendering is already segment-based, so only changed segments re-encode

Measured revision cost: **4.3 s re-encode + 2.3 s concat + 49 s audio master ≈
under a minute**, versus ~35 minutes for a full render.

**Design in decision provenance from the start.** Each removal must carry *why*
(`false_start`, `filler`, `dead_air`, `speedup`) or the user cannot reason about
it.

Support two distinct feedback kinds:
- **local override** — "put *this* back" (`undo 3`)
- **rule adjustment** — "stop removing 'So' everywhere" (change threshold, re-plan)

---

## 17. Rust implementation notes

### Crate suggestions

| Need | Crate | Note |
| --- | --- | --- |
| ffmpeg | **shell out to the CLI** | what the Python did; stable interface, avoids binding pain. `ffmpeg-next` only if you need in-process frames |
| WAV read | `hound` | |
| Arrays / DSP | `ndarray`, `rustfft` | replaces numpy |
| Silero VAD | `ort` (ONNX Runtime) | Silero ships ONNX |
| Whisper | `whisper-rs` | bindings to whisper.cpp; needs a C++ toolchain |
| Plan / config | `serde` + `serde_json` | **typed plan struct is a real win over Python** |
| HTTP (Ollama) | `reqwest` or `ureq` | |
| CLI | `clap` | |
| Parallel segment render | `rayon` or a bounded task pool | the big perf win |
| Errors | `anyhow` / `thiserror` | |

### Structural advice

- **Model the plan as a typed struct** with `serde`. Segment, TimeMap, Decision
  (with a provenance enum). In Python these were dicts and it was the main source
  of fragility.
- **Make the AI layer a trait** (`LlmProvider`) with implementations for Ollama,
  OpenAI-compatible, and a Null provider. The Null one must let the whole
  pipeline pass — enforce with a test.
- **Enforce the core/ai boundary** with module structure and a CI check.
- **`DeadTimeSignal` as a trait** returning a boolean mask on the plan grid.
  Freeze detection is the first implementation; slide-change and diarization
  become drop-ins. This is what makes genre extension cheap.

### One honest warning

This project succeeded through **fast measurement iteration** — dozens of
threshold sweeps and probes, each answered in seconds. Rust's compile cycle makes
that loop slower, and the temptation will be to guess instead of measure.

**Suggestion: keep a small Python "lab" for threshold exploration and port only
settled logic to Rust.** The Rust binary is the product; the lab is how you find
the numbers. Do not let the language choice push you into assuming.

---

## 18. Build order

1. **Core pipeline with no AI at all** — probe, audio extract, Silero VAD,
   decision engine, render, master. Plus the **verification suite** and the
   core/ai boundary check. The tool must be useful with no model.
   Write it as **plain functions**. Keep Rule A and Rule B from section 1; they
   are what make later abstraction cheap. Do not write a trait yet.
2. **The pre-flight audit** (§15) — cheap, and it sets user expectations.
3. **Provider trait + capability probe.** Two providers proves
   model-agnosticism.
4. **`caption.intelligible`** — the safest first AI task; it only flags.
5. **OCR of stable frames** → per-scene vocabulary. Currently UNBUILT and on the
   critical path for the best AI feature.
6. **`caption.fix`** as gated proposals with one-click review.
7. **Disfluency removal** behind a flag, gated by the splice test.
8. **The revision loop.**
9. **Auto-crop** (§6) to remove the last manual step.
10. **A second genre — and only now, the trait.** Slide-change detection is the
    cheapest (it is freeze detection inverted), then diarization for interviews.
    Introduce `DeadTimeSignal` at this point, when you finally have two
    implementations to generalise over. Because of Rule A and Rule B this is a
    small mechanical change, not a rewrite.

---

## 19. Known-unvalidated / open questions

- **n = 1 for the full pipeline.** Every threshold is tuned to one recording, one
  mic, one speaker. Two other videos were audited but not fully edited. **Run the
  pipeline over 10–20 varied recordings and catalogue failures before building
  much.** Freeze detection and the disfluency splice test are the likely first
  casualties.
- **The Silero result was validated against my own labels on the same video.**
  Encouraging, not conclusive.
- **The AI figures come from ~15 items on one model.** Directionally solid
  (generation vs selection is a large effect), but the specific tier cut-offs are
  not yet earned.
- **OCR is assumed, not built**, and the best AI feature depends on it.
- **Jump cuts are visible when there's a webcam** — 28 speech cuts means 28 head
  jumps. Acceptable on YouTube, but consider warning or offering cross-dissolves.
- **Language.** Filler lists, the enumerator guard and caption line-breaking are
  English-specific.
- **`deepseek-r1:7b` was installed but never probed.** Reasoning models may do
  better on adjudication and worse on latency.
