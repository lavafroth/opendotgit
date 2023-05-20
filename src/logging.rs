use color_eyre::{eyre::bail, Result};
use simple_logger;
pub fn init(verbosity: u8) -> Result<()> {
    simple_logger::init_with_level(match verbosity {
        0 => log::Level::Info,
        1 => log::Level::Debug,
        2 => log::Level::Trace,
        _ => {
            bail!("I'm sorry, but revealing too much information might wake the real Elliot. For now, let's focus on Dark Army, shall we?")
        }
    })?;
    Ok(())
}
