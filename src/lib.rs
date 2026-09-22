// A `Result` that is called and then ignored is an error that vanishes: the
// program carries on as if the step worked. This makes every such case a BUILD
// error, in every module — not a warning, and not only inside `main`.
#![deny(unused_must_use)]

pub mod align;
pub mod audio;
pub mod captions;
pub mod cli;
pub mod constants;
pub mod crop;
pub mod disfluency;
pub mod dsp;
pub mod features;
pub mod freeze;
pub mod master;
pub mod ai;
pub mod plan;
pub mod policy;
pub mod probe;
pub mod render;
pub mod signal;
pub mod utilities;
pub mod vad;
pub mod verify;
