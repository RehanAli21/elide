//! The provider abstraction. Two implementations: Ollama and Null.
//!
//! Null exists so the whole pipeline can be proven to run with no model at all.
//! It fails every call, which forces the caller down its deterministic fallback
//! — the only way to be sure the fallback is real and not decorative.

use std::process::Command;

use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::Value;

/// Measured, never advertised. `reliable_ctx` and `tok_per_sec` come from the
/// probe, not from a model card.
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub name: String,
    pub json_mode: bool,
    pub tok_per_sec: f64,
    pub selection: f64,
    pub generation: f64,
    pub tier: Tier,
}

/// Tier is set on PRECISION, not coverage: abstaining is free because the gate
/// falls back, whereas acting wrongly is what costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// fallbacks only
    None,
    /// select only, strict gates
    Small,
    /// select + classify
    Mid,
    /// all tasks including generation
    Large,
}

impl Tier {
    pub fn from_scores(selection: f64, generation: f64) -> Tier {
        if selection >= 0.85 && generation >= 0.75 {
            Tier::Large
        } else if selection >= 0.75 {
            Tier::Mid
        } else if selection >= 0.55 {
            Tier::Small
        } else {
            Tier::None
        }
    }

    /// Generation is only allowed at the top tier. Everything else gets the
    /// deterministic fallback for generative tasks.
    pub fn allows_generation(self) -> bool {
        self == Tier::Large
    }

    pub fn allows_selection(self) -> bool {
        !matches!(self, Tier::None)
    }
}

pub trait LlmProvider {
    fn name(&self) -> &str;

    /// `schema`, where given, is applied as a native constraint. Ollama's
    /// `format` produced valid JSON on 100% of calls; without it, budget a
    /// repair loop.
    fn complete(
        &self,
        prompt: &str,
        schema: Option<&Value>,
        max_tokens: u32,
        temperature: f64,
    ) -> Result<String>;
}

#[derive(Deserialize)]
struct GenResponse {
    response: String,
}

pub struct Ollama {
    pub model: String,
    pub url: String,
    pub timeout_s: u32,
}

impl Ollama {
    pub fn new(model: &str) -> Self {
        Ollama {
            model: model.to_string(),
            url: "http://localhost:11434/api/generate".to_string(),
            timeout_s: 60,
        }
    }
}

impl LlmProvider for Ollama {
    fn name(&self) -> &str {
        &self.model
    }

    fn complete(
        &self,
        prompt: &str,
        schema: Option<&Value>,
        max_tokens: u32,
        temperature: f64,
    ) -> Result<String> {
        let mut body = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "stream": false,
            "options": { "temperature": temperature, "num_predict": max_tokens },
        });
        if let Some(s) = schema {
            body["format"] = s.clone();
        }

        let out = match Command::new("curl")
            .args([
                "-s",
                "-m",
                &self.timeout_s.to_string(),
                "-X",
                "POST",
                &self.url,
                "-H",
                "Content-Type: application/json",
                "-d",
                &body.to_string(),
            ])
            .output()
        {
            Ok(o) => o,
            Err(e) => return Err(anyhow::Error::from(e).context("could not run curl")),
        };

        if !out.status.success() {
            bail!("provider unreachable");
        }
        let envelope: GenResponse = match serde_json::from_slice(&out.stdout) {
            Ok(v) => v,
            Err(e) => {
                return Err(anyhow::Error::from(e).context("provider replied with unreadable JSON"));
            }
        };
        Ok(envelope.response)
    }
}

/// Fails every call on purpose. The pipeline must pass with this installed.
pub struct Null;

impl LlmProvider for Null {
    fn name(&self) -> &str {
        "null"
    }

    fn complete(&self, _: &str, _: Option<&Value>, _: u32, _: f64) -> Result<String> {
        bail!("null provider")
    }
}
