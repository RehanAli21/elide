use elide::ai::capabilities::{describe, probe};
use elide::ai::provider::{Null, Ollama};

fn main() {
    for p in [
        &Ollama::new("qwen2.5:7b") as &dyn elide::ai::provider::LlmProvider,
        &Null,
    ] {
        let c = probe(p);
        println!(
            "{:<12} json {:<5} sel {:.2}  gen {:.2}  {:.1} tok/s  tier {:?}",
            c.name, c.json_mode, c.selection, c.generation, c.tok_per_sec, c.tier
        );
        println!("  {}", describe(&c));
    }
}
