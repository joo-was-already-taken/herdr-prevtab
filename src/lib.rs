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
    #[error("unexpected response")]
    ConnectionClosed,
    #[error("{method} failed: {detail}")]
    RequestFailed {
        method: &'static str,
        detail: String,
    },
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

struct HerdrConnection {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl HerdrConnection {
    fn connect(socket_path: &Path) -> io::Result<Self> {
        let stream = UnixStream::connect(socket_path)?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            reader,
            writer: stream,
        })
    }

    fn request(
        &mut self,
        req_id: &str,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        let req_json = json!({
            "id": req_id,
            "method": method,
            "params": params,
        });
        writeln!(&self.writer, "{req_json}")?;
        self.read_response(req_id)
    }

    fn read_response(&mut self, req_id: &str) -> Result<serde_json::Value, Error> {
        let mut buf = String::new();
        loop {
            buf.clear();
            if self.reader.read_line(&mut buf)? == 0 {
                return Err(HerdrError::ConnectionClosed.into());
            }
            let val: serde_json::Value =
                serde_json::from_str(&buf).map_err(HerdrError::from)?;
            let is_response = val
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| id == req_id);
            if is_response {
                return Ok(val);
            }
        }
    }
}

fn resp_ok(resp: &serde_json::Value, method: &'static str) -> Result<(), Error> {
    if let Some(err) = resp.get("error") {
        return Err(HerdrError::RequestFailed {
            method,
            detail: err.to_string(),
        }
        .into());
    }
    Ok(())
}

pub fn jump_back(socket_path: &Path, tab_id: &str) -> Result<(), Error> {
    let mut conn = HerdrConnection::connect(socket_path)?;
    let resp = conn.request(
        &format!("{SHORT_PLUGIN_ID}_jump_back"),
        "tab.focus",
        &json!({ "tab_id": tab_id }),
    )?;
    resp_ok(&resp, "jump back")
}

pub fn workspace_jump_back(socket_path: &Path, workspace_id: &str) -> Result<(), Error> {
    let mut conn = HerdrConnection::connect(socket_path)?;
    let resp = conn.request(
        &format!("{SHORT_PLUGIN_ID}_ws_jump_back"),
        "workspace.focus",
        &json!({ "workspace_id": workspace_id }),
    )?;
    resp_ok(&resp, "jump back")
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
    let mut conn = HerdrConnection::connect(socket_path)?;

    if let Ok(Some((tab_id, workspace_id))) = fetch_herdr_snapshot(&mut conn) {
        let _ = state.on_tab_focused(&tab_id, &workspace_id, state_dir);
    } else {
        log::warn!("failed to fetch herdr snapshot");
    }

    subscribe_tab_focused_events(&mut conn)?;
    log::info!("subscribed to 'tab.focused' events");

    process_events(&mut conn, &mut state, state_dir)
}

fn process_events(
    conn: &mut HerdrConnection,
    state: &mut CurrentTabs,
    state_dir: &Path,
) -> Result<(), Error> {
    let mut buf = String::new();
    loop {
        buf.clear();
        if conn.reader.read_line(&mut buf)? == 0 {
            return Ok(());
        }
        let val: serde_json::Value =
            serde_json::from_str(&buf).map_err(HerdrError::from)?;

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
}

fn fetch_herdr_snapshot(
    conn: &mut HerdrConnection,
) -> Result<Option<(String, String)>, Error> {
    let snapshot = conn.request(
        &format!("{SHORT_PLUGIN_ID}_snapshot"),
        "session.snapshot",
        &json!({}),
    )?;

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

fn subscribe_tab_focused_events(conn: &mut HerdrConnection) -> Result<(), Error> {
    let resp = conn.request(
        &format!("{SHORT_PLUGIN_ID}_sub_tab_focused"),
        "events.subscribe",
        &json!({ "subscriptions": [{"type": "tab.focused"}] }),
    )?;
    resp_ok(&resp, "subscription")
}

pub fn herdr_notify(socket_path: &Path, body: &str) -> Result<(), Error> {
    let mut conn = HerdrConnection::connect(socket_path)?;
    let resp = conn.request(
        &format!("{SHORT_PLUGIN_ID}_notify"),
        "notification.show",
        &json!({ "title": SHORT_PLUGIN_ID, "body": body }),
    )?;
    resp_ok(&resp, "notification")
}
