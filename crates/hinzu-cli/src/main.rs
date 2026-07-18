//! The hinzu CLI. A thin shell: parse argv, hand off to hinzu-core.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "hinzu", version, about = "hinzu")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the engine. Placeholder until the real surface lands.
    Run,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Run => {
            println!("{}", hinzu_core::run()?);
            Ok(())
        }
    }
}
