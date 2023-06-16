use clap::{ArgAction::Count, Parser};
use color_eyre::{
    eyre::{Result, WrapErr},
    Section,
};
use url::Url;
mod constants;
mod expression;
mod logging;
mod macros;
mod pack;
mod runner;
mod webpage;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    /// URL of the .git directory
    url: Url,

    /// Directory to output the results
    output: String,

    /// Number of asynchronous jobs to spawn
    #[arg(short, long, default_value_t = 8)]
    tasks: usize,

    /// Turn debugging information on
    #[arg(short, long, action = Count)]
    verbose: u8,
}
#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    logging::init(cli.verbose)?;

    // Create the output directory specified in the command line arguments
    // and ensure that all parent directories exist.
    std::fs::create_dir_all(&cli.output)
        .wrap_err("Failed to create output directory")
        .suggestion("Try supplying a location you can write to")?;

    // Set the current working directory to the output directory.
    log::info!("Changing current directory to \"{}\"", &cli.output);
    std::env::set_current_dir(cli.output)?;

    // Spawn a new `Runner` instance with the specified URL and tasks.
    runner::Runner::new(&cli.url, cli.tasks).run().await?;

    Ok(())
}
