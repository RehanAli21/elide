# The interface: three inputs, nothing else

This is the contract the tool must satisfy. It is not described in
`PROJECT_HANDOFF.md`, `BUILD_STEPS.md` or `FLOW_SPEC.md` — those describe the
*flow*, and assume a human supplies the content region and picks the
parameters. **Read this first; it changes what Milestone 0 has to be.**

```
vidcut --input  demo.mp4 \
       --output out/ \
       --prompt "demonstration of my app BrainClean, for YouTube"
```

Nothing else is required. Every other flag is an override that exists for
debugging and for reproducing a past run — never something the user must
discover.

```
--crop W:H:X:Y     skip auto-detection (see §3)
--params file.json replay an exact parameter set
--dry-run          plan and report, encode nothing
--no-ai            skip the model entirely, use defaults
```

---

## 1. The rule that governs everything here

**The prompt sets policy. The prompt never touches a guard.**

Two categories, and the boundary is not negotiable:

| | examples | who decides |
| --- | --- | --- |
| **Policy** — how the edit should feel | remove disfluencies or not, how hard to compress dead air, target loudness | the prompt |
| **Guards** — what makes a cut safe | `QUIET`, `GATE`, the 0.10 s quiet-run guard, `SNAP` | **fixed in code, forever** |

The reason is in the handoff already: the disfluency score rewards removing
more seconds, and it is only honest because the quiet-run guard makes damaging
cuts *unavailable*. A prompt that could widen a guard could talk the pipeline
into destroying the audio and would score itself higher for doing it. So the
guards are not parameters. They are invariants.

A user saying "edit aggressively" gets more removed by policy — more dead air
collapsed, a lower pause floor. They do not get a looser splice test.

---

## 2. Prompt → policy, in one step

Parse the prompt **once, up front**, into a struct. Then the pipeline is
deterministic given that struct. Do not sprinkle model influence through the
fourteen steps.

```
Policy {
    remove_disfluencies : bool          // false for interviews/podcasts
    dead_air            : Cut | Speed | Keep
    max_speed           : 1.0 ..= 20.0
    pause_floor_s       : 0.3 ..= 1.5   // pacing
    target_lufs         : -23.0 ..= -14.0
    expect_screen       : bool          // enables freeze detection at all
    captions            : bool
    chapters            : bool
}
```

Every field is bounded. The model picks *within* the range or the value is
rejected and the default used — the same JSON-schema-constrained call, and the
same `ok=true` fallback, already specified for the monitor loop. **If Ollama is
unreachable the tool still runs**, on defaults, and says so.

Worked examples:

| prompt | what changes |
| --- | --- |
| "demonstration of my app, for YouTube" | screen recording → freeze detection on; disfluencies removed; `-14 LUFS`; chapters on |
| "conference talk recording" | `expect_screen=false` → dead air from audio only; disfluencies removed; pause floor higher, a speaker's pauses are rhetorical |
| "podcast interview, two people" | **disfluencies off** — natural speech is the product; dead air `Cut` not `Speed`; `-16 LUFS` |
| "raw gameplay, cut the loading screens" | freeze detection on, `max_speed` high; disfluencies off |

Note what the third row means: the prompt can switch off the most expensive
feature in the tool. That is correct behaviour, and it is the clearest proof
the prompt is doing real work rather than decorating a fixed pipeline.

**Log the resolved struct into `out/` next to the video.** A user asking "why
did it do that?" gets an answer, and `--params` replays it exactly.

---

## 3. Auto-detecting the content region

This is the piece that currently blocks the three-input interface —
`PROJECT_HANDOFF.md:451` marks it UNBUILT and `TOOL_DESIGN.md:48` calls it the
main unsolved problem. Freeze detection is meaningless without it: run
`freezedetect` on a full 1920×1080 frame and it never fires, because the OS
clock ticks and the webcam bubble moves.

**The signal: content is where pixels change over time; chrome is static.**

```
1. sample ~200 frames spread across the file, downscale each to 160x90
2. per-cell temporal variance across those samples
3. threshold -> activity mask
4. largest connected component -> bounding box -> scale back to full res
5. snap outward to even pixel coordinates (encoders want them)
```

Step 4 is what rejects the webcam bubble and the clock: both *do* change, but
both are small. The application window is the largest thing that changes.

**It must be able to say "I don't know."** If no region has meaningfully more
activity than the frame as a whole — a talking-head video, where everything
moves a little and nothing moves a lot — report no distinct content region,
set `expect_screen=false`, and take dead air from audio alone. This is the same
no-signal branch as the threshold calibration in §8 of the flow, and for the
same reason: on three test videos that calibration only worked on one, and
pretending otherwise would have been worse than admitting it.

**How to know it works.** You already have a hand-measured ground truth:
`crop=374:820:773:113` on `brainclean_demonstration.mp4`. Run freeze detection
with the auto crop and with the hand crop, and compare the resulting freeze
intervals. That is a pass/fail check, not an eyeball — the same standard the
rest of the verification suite is held to.

---

## 4. What this does to the milestones

`BUILD_STEPS.md` defers auto-crop to "after M8; `--crop` is fine". Keep that —
it is the right build order, and `--crop` remains useful forever as an
override. Two amendments:

- **M0 takes the real CLI from the start**: `--input`, `--output`, `--prompt`.
  The prompt can be parsed and ignored at first, but the shape must be right on
  day one, or every milestone builds against an interface you intend to throw
  away.
- **Auto-crop is not optional.** It is the last thing standing between you and
  the three-input tool, so it is a required milestone, not a nice-to-have.
  Schedule it immediately after freeze detection works with a manual crop.

Order that satisfies both: manual `--crop` through M8 → freeze detection
verified against it → auto-crop verified against the same ground truth →
`--crop` becomes an override.
