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
}
