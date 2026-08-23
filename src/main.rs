use herdr_prevtab::{herdr_notify, jump_back, run_subscriber, workspace_jump_back};

use clap::Parser;
use rustix::fs::{FlockOperation, flock};
use rustix::process::{Pid, Signal, kill_process};

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    /// Start the long-lived tab-focus subscriber daemon.
    Rund,
    /// Focus the previous tab and swap state (one-shot).
    JumpBack,
    /// Focus the previously active workspace (one-shot).
    WorkspaceJumpBack,
}

fn run_daemon(socket_path: &Path, state_path: &Path) {
    let lock_path = state_path.join("writer.lock");
    if let Some(pid) = fs::read_to_string(&lock_path)
        .ok()
        .and_then(|pid| pid.trim().parse::<i32>().ok())
        .and_then(Pid::from_raw)
    {
        let _ = kill_process(pid, Signal::TERM);
    } else {
        log::debug!("no valid previous daemon PID found in lock file");
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap_or_else(|e| error!("failed to open lock file: {e}"));

    let mut locked = false;
    for _ in 0..50 {
        if flock(&file, FlockOperation::NonBlockingLockExclusive).is_ok() {
            locked = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !locked {
        let _ = herdr_notify(
            socket_path,
            "Another daemon instance is already running (failed to acquire lock)",
        );
        error!("another subscriber instance is already running (failed to acquire lock)");
    }

    let res = unsafe { libc::daemon(0, 0) };
    if res < 0 {
        error!("failed to daemonize: {}", io::Error::last_os_error());
    }

    if let Err(e) = file
        .set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)))
        .and_then(|_| write!(file, "{}", std::process::id()))
    {
        error!("failed to write PID to lock file: {e}");
    }
    run_subscriber(socket_path, state_path);
}

fn main() {
    env_logger::init();
    let cli = Cli::parse();

    let socket_path = env::var("HERDR_SOCKET_PATH")
        .map_or_else(|_| error!("HERDR_SOCKET_PATH is not set"), PathBuf::from);
    let state_path = env::var("HERDR_PLUGIN_STATE_DIR").map_or_else(
        |_| error!("HERDR_PLUGIN_STATE_DIR is not set"),
        |state_path| {
            let session_hash = &herdr_prevtab::session_hash(&socket_path);
            let session_hash_str = std::str::from_utf8(session_hash).unwrap();
            PathBuf::from(state_path).join(session_hash_str)
        },
    );

    if let Err(e) = fs::create_dir(&state_path)
        && e.kind() != io::ErrorKind::AlreadyExists
    {
        error!("failed to create state directory: {e}");
    }

    match cli {
        Cli::Rund => run_daemon(&socket_path, &state_path),
        Cli::JumpBack => {
            let workspace_id = env::var("HERDR_WORKSPACE_ID")
                .unwrap_or_else(|_| error!("HERDR_WORKSPACE_ID is not set"));

            let state_file = herdr_prevtab::StateFile::PreviousTab {
                dir: &state_path,
                ws_id: &workspace_id,
            };

            let previous = if let Ok(id) = state_file.read()
                && !id.is_empty()
            {
                id
            } else {
                let _ = herdr_notify(
                    &socket_path,
                    "No previous tab recorded for this workspace",
                );
                error!("no previous tab recorded for this workspace");
            };

            if let Err(e) = jump_back(&socket_path, &previous) {
                let _ = herdr_notify(
                    &socket_path,
                    "Wasn't able to switch to the previous tab",
                );
                error!("{e}");
            }
        },
        Cli::WorkspaceJumpBack => {
            let state_file =
                herdr_prevtab::StateFile::PreviousWorkspace { dir: &state_path };
            let previous_ws = if let Ok(id) = state_file.read()
                && !id.is_empty()
            {
                id
            } else {
                let _ = herdr_notify(&socket_path, "No previous workspace recorded");
                error!("no previous workspace recorded");
            };

            if let Err(e) = workspace_jump_back(&socket_path, &previous_ws) {
                let _ = herdr_notify(
                    &socket_path,
                    "Wasn't able to switch to the previous workspace",
                );
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
