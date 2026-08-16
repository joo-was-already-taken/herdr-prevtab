use herdr_prevtab::{jump_back, run_subscriber};

use clap::Parser;
use rustix::fs::{FlockOperation, flock};

use std::env;
use std::fs::OpenOptions;
use std::io;
use std::path::PathBuf;

macro_rules! error {
    ($($args:tt)*) => {
        {
            log::error!($($args)*);
            std::process::exit(1);
        }
    };
}

#[derive(Parser, Debug)]
#[command(name = "herdr-prevtab", version, about)]
enum Cli {
    /// Start the long-lived tab-focus subscriber.
    Run,
    /// Focus the previous tab and swap state (one-shot).
    JumpBack,
}

fn main() {
    env_logger::init();
    let cli = Cli::parse();

    let socket_path = env::var("HERDR_SOCKET_PATH")
        .map_or_else(|_| error!("HERDR_SOCKET_PATH is not set"), PathBuf::from);
    let state_path = env::var("HERDR_PLUGIN_STATE_DIR").map_or_else(
        |_| error!("HERDR_PLUGIN_STATE_DIR is not set"),
        PathBuf::from,
    );

    match cli {
        Cli::Run => {
            let lock_path = state_path.join("subscriber.lock");
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&lock_path)
                .unwrap_or_else(|e| error!("failed to open lock file: {e}"));
            if flock(&file, FlockOperation::NonBlockingLockExclusive).is_err() {
                error!(
                    "another subscriber instance is already running (failed to acquire lock)"
                );
            }
            run_subscriber(&socket_path, &state_path);
        },
        Cli::JumpBack => {
            let workspace_id = env::var("HERDR_WORKSPACE_ID")
                .unwrap_or_else(|_| error!("HERDR_WORKSPACE_ID is not set"));

            let state_file = herdr_prevtab::PreviousTabPath {
                dir: &state_path,
                ws_id: &workspace_id,
            };

            let previous = match state_file.read() {
                Ok(id) if !id.is_empty() => id,
                Ok(_) => {
                    error!("no previous tab recorded for this workspace");
                },
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    error!("no previous tab recorded for this workspace");
                },
                Err(e) => {
                    error!("failed to read state for workspace `{workspace_id}`: {e}")
                },
            };

            if let Err(e) = jump_back(&socket_path, &previous) {
                error!("{e}");
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn verify_cli() {
        Cli::command().debug_assert();
    }
}
