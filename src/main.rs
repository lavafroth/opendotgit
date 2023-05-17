use clap::{ArgAction::Count, Parser};
use color_eyre::{
    eyre::{Result, WrapErr},
    Section,
};
use url::Url;
mod pack;
mod path;
mod runner;
mod webpage;
use runner::Runner;

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
    simple_logger::init_with_level(log::Level::Info).unwrap();
    std::fs::create_dir_all(&cli.output)
        .wrap_err("Failed to create output directory")
        .suggestion("Try supplying a location you can write to")?;
    log::info!("Changing current directory to \"{}\"", &cli.output);
    std::env::set_current_dir(cli.output)?;
    Runner::new(&cli.url, cli.tasks).run().await?;
    Ok(())
}
