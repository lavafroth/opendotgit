use color_eyre::{
    eyre::{Result, WrapErr},
    Section,
};
mod args;
mod constants;
mod download;
mod expression;
mod logging;
mod pack;
mod response;
mod runner;
mod webpage;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = args::parse();
    logging::init(cli.verbose)?;

    // Create the output directory specified in the command line arguments
    // and ensure that all parent directories exist.
    std::fs::create_dir_all(&cli.output)
        .wrap_err("Failed to create output directory")
        .suggestion("Try supplying a location you can write to")?;

    // Set the current working directory to the output directory.
    log::info!("Changing current directory to \"{}\"", &cli.output);
    std::env::set_current_dir(&cli.output)?;

    // Spawn a new `Runner` instance with the specified URL and tasks.
    runner::run(cli).await?;

    Ok(())
}
