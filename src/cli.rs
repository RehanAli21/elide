use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Cli {
    /// input file path like demo.mp4
    #[arg(short, long)]
    pub input: String,

    /// output directory path like /home/user/demo_results
    #[arg(short, long)]
    pub output: String,

    /// prompt like "This video is a demo of my application called brainclean"
    #[arg(short, long)]
    pub prompt: String,

    /// optional word-level transcript (JSON: [{text,start,end,prob}]) to use
    /// instead of running whisper — a debugging override
    #[arg(long)]
    pub transcript: Option<String>,

    /// which "nothing is happening" signal to use: freeze (screen recordings),
    /// slides (slide lectures), none (talking head — silence decides alone).
    /// Overrides what the prompt implied.
    #[arg(long)]
    pub signal: Option<String>,

    /// skip the model entirely — policy comes from defaults, captions stay raw
    /// ASR. The edit is unaffected: the model never decides a cut.
    #[arg(long, default_value_t = false)]
    pub no_ai: bool,

    /// replay an exact parameter set (a policy.json from a previous run)
    /// instead of asking the model to read the prompt again
    #[arg(long)]
    pub params: Option<String>,
}
