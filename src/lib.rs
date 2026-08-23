use serde_json::json;

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const SHORT_PLUGIN_ID: &str = "prevtab";

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Herdr(#[from] HerdrError),
}

#[derive(thiserror::Error, Debug)]
pub enum HerdrError {
    #[error("{0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("invalid tab format: '{0}'")]
    InvalidTabFormat(String),
    #[error("unexpected response")]
    ConnectionClosed,
    #[error("unexpected JSON")]
    UnexpectedJson,
    #[error("subscription failed: {0}")]
    SubscriptionFailed(String),
    #[error("notification failed: {0}")]
    NotificationFailed(String),
    #[error("jump back failed: {0}")]
    JumpBackFailed(String),
}

pub const SESSION_HASH_LEN: usize = 12;
pub type SessionHash = [u8; SESSION_HASH_LEN];

#[must_use]
pub fn session_hash(socket_path: &Path) -> SessionHash {
    let charset = b"herd";
    let mut hash_value = seahash::hash(socket_path.as_os_str().as_bytes());
    let mut output = [0; SESSION_HASH_LEN];
    for byte in &mut output {
        *byte = charset[usize::try_from(hash_value % charset.len() as u64).unwrap()];
        hash_value /= charset.len() as u64;
    }
    output
}

pub enum StateFile<'a> {
    PreviousTab { dir: &'a Path, ws_id: &'a str },
    PreviousWorkspace { dir: &'a Path },
}

impl StateFile<'_> {
    pub fn read(&self) -> io::Result<String> {
        let content = fs::read_to_string(self.path())?;
        Ok(content.trim().to_string())
    }

    pub fn write(&self, content: &str) -> io::Result<()> {
        let path = self.path();
        let tmp_path = PathBuf::from(format!("{}.tmp", path.display()));
        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        fs::rename(tmp_path, path)?;
        Ok(())
    }

    fn path(&self) -> PathBuf {
        match self {
            Self::PreviousTab { dir, ws_id } => dir.join(ws_id),
            Self::PreviousWorkspace { dir } => dir.join("prev_workspace"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CurrentTabs {
    pub current_tabs: BTreeMap<String, String>,
    pub current_workspace: Option<String>,
}

impl CurrentTabs {
    pub fn on_tab_focused(
        &mut self,
        tab_id: &str,
        workspace_id: &str,
        state_dir: &Path,
    ) -> Result<(), Error> {
        use std::collections::btree_map::Entry;

        let cur_tab_entry = self.current_tabs.entry(workspace_id.to_string());
        if let Entry::Occupied(entry) = &cur_tab_entry
            && entry.get() != tab_id
        {
            StateFile::PreviousTab {
                dir: state_dir,
                ws_id: workspace_id,
            }
            .write(entry.get())?;
        }
        cur_tab_entry.insert_entry(tab_id.to_string());

        if let Some(cur_ws) = &self.current_workspace
            && cur_ws != workspace_id
        {
            StateFile::PreviousWorkspace { dir: state_dir }.write(cur_ws)?;
        }
        self.current_workspace = Some(workspace_id.to_string());

        Ok(())
    }
}

pub fn jump_back(socket_path: &Path, tab_id: &str) -> Result<(), Error> {
    let stream = UnixStream::connect(socket_path)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let writer = stream;

    let req_id = format!("{SHORT_PLUGIN_ID}_jump_back");
    let req_json = format!(
        "{}",
        json!(
            {
                "id": &req_id,
                "method": "tab.focus",
                "params": {
                    "tab_id": tab_id
                }
            }
        )
    );
    assert!(!req_json.contains('\n'));
    writeln!(&writer, "{req_json}")?;

    let resp = read_response(&mut reader, &req_id)?;
    if let Some(err) = resp.get("error") {
        Err(HerdrError::JumpBackFailed(err.to_string()))?;
    }

    Ok(())
}

pub fn workspace_jump_back(socket_path: &Path, workspace_id: &str) -> Result<(), Error> {
    let stream = UnixStream::connect(socket_path)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let writer = stream;

    let req_id = format!("{SHORT_PLUGIN_ID}_ws_jump_back");
    let req_json = format!(
        "{}",
        json!(
            {
                "id": &req_id,
                "method": "workspace.focus",
                "params": {
                    "workspace_id": workspace_id
                }
            }
        ),
    );
    assert!(!req_json.contains('\n'));
    writeln!(&writer, "{req_json}")?;

    let resp = read_response(&mut reader, &req_id)?;
    if let Some(err) = resp.get("error") {
        Err(HerdrError::JumpBackFailed(err.to_string()))?;
    }

    Ok(())
}

pub fn run_subscriber(socket_path: &Path, state_path: &Path) {
    let min_backoff = Duration::from_millis(10);
    let max_backoff = Duration::from_secs(2);
    let mut backoff = min_backoff;
    let mut max_interval_retries = 0;

    loop {
        let start = Instant::now();

        match subscribe_and_run(socket_path, state_path) {
            Ok(()) => log::info!("subscriber connection closed gracefully"),
            Err(e) => log::warn!("subscriber error: {e}"),
        }

        if start.elapsed() > Duration::from_secs(5) {
            backoff = min_backoff;
            max_interval_retries = 0;
        }

        if backoff == max_backoff {
            max_interval_retries += 1;
            if max_interval_retries >= 10 {
                log::error!("failed to reconnect after multiple attempts, exiting");
                return;
            }
        }

        log::info!("reconnecting in {backoff:?}");
        thread::sleep(backoff);
        backoff = (backoff * 2).min(max_backoff);
    }
}

fn subscribe_and_run(socket_path: &Path, state_dir: &Path) -> Result<(), Error> {
    let mut state = CurrentTabs::default();

    if let Ok(mut snapshot_stream) = UnixStream::connect(socket_path) {
        let mut snapshot_reader = BufReader::new(snapshot_stream.try_clone()?);
        if let Ok(Some((tab_id, workspace_id))) =
            fetch_herdr_snapshot(&mut snapshot_stream, &mut snapshot_reader)
        {
            let _ = state.on_tab_focused(&tab_id, &workspace_id, state_dir);
        } else {
            log::warn!("failed to fetch herdr snapshot");
        }
    }

    let stream = UnixStream::connect(socket_path)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    subscribe_tab_focused_events(&mut writer, &mut reader)?;
    log::info!("subscribed to 'tab.focused' events");

    for line in reader.lines() {
        let val: serde_json::Value =
            serde_json::from_str(&line?).map_err(HerdrError::from)?;

        if val.get("event").and_then(serde_json::Value::as_str) == Some("tab_focused") {
            let tab_id = val
                .pointer("/data/tab_id")
                .and_then(serde_json::Value::as_str);
            let workspace_id = val
                .pointer("/data/workspace_id")
                .and_then(serde_json::Value::as_str);
            if let (Some(tab_id), Some(workspace_id)) = (tab_id, workspace_id)
                && let Err(e) = state.on_tab_focused(tab_id, workspace_id, state_dir)
            {
                log::warn!("failed to save state: {e}");
            }
        }
    }
    Ok(())
}

fn fetch_herdr_snapshot(
    writer: &mut impl Write,
    reader: &mut impl BufRead,
) -> Result<Option<(String, String)>, Error> {
    let req_id = format!("{SHORT_PLUGIN_ID}_snapshot");
    let req_json = json!(
        {
            "id": &req_id,
            "method": "session.snapshot",
            "params": {}
        }
    );
    writeln!(writer, "{req_json}")?;
    let snapshot = read_response(reader, &req_id)?;

    let tab_id = snapshot
        .pointer("/result/snapshot/focused_tab_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let workspace_id = snapshot
        .pointer("/result/snapshot/focused_workspace_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);

    if let (Some(t), Some(w)) = (tab_id, workspace_id) {
        Ok(Some((t, w)))
    } else {
        Ok(None)
    }
}

fn subscribe_tab_focused_events(
    writer: &mut impl Write,
    reader: &mut impl BufRead,
) -> Result<(), Error> {
    let req_id = format!("{SHORT_PLUGIN_ID}_sub_tab_focused");
    let req_json = json!(
        {
            "id": &req_id,
            "method": "events.subscribe",
            "params": {
                "subscriptions": [
                    {"type": "tab.focused"}
                ]
            }
        }
    );
    writeln!(writer, "{req_json}")?;

    let resp = read_response(reader, &req_id)?;
    if let Some(err) = resp.get("error") {
        return Err(HerdrError::SubscriptionFailed(err.to_string()).into());
    }

    Ok(())
}

pub fn herdr_notify(socket_path: &Path, body: &str) -> Result<(), Error> {
    let stream = UnixStream::connect(socket_path)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    let req_id = format!("{SHORT_PLUGIN_ID}_notify");
    let req_json = json!(
        {
            "id": &req_id,
            "method": "notification.show",
            "params": {
                "title": SHORT_PLUGIN_ID,
                "body": body
            }
        }
    );
    writeln!(writer, "{req_json}")?;

    let resp = read_response(&mut reader, &req_id)?;
    if let Some(err) = resp.get("error") {
        Err(HerdrError::NotificationFailed(err.to_string()))?;
    }

    Ok(())
}

fn read_response(
    reader: &mut impl BufRead,
    req_id: &str,
) -> Result<serde_json::Value, Error> {
    for line in reader.lines() {
        let raw = line?;
        let val: serde_json::Value =
            serde_json::from_str(&raw).map_err(HerdrError::from)?;
        let is_response = val
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| id == req_id);
        if is_response {
            return Ok(val);
        }
    }
    Err(HerdrError::ConnectionClosed.into())
}
