use anyhow::Result;
use clap::{Parser, Subcommand};
use koala::{config, tui};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "koala", version, about = "Extensible terminal agent")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start an interactive chat session (default)
    Chat,
    /// Install a local extension directory (contains extension.toml)
    ExtensionInstall {
        source: PathBuf,
        #[arg(long, default_value = ".koala/extensions")]
        directory: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Chat) {
        Command::ExtensionInstall { source, directory } => {
            let manifest =
                koala::extensions::install(&source, &directory).map_err(anyhow::Error::msg)?;
            println!(
                "Installed. Add to [extensions].manifests in config.toml:\n{}",
                serde_json::to_string(&manifest)?
            );
        }
        Command::Chat => tui::run(&config::Config::load()?).await?,
    }
    Ok(())
}
