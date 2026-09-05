use herdr_prevtab::{herdr_notify, jump_back, run_subscriber, workspace_jump_back};

use clap::Parser;
use rustix::fs::{FlockOperation, flock};
use rustix::process::{Pid, Signal, kill_process, setsid, umask};

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const PLUGIN_VERSION: &str = env!("PLUGIN_VERSION");

const DAEMON_SHUTDOWN_CMD: &[u8] = b"SHUTDOWN";

macro_rules! error {
    ($($args:tt)*) => {
        {
            log::error!($($args)*);
            std::process::exit(1);
        }
    };
}

#[derive(Parser, Debug)]
#[command(name = "herdr-prevtab", version = env!("PLUGIN_VERSION"), about)]
enum Cli {
    /// Start the long-lived tab-focus subscriber daemon.
    Rund,
    /// Focus the previous tab and swap state (one-shot).
    JumpBack,
    /// Focus the previously active workspace (one-shot).
    WorkspaceJumpBack,
}

/// Detaches into the background and redirects stderr to `log_path`.
/// Unsafe to call when already spawned other threads.
unsafe fn daemonize(log_path: &Path) -> io::Result<()> {
    unsafe fn fork() -> io::Result<()> {
        match unsafe { libc::fork() } {
            -1 => Err(io::Error::last_os_error()),
            0 => Ok(()),
            _ => unsafe { libc::_exit(0) },
        }
    }

    unsafe { fork()? };
    setsid()?;
    unsafe { fork()? };

    umask(rustix::fs::Mode::empty());

    env::set_current_dir("/")?;

    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let null = OpenOptions::new().read(true).open("/dev/null")?;

    rustix::stdio::dup2_stdin(&null)?;
    rustix::stdio::dup2_stdout(&log)?;
    rustix::stdio::dup2_stderr(&log)?;

    Ok(())
}

fn run_daemon(socket_path: &Path, state_path: &Path) {
    let lock_path = state_path.join("writer.lock");
    let daemon_sock = state_path.join("daemon.sock");
    let version_path = state_path.join("daemon.version");

    // Prevent starting two daemons at the same time
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
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
        error!("another subscriber instance is already running (failed to acquire lock)");
    }

    if let Err(e) = fs::write(&version_path, PLUGIN_VERSION) {
        log::warn!("failed to write daemon version: {e}");
    }

    if let Err(e) = unsafe { daemonize(&state_path.join("daemon.log")) } {
        error!("failed to daemonize: {e}");
    }

    let _ = fs::remove_file(&daemon_sock);
    let listener = UnixListener::bind(&daemon_sock)
        .unwrap_or_else(|e| error!("failed to bind daemon socket: {e}"));

    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buf = Vec::new();
            if stream.read_to_end(&mut buf).is_ok() && buf == DAEMON_SHUTDOWN_CMD {
                log::info!(
                    "received {} command, exiting",
                    std::str::from_utf8(DAEMON_SHUTDOWN_CMD).unwrap()
                );
                let _ = fs::remove_file(&daemon_sock);
                std::process::exit(0);
            }
        }
    });

    run_subscriber(socket_path, state_path);
}

fn ensure_daemon_running(state_path: &Path, socket_path: &Path) {
    let version_path = state_path.join("daemon.version");
    let daemon_sock = state_path.join("daemon.sock");

    let running_version = fs::read_to_string(&version_path).unwrap_or_default();

    if running_version.trim() == PLUGIN_VERSION {
        return;
    }

    if let Ok(mut stream) = UnixStream::connect(&daemon_sock) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
        let _ = stream.write_all(DAEMON_SHUTDOWN_CMD);
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let copy_res = io::copy(&mut stream, &mut io::sink());
        if let Err(e) = copy_res
            && matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut,
            )
        {
            let _ = herdr_notify(
                socket_path,
                "Could not shutdown the old daemon, please run `pkill herdr-prevtab`",
            );
            log::warn!("Could not shutdown the old daemon: {e}");
        }
    } else {
        let lock_path = state_path.join("writer.lock");
        if let Some(pid) = fs::read_to_string(&lock_path)
            .ok()
            .and_then(|pid| pid.trim().parse::<i32>().ok())
            .and_then(Pid::from_raw)
        {
            let _ = kill_process(pid, Signal::TERM);
        }
    }

    if let Ok(exe) = env::current_exe() {
        if let Err(e) = std::process::Command::new(exe).arg("rund").spawn() {
            log::warn!("failed to autostart daemon: {e}");
        }

        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(1) {
            if UnixStream::connect(&daemon_sock).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
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
            ensure_daemon_running(&state_path, &socket_path);

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
            ensure_daemon_running(&state_path, &socket_path);

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
