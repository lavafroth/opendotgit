use clap::{ArgAction::Count, Parser};
use tokio::time::Duration;
use url::Url;
#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct Args {
    /// URL of the .git directory
    pub url: Url,

    /// Directory to output the results
    pub output: String,

    /// Number of asynchronous jobs to spawn
    #[arg(short = 'j', long, default_value_t = 8)]
    pub jobs: usize,

    /// Turn debugging information on
    #[arg(short, long, action = Count)]
    pub verbose: u8,

    /// Number of times to retry a failed request
    #[arg(short, long, default_value_t = 3)]
    pub retries: usize,

    /// Timeout beyond which a request is no longer retried
    #[arg(short, long, default_value = "10", value_parser = parse_seconds, value_name="SECONDS")]
    pub timeout: Duration,
}

pub fn parse() -> Args {
    Args::parse()
}

fn parse_seconds(arg: &str) -> Result<std::time::Duration, std::num::ParseIntError> {
    let seconds = arg.parse()?;
    Ok(Duration::from_secs(seconds))
}
