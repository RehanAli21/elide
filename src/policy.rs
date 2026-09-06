//! Prompt -> policy, in one step.
//!
//! **THE PROMPT SETS POLICY. THE PROMPT NEVER TOUCHES A GUARD.**
//!
//! Policy is how the edit should *feel* — whether to remove disfluencies, how
//! hard to compress dead air, target loudness, pacing. Guards are what make a
//! cut *safe* — QUIET, GATE, the 0.10 s quiet-run guard, SNAP. Guards are fixed
//! in code forever and nothing here can reach them.
//!
//! The reason is not stylistic. The disfluency score rewards removing more
//! seconds, and it is only honest because the quiet-run guard makes damaging
//! cuts unavailable. A prompt that could widen a guard could talk the pipeline
//! into destroying the audio and would score itself higher for doing it.
//!
//! Parsed ONCE, up front, into a struct. After that the pipeline is
//! deterministic given the struct — no model influence sprinkled through the
//! fourteen steps.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ai::provider::{Capabilities, LlmProvider};
use crate::plan::Pacing;

pub use crate::plan::DeadAir;

// ---------------------------------------------------------------------------
// Why the model is asked ONE question and not eight
// ---------------------------------------------------------------------------
//
// The first version asked the model to fill all eight policy fields in a single
// schema-constrained call. On "a short demo of my application called
// BrainClean" it answered `remove_disfluencies: false` — three times out of
// three, deterministically — and silently disabled the whole disfluency stage
// on a demo. Every verification check still passed.
//
// Asked that same question ALONE, the model gets it right five times out of
// five. So the field was never the problem; the LOAD was.
//
// The second version therefore asked seven separate questions. That fixed the
// disfluency field, but two more problems showed up:
//
//   * Questions with no way to abstain do not measure what the description
//     says, they measure what the model will invent. Asked for a delivery
//     loudness, it answered -16 LUFS for a description that says nothing about
//     loudness, overriding a target backed by measurement against the
//     reference's own exports. Giving every question an explicit "not stated"
//     option (index 0, the shape the capability probe already uses) fixed that.
//   * The answers were not robust. Editing the wording of the PAUSE question
//     flipped the answer to the DISFLUENCY question, whose text had not
//     changed. The model was pattern-matching the option lists rather than
//     reading the description, and tuning seven prompts against the spec's four
//     worked examples is fitting to the test set, not understanding it.
//
// Measured, both designs, same six descriptions:
//
//     seven questions   unstable; wording of one question moves another
//     one genre pick    30/30 correct, and identical across five repeats
//
// So: the model answers the one question the capability probe actually measures
// it on — pick one option by index, where it scores 0.83 — and the mapping from
// genre to the eight fields lives HERE, in code, as a table. That table is the
// spec's own worked-example table. It is reviewable, diffable, and identical on
// every run, which the seven-question version was not.
//
// This is the same lesson as the capability probe itself: a component that
// cannot tell when it is guessing is worse than a constant.

/// The kinds of video the policy table knows about. `Other` means the model
/// could not place it, and everything stays on defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Genre {
    Demo,
    Talk,
    Interview,
    Gameplay,
    Other,
}

impl Genre {
    fn from_index(i: i64) -> Genre {
        match i {
            0 => Genre::Demo,
            1 => Genre::Talk,
            2 => Genre::Interview,
            3 => Genre::Gameplay,
            _ => Genre::Other,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Genre::Demo => "demo",
            Genre::Talk => "talk",
            Genre::Interview => "interview",
            Genre::Gameplay => "gameplay",
            Genre::Other => "unclassified",
        }
    }
}

/// The options the model picks from. Index order must match `from_index`.
const GENRE_OPTIONS: &str = "0 = a screen recording, app demo or software tutorial; \
                             1 = a conference talk, lecture or presentation to an audience; \
                             2 = an interview, podcast or conversation between people; \
                             3 = gameplay or long-form footage with commentary; \
                             4 = none of these";

#[derive(Deserialize)]
struct Choice {
    index: i64,
}

impl Policy {
    /// **The spec's worked-example table, in code.**
    ///
    /// | prompt | what changes |
    /// | --- | --- |
    /// | "demonstration of my app, for YouTube" | freeze detection on; disfluencies removed; -14 LUFS |
    /// | "conference talk recording" | `expect_screen=false` — dead air from audio only; disfluencies removed; pause floor higher, a speaker's pauses are rhetorical |
    /// | "podcast interview, two people" | disfluencies OFF — natural speech is the product; dead air `Cut` not `Speed`; -16 LUFS |
    /// | "raw gameplay, cut the loading screens" | freeze detection on; `max_speed` high; disfluencies off |
    fn from_genre(g: Genre) -> Policy {
        let base = Policy::default();
        match g {
            Genre::Demo => base,

            // No screen to crop to, so dead air comes from audio alone. A
            // presenter's pauses are rhetorical — do not shorten the short ones.
            Genre::Talk => Policy {
                expect_screen: false,
                pause_floor_s: 1.5,
                ..base
            },

            // The prompt switching off the most expensive feature in the tool
            // is correct behaviour, and the clearest proof it does real work.
            // A fast-forward through a conversational pause looks wrong, so
            // dead air is removed outright rather than sped up.
            Genre::Interview => Policy {
                remove_disfluencies: false,
                dead_air: DeadAir::Cut,
                target_lufs: -16.0,
                expect_screen: false,
                ..base
            },

            // Commentary is unscripted; its disfluencies are the register, not
            // a defect. Loading screens are exactly what max_speed is for.
            Genre::Gameplay => Policy {
                remove_disfluencies: false,
                max_speed: 20.0,
                ..base
            },

            Genre::Other => base,
        }
    }
}

/// Deterministic overrides, read straight from the description. No model.
///
/// These are the things a user writes explicitly, where a keyword is both more
/// reliable than a classifier and easier to explain when it fires. Bounds still
/// apply afterwards — `clamp` is the only thing that decides what is legal.
fn apply_explicit(p: &mut Policy, prompt: &str) -> Vec<String> {
    let text = prompt.to_lowercase();
    let mut fired = Vec::new();

    if text.contains("no captions")
        || text.contains("no subtitles")
        || text.contains("without captions")
        || text.contains("without subtitles")
    {
        p.captions = false;
        fired.push("captions off".to_string());
    }

    // An explicit loudness, e.g. "deliver at -16 LUFS". Read the number that
    // immediately precedes the unit rather than the first number in the text,
    // so "a 20 minute talk at -16 LUFS" does not pick up the 20.
    if let Some(at) = text.find("lufs") {
        let before: String = text[..at].trim_end().to_string();
        let num: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if let Ok(v) = num.parse::<f64>() {
            if (-23.0..=-14.0).contains(&v) {
                p.target_lufs = v;
                fired.push(format!("{v} LUFS"));
            }
        }
    }

    if text.contains("keep the ums")
        || text.contains("keep the filler")
        || text.contains("don't remove filler")
        || text.contains("do not remove filler")
    {
        p.remove_disfluencies = false;
        fired.push("disfluencies off".to_string());
    }

    fired
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub remove_disfluencies: bool,
    pub dead_air: DeadAir,
    pub max_speed: f64,
    pub pause_floor_s: f64,
    pub target_lufs: f64,
    pub expect_screen: bool,
    pub captions: bool,
    pub chapters: bool,
    /// how this was arrived at — recorded so "why did it do that?" has an answer
    pub source: String,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            remove_disfluencies: true,
            dead_air: DeadAir::Speed,
            max_speed: 20.0,
            pause_floor_s: 1.0,
            target_lufs: -14.0,
            expect_screen: true,
            captions: true,
            chapters: false, // unbuilt
            source: "defaults".to_string(),
        }
    }
}

impl Policy {
    /// Clamp every field into its documented range. A value outside the range
    /// is not an error to argue about — it is simply replaced by the bound.
    /// This is the ONLY place a policy number becomes legal.
    fn clamp(&mut self) {
        self.max_speed = self.max_speed.clamp(1.0, 20.0);
        self.pause_floor_s = self.pause_floor_s.clamp(0.3, 1.5);
        self.target_lufs = self.target_lufs.clamp(-23.0, -14.0);
    }

    /// Ask the model what kind of video this is. One index pick — the shape the
    /// capability probe measures. Returns None whenever it cannot be trusted,
    /// and None always means "use defaults", never "guess".
    fn classify(prompt: &str, provider: &dyn LlmProvider) -> Option<Genre> {
        let schema = json!({
            "type": "object",
            "properties": { "index": { "type": "integer" } },
            "required": ["index"]
        });
        let ask = format!(
            "Pick exactly one option by index.\n\nSITUATION: {prompt}\n\n\
             QUESTION: What kind of video is this?\n\nOPTIONS: {GENRE_OPTIONS}\n\nReply JSON."
        );
        let raw = provider.complete(&ask, Some(&schema), 50, 0.0).ok()?;
        let i = serde_json::from_str::<Choice>(&raw).ok()?.index;
        Some(Genre::from_index(i))
    }

    /// Resolve a prompt into policy. Falls back to defaults, loudly, whenever
    /// the model is absent, unusable, or returns something out of contract —
    /// the tool must run with no model at all.
    pub fn resolve(prompt: &str, provider: Option<(&dyn LlmProvider, &Capabilities)>) -> Policy {
        let mut p;
        let mut why;

        match provider {
            None => {
                p = Policy::default();
                why = "defaults (--no-ai)".to_string();
            }
            Some((provider, caps)) if caps.tier.allows_selection() => {
                match Policy::classify(prompt, provider) {
                    Some(Genre::Other) | None => {
                        p = Policy::default();
                        why = format!("defaults ({} could not classify it)", caps.name);
                    }
                    Some(g) => {
                        p = Policy::from_genre(g);
                        why = format!("{} via {}", g.name(), caps.name);
                    }
                }
            }
            Some((_, caps)) => {
                p = Policy::default();
                why = format!("defaults ({} unusable)", caps.name);
            }
        }

        // Explicit instructions in the description beat the genre table, and
        // are read without a model.
        let fired = apply_explicit(&mut p, prompt);
        if !fired.is_empty() {
            why = format!("{why}; stated: {}", fired.join(", "));
        }

        p.clamp();
        p.source = why;
        p
    }

    /// The subset the planner is allowed to see. Everything else stays here.
    pub fn pacing(&self) -> Pacing {
        Pacing {
            dead_air: self.dead_air,
            max_speed: self.max_speed,
            pause_floor_s: self.pause_floor_s,
        }
    }

    /// Replay an exact parameter set.
    pub fn from_file(path: &str) -> Result<Policy> {
        let raw = std::fs::read_to_string(path)?;
        let mut p: Policy = serde_json::from_str(&raw)?;
        p.clamp(); // a hand-edited file is still not allowed out of range
        Ok(p)
    }

    pub fn summary(&self) -> String {
        format!(
            "disfluencies {}  dead air {:?}  max speed {:.0}x  pause floor {:.2}s  \
             target {:.1} LUFS  screen {}  captions {}  [{}]",
            if self.remove_disfluencies { "on" } else { "off" },
            self.dead_air,
            self.max_speed,
            self.pause_floor_s,
            self.target_lufs,
            self.expect_screen,
            self.captions,
            self.source
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_range_values_are_clamped_not_honoured() {
        let mut p = Policy {
            max_speed: 999.0,
            pause_floor_s: 0.0,
            target_lufs: 0.0,
            ..Default::default()
        };
        p.clamp();
        assert_eq!(p.max_speed, 20.0);
        assert_eq!(p.pause_floor_s, 0.3);
        assert_eq!(p.target_lufs, -14.0);
    }

    #[test]
    fn no_provider_gives_defaults() {
        let p = Policy::resolve("anything at all", None);
        assert_eq!(p.dead_air, DeadAir::Speed);
        assert!(p.remove_disfluencies);
        assert!(p.source.contains("defaults"));
    }

    /// The spec's worked-example table. If the table drifts from the spec, this
    /// is what says so — no model, no network, so it always runs.
    #[test]
    fn genre_table_matches_the_spec() {
        let demo = Policy::from_genre(Genre::Demo);
        assert!(demo.expect_screen, "demo: freeze detection on");
        assert!(demo.remove_disfluencies, "demo: disfluencies removed");
        assert_eq!(demo.target_lufs, -14.0);

        let talk = Policy::from_genre(Genre::Talk);
        assert!(!talk.expect_screen, "talk: dead air from audio only");
        assert!(talk.remove_disfluencies, "talk: disfluencies removed");
        assert!(
            talk.pause_floor_s > demo.pause_floor_s,
            "talk: pause floor higher, a speaker's pauses are rhetorical"
        );

        let pod = Policy::from_genre(Genre::Interview);
        assert!(!pod.remove_disfluencies, "podcast: disfluencies OFF");
        assert_eq!(pod.dead_air, DeadAir::Cut, "podcast: Cut, not Speed");
        assert_eq!(pod.target_lufs, -16.0);

        let game = Policy::from_genre(Genre::Gameplay);
        assert!(game.expect_screen, "gameplay: freeze detection on");
        assert!(!game.remove_disfluencies, "gameplay: disfluencies off");
        assert_eq!(game.max_speed, 20.0, "gameplay: max_speed high");
    }

    /// An unclassified video must edit exactly as it did before this feature
    /// existed — the model failing is never allowed to change the edit.
    #[test]
    fn unclassified_is_byte_identical_to_defaults() {
        let d = Policy::default();
        let o = Policy::from_genre(Genre::Other);
        assert_eq!(o.remove_disfluencies, d.remove_disfluencies);
        assert_eq!(o.dead_air, d.dead_air);
        assert_eq!(o.max_speed, d.max_speed);
        assert_eq!(o.pause_floor_s, d.pause_floor_s);
        assert_eq!(o.target_lufs, d.target_lufs);
        assert_eq!(o.expect_screen, d.expect_screen);
        assert_eq!(o.captions, d.captions);
    }

    #[test]
    fn stated_loudness_beats_the_genre() {
        // podcast would give -16; the description overrules it
        let mut p = Policy::from_genre(Genre::Interview);
        apply_explicit(&mut p, "podcast interview, deliver at -20 LUFS");
        assert_eq!(p.target_lufs, -20.0);
    }

    #[test]
    fn stated_loudness_reads_the_number_next_to_the_unit() {
        let mut p = Policy::default();
        apply_explicit(&mut p, "a 20 minute talk at -16 LUFS");
        assert_eq!(p.target_lufs, -16.0);
    }

    #[test]
    fn an_out_of_range_stated_loudness_is_ignored() {
        let mut p = Policy::default();
        let fired = apply_explicit(&mut p, "master it at -60 LUFS");
        assert_eq!(p.target_lufs, -14.0, "out of range, so the default stands");
        assert!(fired.is_empty());
    }

    #[test]
    fn captions_can_be_switched_off_by_the_description() {
        let mut p = Policy::default();
        apply_explicit(&mut p, "demo of my app, no subtitles please");
        assert!(!p.captions);
    }
}
