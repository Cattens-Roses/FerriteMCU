//! Transport framing and text position utilities shared by language-server clients.
//!
//! These functions perform blocking I/O and must be called on a worker thread, never the UI
//! thread. A client owns the child process, request routing, cancellation and lifecycle.

use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Reject pathological messages before allocating their body.
const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024;

/// Read one LSP message, returning `None` only for EOF between messages.
pub fn read_message(reader: &mut impl BufRead) -> io::Result<Option<serde_json::Value>> {
    let mut content_length = None;
    let mut header_bytes = 0usize;
    loop {
        let mut line = Vec::new();
        let read = reader
            .take((MAX_HEADER_BYTES - header_bytes + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return if header_bytes == 0 {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "EOF in LSP headers",
                ))
            };
        }
        header_bytes = header_bytes.saturating_add(read);
        if header_bytes > MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LSP headers too large",
            ));
        }
        if line == b"\r\n" {
            break;
        }
        let header = std::str::from_utf8(&line)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid LSP header"))?;
        let (name, value) = header
            .trim_end_matches("\r\n")
            .split_once(':')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid LSP header"))?;
        if name.eq_ignore_ascii_case("Content-Length") {
            if content_length.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "duplicate Content-Length",
                ));
            }
            let length: usize = value.trim().parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length")
            })?;
            if length > MAX_MESSAGE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "LSP message too large",
                ));
            }
            content_length = Some(length);
        }
    }
    let length = content_length
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let message = serde_json::from_slice(&body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    trace_wire_message("server -> client", &message);
    Ok(Some(message))
}

/// Write and flush a JSON-RPC message using LSP framing.
pub fn write_message(writer: &mut impl Write, message: &serde_json::Value) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(io::Error::other)?;
    if body.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "LSP message too large",
        ));
    }
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    trace_wire_message("client -> server", message);
    Ok(())
}

/// Optional wire trace, enabled by setting `FERRITEMCU_LSP_TRACE` to a file path (or `1` to
/// use FerriteMCU's local app-data log). It records full JSON-RPC payloads for diagnosis.
fn trace_wire_message(direction: &str, message: &serde_json::Value) {
    static TRACE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    let Some(file) = TRACE
        .get_or_init(|| {
            let setting = std::env::var_os("FERRITEMCU_LSP_TRACE")?;
            let configured = PathBuf::from(&setting);
            let path = if configured == Path::new("1") || configured == Path::new("true") {
                std::env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(std::env::temp_dir)
                    .join("FerriteMCU")
                    .join("logs")
                    .join("rust-analyzer-lsp.jsonl")
            } else {
                configured
            };
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
                .map(Mutex::new)
        })
        .as_ref()
    else {
        return;
    };
    let Ok(mut file) = file.lock() else { return };
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let _ = writeln!(
        file,
        "{{\"time_ms\":{timestamp},\"direction\":{},\"message\":{}}}",
        serde_json::Value::String(direction.to_owned()),
        message
    );
    let _ = file.flush();
}

/// Find the nearest ancestor directory containing a Cargo manifest.
pub fn cargo_root(path: &Path) -> Option<PathBuf> {
    let directory = if path.is_dir() { path } else { path.parent()? };
    directory
        .ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file())
        .map(Path::to_path_buf)
}

/// Convert an absolute file or directory path to its encoded `file:` URI.
pub fn path_to_uri(path: &Path) -> io::Result<String> {
    let path = std::fs::canonicalize(path)?;
    let uri = if path.is_dir() {
        url::Url::from_directory_path(path)
    } else {
        url::Url::from_file_path(path)
    }
    .map_err(|()| io::Error::new(io::ErrorKind::InvalidInput, "invalid file path"))?;
    Ok(uri.to_string())
}

/// Convert an LSP file URI to a native path, rejecting non-file schemes.
pub fn uri_to_path(uri: &str) -> io::Result<PathBuf> {
    let url =
        url::Url::parse(uri).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    url.to_file_path()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "LSP URI is not a local file"))
}

/// Probe the PATH-resolved rust-analyzer in a project directory. Rustup proxies can exist
/// without a corresponding component, so availability requires a successful version check.
/// This blocks for at most `timeout` and must run on a worker thread.
pub fn system_rust_analyzer(root: &Path, timeout: std::time::Duration) -> io::Result<PathBuf> {
    let name = if cfg!(windows) {
        "rust-analyzer.exe"
    } else {
        "rust-analyzer"
    };
    let started = std::time::Instant::now();
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path).filter(|path| path.is_absolute()) {
            let binary = directory.join(name);
            if binary.is_file()
                && probe_rust_analyzer(&binary, root, timeout.saturating_sub(started.elapsed()))
                    .is_ok()
            {
                return Ok(binary);
            }
            if started.elapsed() >= timeout {
                break;
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no usable rust-analyzer found on PATH",
    ))
}

fn probe_rust_analyzer(binary: &Path, root: &Path, timeout: std::time::Duration) -> io::Result<()> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(binary);
    command
        .current_dir(root)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn()?;
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "rust-analyzer is not available for this toolchain",
                ))
            };
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "rust-analyzer version check timed out",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Resolve a usable server from PATH or the managed cache, downloading an official release
/// when necessary. This performs process and network I/O and must run on a worker thread.
pub fn resolve_rust_analyzer(root: &Path, cache: &Path) -> io::Result<PathBuf> {
    let timeout = std::time::Duration::from_secs(5);
    if let Ok(binary) = system_rust_analyzer(root, timeout) {
        return Ok(binary);
    }
    let cached = cache.join(if cfg!(windows) {
        "rust-analyzer.exe"
    } else {
        "rust-analyzer"
    });
    if cached.exists() {
        if probe_rust_analyzer(&cached, root, timeout).is_ok() {
            return Ok(cached);
        }
        std::fs::remove_file(&cached)?;
    }
    let downloaded = install::install(cache)?;
    probe_rust_analyzer(&downloaded, root, timeout)?;
    Ok(downloaded)
}

/// Convert a UTF-8 byte offset to an LSP UTF-16 `(line, character)` position.
/// Returns `None` for out-of-bounds offsets or offsets inside a UTF-8 code point.
pub fn byte_to_utf16_position(text: &str, offset: usize) -> Option<(u32, u32)> {
    let prefix = text.get(..offset)?;
    let line = u32::try_from(prefix.bytes().filter(|&byte| byte == b'\n').count()).ok()?;
    let last_line = prefix.rsplit('\n').next()?;
    // The CR in a CRLF delimiter is not a character in the LSP line.
    let last_line = if text.as_bytes().get(offset) == Some(&b'\n') {
        last_line.strip_suffix('\r').unwrap_or(last_line)
    } else {
        last_line
    };
    let character = u32::try_from(last_line.encode_utf16().count()).ok()?;
    Some((line, character))
}

/// Convert an LSP UTF-16 `(line, character)` position to a UTF-8 byte offset.
/// Positions inside a surrogate pair or beyond the line are rejected.
pub fn utf16_position_to_byte(text: &str, line: u32, character: u32) -> Option<usize> {
    let mut start = 0usize;
    for _ in 0..line {
        start += text.get(start..)?.find('\n')? + 1;
    }
    let rest = text.get(start..)?;
    let end = rest.find('\n').unwrap_or(rest.len());
    let line_text = rest.get(..end)?.strip_suffix('\r').unwrap_or(&rest[..end]);
    let mut units = 0u32;
    for (offset, ch) in line_text.char_indices() {
        if units == character {
            return Some(start + offset);
        }
        units = units.checked_add(ch.len_utf16() as u32)?;
        if units > character {
            return None;
        }
    }
    (units == character).then_some(start + line_text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn framing_round_trips_unicode_and_multiple_messages() {
        let first = serde_json::json!({"text": "é😀"});
        let second = serde_json::json!({"id": 2});
        let mut bytes = Vec::new();
        write_message(&mut bytes, &first).unwrap();
        write_message(&mut bytes, &second).unwrap();
        let mut reader = Cursor::new(bytes);
        assert_eq!(read_message(&mut reader).unwrap(), Some(first));
        assert_eq!(read_message(&mut reader).unwrap(), Some(second));
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }

    #[test]
    fn rejects_invalid_or_truncated_frames() {
        for input in [
            "Content-Length: 2\r\n\r\n{",
            "Content-Length: 1\r\nContent-Length: 1\r\n\r\n0",
            "Content-Length: 999999999\r\n\r\n",
            "Missing: 1\r\n\r\n",
        ] {
            assert!(read_message(&mut Cursor::new(input.as_bytes())).is_err());
        }
    }

    #[test]
    fn utf16_positions_reject_partial_characters_and_handle_crlf() {
        let text = "a😀b\r\nnext";
        assert_eq!(byte_to_utf16_position(text, 5), Some((0, 3)));
        assert_eq!(utf16_position_to_byte(text, 0, 3), Some(5));
        assert_eq!(utf16_position_to_byte(text, 0, 2), None);
        assert_eq!(utf16_position_to_byte(text, 0, 4), Some(6));
        assert_eq!(byte_to_utf16_position(text, 7), Some((0, 4)));
        assert_eq!(utf16_position_to_byte(text, 1, 0), Some(8));
        assert_eq!(byte_to_utf16_position(text, 2), None);
    }
}

/// A running language server. Its stdout is read on a dedicated worker thread.
/// Start and shutdown must run off the UI thread.
pub struct Session {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    incoming: std::sync::mpsc::Receiver<io::Result<serde_json::Value>>,
    temp_dir: Option<PathBuf>,
}

impl Session {
    /// Start a server and complete the initialize/initialized handshake.
    pub fn start(executable: &Path, root: &Path, timeout: std::time::Duration) -> io::Result<Self> {
        use std::process::{Command, Stdio};
        let root = std::fs::canonicalize(root)?;
        let root_uri = url::Url::from_directory_path(&root)
            .map_err(|()| io::Error::new(io::ErrorKind::InvalidInput, "invalid project root"))?;
        #[cfg(windows)]
        let temp_dir = Some(create_server_temp_dir(&root)?);
        #[cfg(not(windows))]
        let temp_dir: Option<PathBuf> = None;
        let mut command = Command::new(executable);
        command
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            if let Some(temp_dir) = temp_dir.as_ref() {
                // Sandboxed desktop hosts can supply TEMP paths containing `{}`. Cargo uses
                // this path for rust-analyzer's temporary lockfile, where braces are parsed
                // as config-variable interpolation. Keep the override scoped to the server
                // process; changing the app-wide environment would race other workers.
                command.env("TEMP", temp_dir).env("TMP", temp_dir);
            }
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                if let Some(temp_dir) = temp_dir.as_ref() {
                    let _ = std::fs::remove_dir_all(temp_dir);
                }
                return Err(error);
            }
        };
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("missing server stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing server stdout"))?;
        let (sender, incoming) = std::sync::mpsc::sync_channel(256);
        std::thread::spawn(move || {
            let mut reader = io::BufReader::new(stdout);
            loop {
                match read_message(&mut reader) {
                    Ok(Some(message)) => {
                        if sender.send(Ok(message)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = sender.send(Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "language server exited",
                        )));
                        break;
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });
        let mut session = Self {
            child,
            stdin,
            incoming,
            temp_dir,
        };
        session.send(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "processId": std::process::id(), "rootUri": root_uri.as_str(),
                "workspaceFolders": [{"uri": root_uri.as_str(), "name": root.file_name().unwrap_or_default().to_string_lossy()}],
                "capabilities": {
                    "general": {"positionEncodings": ["utf-16"]},
                    "experimental": {"serverStatusNotification": true}
                },
                "clientInfo": {"name": "FerriteMCU", "version": env!("CARGO_PKG_VERSION")}
            }
        }))?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "language server initialization timed out",
                ));
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let message = session
                .incoming
                .recv_timeout(remaining)
                .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error))??;
            if message.get("id") == Some(&serde_json::json!(1)) && message.get("method").is_none() {
                if let Some(error) = message.get("error") {
                    return Err(io::Error::other(format!("initialize failed: {error}")));
                }
                if message.get("result").is_none() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "missing initialize result",
                    ));
                }
                break;
            }
            if let Some(id) = message
                .get("id")
                .filter(|_| message.get("method").is_some())
            {
                let method = message["method"].as_str().unwrap_or_default();
                let params = &message["params"];
                let response = match method {
                    "workspace/configuration" => serde_json::json!({
                        "jsonrpc":"2.0", "id":id,
                        "result":params["items"].as_array()
                            .map(|items| vec![serde_json::Value::Null; items.len()])
                            .unwrap_or_default()
                    }),
                    "workspace/workspaceFolders" => serde_json::json!({
                        "jsonrpc":"2.0", "id":id,
                        "result":[{"uri":root_uri.as_str(), "name":root.file_name().unwrap_or_default().to_string_lossy()}]
                    }),
                    "window/workDoneProgress/create"
                    | "client/registerCapability"
                    | "client/unregisterCapability"
                    | "window/showMessageRequest" => {
                        serde_json::json!({"jsonrpc":"2.0", "id":id, "result":null})
                    }
                    _ => {
                        serde_json::json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32601, "message":format!("Method not found: {method}")}})
                    }
                };
                session.send(&response)?;
            }
        }
        session.send(&serde_json::json!({"jsonrpc":"2.0", "method":"initialized", "params":{}}))?;
        Ok(session)
    }

    /// Send a JSON-RPC message to the server. This is blocking and belongs on a worker thread.
    pub fn send(&mut self, message: &serde_json::Value) -> io::Result<()> {
        write_message(&mut self.stdin, message)
    }

    /// Receive the next server message with a bounded wait.
    pub fn receive(&self, timeout: std::time::Duration) -> io::Result<serde_json::Value> {
        self.incoming
            .recv_timeout(timeout)
            .map_err(|error| match error {
                std::sync::mpsc::RecvTimeoutError::Timeout => {
                    io::Error::new(io::ErrorKind::TimedOut, error)
                }
                std::sync::mpsc::RecvTimeoutError::Disconnected => {
                    io::Error::new(io::ErrorKind::UnexpectedEof, error)
                }
            })?
    }

    /// Ask the server to shut down cleanly, then wait briefly for its exit.
    /// This blocks and must be called on a worker thread. Dropping the session still kills
    /// an unresponsive process as a fallback.
    pub fn shutdown(mut self, timeout: std::time::Duration) -> io::Result<()> {
        const ID: &str = "ferritemcu-shutdown";
        let deadline = std::time::Instant::now() + timeout;
        self.send(
            &serde_json::json!({"jsonrpc":"2.0", "id":ID, "method":"shutdown", "params":null}),
        )?;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "server shutdown timed out",
                ));
            }
            let message = self.receive(remaining)?;
            if message.get("id") == Some(&serde_json::json!(ID)) && message.get("method").is_none()
            {
                if let Some(error) = message.get("error") {
                    return Err(io::Error::other(format!("server shutdown failed: {error}")));
                }
                break;
            }
            if let Some(id) = message
                .get("id")
                .filter(|_| message.get("method").is_some())
            {
                self.send(&serde_json::json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32601,"message":"client is shutting down"}}))?;
            }
        }
        self.send(&serde_json::json!({"jsonrpc":"2.0", "method":"exit", "params":null}))?;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(io::Error::other(format!("server exited with {status}")))
                };
            }
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "server did not exit",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(temp_dir) = self.temp_dir.take() {
            let _ = std::fs::remove_dir_all(temp_dir);
        }
    }
}

#[cfg(windows)]
fn create_server_temp_dir(root: &Path) -> io::Result<PathBuf> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let safe_profile = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|profile| {
            profile
                .join("AppData")
                .join("Local")
                .join("FerriteMCU")
                .join("rust-analyzer-temp")
        })
        .filter(|path| !path.to_string_lossy().contains(['{', '}']));
    let candidate = safe_profile.unwrap_or_else(|| root.join("target").join(".ferritemcu-lsp-tmp"));
    let session_dir = candidate.join(format!("session-{}-{sequence}", std::process::id()));
    match std::fs::create_dir_all(&session_dir) {
        Ok(()) => Ok(session_dir),
        Err(profile_error) if candidate != root.join("target").join(".ferritemcu-lsp-tmp") => {
            let fallback = root
                .join("target")
                .join(".ferritemcu-lsp-tmp")
                .join(format!("session-{}-{sequence}", std::process::id()));
            std::fs::create_dir_all(&fallback).map(|()| fallback).map_err(|fallback_error| io::Error::other(format!("could not create rust-analyzer temp directory ({profile_error}); project fallback failed ({fallback_error})")))
        }
        Err(error) => Err(error),
    }
}

/// Official rust-analyzer release management.
pub mod install;

/// An open LSP document; the owner advances its version after every successful change.
pub struct Document {
    uri: String,
    version: i64,
}

impl Document {
    /// Open a Rust document in a running server.
    pub fn open(session: &mut Session, path: &Path, text: &str) -> io::Result<Self> {
        Self::open_at_version(session, path, text, 1)
    }

    /// Open a document at a caller-owned version. This is used when a crashed server is
    /// restarted so the editor's version counter remains monotonic across the restart.
    pub fn open_at_version(
        session: &mut Session,
        path: &Path,
        text: &str,
        version: i64,
    ) -> io::Result<Self> {
        if version < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "document version cannot be negative",
            ));
        }
        let path = std::fs::canonicalize(path)?;
        let uri = url::Url::from_file_path(path)
            .map_err(|()| io::Error::new(io::ErrorKind::InvalidInput, "invalid file path"))?
            .to_string();
        session.send(&serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"rust","version":version,"text":text}}}))?;
        Ok(Self { uri, version })
    }
    /// Synchronize the entire new document contents.
    pub fn change(&mut self, session: &mut Session, text: &str) -> io::Result<()> {
        let version = self
            .version
            .checked_add(1)
            .ok_or_else(|| io::Error::other("document version exhausted"))?;
        self.change_at_version(session, text, version)
    }

    /// Synchronize contents using the editor's explicit document version.
    pub fn change_at_version(
        &mut self,
        session: &mut Session,
        text: &str,
        version: i64,
    ) -> io::Result<()> {
        if version <= self.version {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "document version must increase",
            ));
        }
        session.send(&serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":self.uri,"version":version},"contentChanges":[{"text":text}]}}))?;
        self.version = version;
        Ok(())
    }
    /// Notify the server after a successful save.
    pub fn save(&self, session: &mut Session) -> io::Result<()> {
        session.send(&serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didSave","params":{"textDocument":{"uri":self.uri}}}))
    }
    /// Close this document in the server.
    pub fn close(self, session: &mut Session) -> io::Result<()> {
        session.send(&serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":self.uri}}}))
    }
    /// The document URI used in LSP requests and notifications.
    pub fn uri(&self) -> &str {
        &self.uri
    }
    /// Current document version.
    pub fn version(&self) -> i64 {
        self.version
    }
}
