//! Background rust-analyzer bridge used by the Browse editor.
//!
//! All process, filesystem, and stdio work stays on this worker. The UI communicates with it
//! through owned commands; diagnostics return as immutable snapshots.

use std::{collections::HashMap, path::PathBuf, sync::mpsc, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RequestMethod {
    Completion,
    Hover,
    Definition,
    Formatting,
}

impl RequestMethod {
    fn lsp(self) -> &'static str {
        match self {
            Self::Completion => "textDocument/completion",
            Self::Hover => "textDocument/hover",
            Self::Definition => "textDocument/definition",
            Self::Formatting => "textDocument/formatting",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Diagnostic {
    pub range: (u32, u32, u32, u32),
    pub message: String,
    pub severity: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Off,
    Starting,
    Indexing {
        task: String,
        percentage: Option<u32>,
    },
    Ready,
    Restarting,
    Error(String),
}

impl Status {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Off => "Off".into(),
            Self::Starting => "Starting".into(),
            Self::Indexing { task, percentage } => match (task.is_empty(), percentage) {
                (true, Some(p)) => format!("Indexing {p}%"),
                (false, Some(p)) => format!("Indexing {p}% · {task}"),
                (false, None) => task.clone(),
                (true, None) => "Indexing".into(),
            },
            Self::Ready => "Ready".into(),
            Self::Restarting => "Restarting…".into(),
            Self::Error(_) => "Error".into(),
        }
    }

    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Off => "rust-analyzer is not active for this file".into(),
            Self::Starting => "Starting rust-analyzer".into(),
            Self::Indexing { task, percentage } => match (task.is_empty(), percentage) {
                (false, Some(p)) => format!("{task} — {p}%"),
                (false, None) => task.clone(),
                (true, Some(p)) => format!("Indexing — {p}%"),
                (true, None) => "rust-analyzer is indexing the project".into(),
            },
            Self::Ready => "rust-analyzer is ready".into(),
            Self::Restarting => "rust-analyzer stopped unexpectedly; restarting".into(),
            Self::Error(message) => message.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Diagnostics {
        path: PathBuf,
        version: Option<i64>,
        items: Vec<Diagnostic>,
    },
    Status {
        root: PathBuf,
        status: Status,
    },
    RequestResult {
        id: u64,
        path: PathBuf,
        version: i64,
        method: RequestMethod,
        result: serde_json::Value,
        error: Option<String>,
    },
}

enum Command {
    Start(PathBuf),
    Open(PathBuf, String, i64),
    Change(PathBuf, String, i64),
    Save(PathBuf),
    Close(PathBuf),
    Request {
        id: u64,
        path: PathBuf,
        version: i64,
        method: RequestMethod,
        params: serde_json::Value,
    },
    Stop,
}

#[derive(Clone)]
pub(crate) struct Client(mpsc::Sender<Command>);

impl Client {
    pub(crate) fn start(
        root: PathBuf,
        events: futures::channel::mpsc::UnboundedSender<Event>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let _worker = std::thread::Builder::new()
            .name("ferritemcu-rust-analyzer".into())
            .spawn(move || worker(rx, events))
            .expect("failed to create rust-analyzer worker");
        let client = Self(tx);
        let _ = client.0.send(Command::Start(root));
        client
    }
    pub(crate) fn open(&self, path: PathBuf, text: String, version: i64) {
        let _ = self.0.send(Command::Open(path, text, version));
    }
    pub(crate) fn change(&self, path: PathBuf, text: String, version: i64) {
        let _ = self.0.send(Command::Change(path, text, version));
    }
    pub(crate) fn save(&self, path: PathBuf) {
        let _ = self.0.send(Command::Save(path));
    }
    pub(crate) fn close(&self, path: PathBuf) {
        let _ = self.0.send(Command::Close(path));
    }
    pub(crate) fn request(
        &self,
        path: PathBuf,
        version: i64,
        method: RequestMethod,
        params: serde_json::Value,
    ) -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _ = self.0.send(Command::Request {
            id,
            path,
            version,
            method,
            params,
        });
        id
    }
    pub(crate) fn stop(&self) {
        let _ = self.0.send(Command::Stop);
    }
}

fn worker(rx: mpsc::Receiver<Command>, events: futures::channel::mpsc::UnboundedSender<Event>) {
    use ferritemcu_lsp::{Document, Session};
    let mut session: Option<Session> = None;
    let mut documents: HashMap<PathBuf, Document> = HashMap::new();
    // Retain only currently open documents so a crashed process can be rebuilt and resynced.
    let mut texts: HashMap<PathBuf, String> = HashMap::new();
    let mut versions: HashMap<PathBuf, i64> = HashMap::new();
    let mut progress: HashMap<serde_json::Value, (String, Option<u32>)> = HashMap::new();
    let mut root: Option<PathBuf> = None;
    let mut running = true;
    let mut restart_at = None;
    let mut retry_delay = Duration::from_secs(1);
    let mut restarting = false;
    let mut settled = false;
    let mut server_health_error: Option<String> = None;
    let mut pending: HashMap<String, (u64, PathBuf, i64, RequestMethod)> = HashMap::new();

    while running {
        if session.is_none()
            && restart_at.is_some_and(|at: std::time::Instant| at <= std::time::Instant::now())
        {
            let Some(server_root) = root.as_ref() else {
                restart_at = None;
                continue;
            };
            send_status(
                &events,
                server_root,
                if restarting {
                    Status::Restarting
                } else {
                    Status::Starting
                },
            );
            match launch(server_root) {
                Ok(mut started) => {
                    documents.clear();
                    for (path, text) in &texts {
                        let version = versions.get(path).copied().unwrap_or(1);
                        match Document::open_at_version(&mut started, path, text, version) {
                            Ok(document) => {
                                documents.insert(path.clone(), document);
                            }
                            Err(error) => {
                                send_status(&events, server_root, Status::Error(error.to_string()))
                            }
                        }
                    }
                    session = Some(started);
                    progress.clear();
                    settled = false;
                    server_health_error = None;
                    restart_at = None;
                    retry_delay = Duration::from_secs(1);
                    send_status(
                        &events,
                        server_root,
                        Status::Indexing {
                            task: String::new(),
                            percentage: None,
                        },
                    );
                }
                Err(error) => {
                    send_status(&events, server_root, Status::Error(error.to_string()));
                    if restarting {
                        restart_at = Some(std::time::Instant::now() + retry_delay);
                        retry_delay = (retry_delay * 2).min(Duration::from_secs(30));
                    } else {
                        restart_at = None;
                    }
                }
            }
        }

        let wait = restart_at
            .map(|at: std::time::Instant| {
                at.saturating_duration_since(std::time::Instant::now())
                    .min(Duration::from_millis(20))
            })
            .unwrap_or(Duration::from_millis(20));
        match rx.recv_timeout(wait) {
            Ok(Command::Start(new_root)) => {
                if let Some(old) = session.take() {
                    let _ = old.shutdown(Duration::from_secs(2));
                }
                documents.clear();
                texts.clear();
                versions.clear();
                progress.clear();
                root = Some(new_root);
                restarting = false;
                retry_delay = Duration::from_secs(1);
                restart_at = Some(std::time::Instant::now());
            }
            Ok(Command::Open(path, text, version)) => {
                texts.insert(path.clone(), text.clone());
                versions.insert(path.clone(), version);
                if let (Some(server), Some(server_root)) = (session.as_mut(), root.as_ref()) {
                    match Document::open_at_version(server, &path, &text, version) {
                        Ok(document) => {
                            documents.insert(path, document);
                            if let Some(server_root) = root.as_ref() {
                                let status = if settled {
                                    Status::Ready
                                } else {
                                    progress.values().next().map_or(
                                        Status::Indexing {
                                            task: String::new(),
                                            percentage: None,
                                        },
                                        |(task, percentage)| Status::Indexing {
                                            task: task.clone(),
                                            percentage: *percentage,
                                        },
                                    )
                                };
                                send_status(&events, server_root, status);
                            }
                        }
                        Err(error) => {
                            send_status(&events, server_root, Status::Error(error.to_string()))
                        }
                    }
                }
            }
            Ok(Command::Change(path, text, version)) => {
                texts.insert(path.clone(), text.clone());
                versions.insert(path.clone(), version);
                if let (Some(server), Some(document)) = (session.as_mut(), documents.get_mut(&path))
                {
                    if let Err(error) = document.change_at_version(server, &text, version) {
                        if let Some(server_root) = root.as_ref() {
                            send_status(&events, server_root, Status::Error(error.to_string()));
                        }
                    }
                }
            }
            Ok(Command::Save(path)) => {
                if let (Some(server), Some(document)) = (session.as_mut(), documents.get(&path)) {
                    let _ = document.save(server);
                }
            }
            Ok(Command::Close(path)) => {
                texts.remove(&path);
                versions.remove(&path);
                if let (Some(server), Some(document)) = (session.as_mut(), documents.remove(&path))
                {
                    let _ = document.close(server);
                }
                let _ = events.unbounded_send(Event::Diagnostics {
                    path,
                    version: None,
                    items: Vec::new(),
                });
            }
            Ok(Command::Request {
                id,
                path,
                version,
                method,
                params,
            }) => {
                if let (Some(server), Some(document)) = (session.as_mut(), documents.get(&path)) {
                    let wire_id = format!("ferritemcu-request-{id}");
                    let message = serde_json::json!({"jsonrpc":"2.0", "id":wire_id, "method":method.lsp(), "params":params});
                    match server.send(&message) {
                        Ok(()) => {
                            pending.insert(wire_id, (id, path, version, method));
                        }
                        Err(error) => {
                            if let Some(server_root) = root.as_ref() {
                                send_status(&events, server_root, Status::Error(error.to_string()));
                            }
                        }
                    }
                    let _ = document;
                }
            }
            Ok(Command::Stop) => {
                running = false;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => running = false,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        if let (Some(server), Some(server_root)) = (session.as_mut(), root.as_ref()) {
            match server.receive(Duration::from_millis(5)) {
                Ok(message) => {
                    if message.get("method").is_none() {
                        if let Some(id_value) = message.get("id") {
                            let key = id_value.as_str().unwrap_or_default().to_owned();
                            if let Some((id, path, version, method)) = pending.remove(&key) {
                                let error = message.get("error").map(|e| e.to_string());
                                let result = message
                                    .get("result")
                                    .cloned()
                                    .unwrap_or(serde_json::Value::Null);
                                let _ = events.unbounded_send(Event::RequestResult {
                                    id,
                                    path,
                                    version,
                                    method,
                                    result,
                                    error,
                                });
                            }
                        }
                        continue;
                    }
                    let method = message
                        .get("method")
                        .and_then(|m| m.as_str())
                        .unwrap_or_default();
                    if method == "textDocument/publishDiagnostics" {
                        let params = &message["params"];
                        let uri = params["uri"].as_str().unwrap_or_default();
                        if let Some((path, _)) = documents.iter().find(|(_, doc)| doc.uri() == uri)
                        {
                            let items = params["diagnostics"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|d| {
                                    let start = &d["range"]["start"];
                                    let end = &d["range"]["end"];
                                    Some(Diagnostic {
                                        range: (
                                            start["line"].as_u64()? as u32,
                                            start["character"].as_u64()? as u32,
                                            end["line"].as_u64()? as u32,
                                            end["character"].as_u64()? as u32,
                                        ),
                                        message: d["message"].as_str()?.to_owned(),
                                        severity: d["severity"].as_u64().unwrap_or(1) as u8,
                                    })
                                })
                                .collect();
                            let _ = events.unbounded_send(Event::Diagnostics {
                                path: path.clone(),
                                version: params["version"].as_i64(),
                                items,
                            });
                        }
                    } else if method == "$/progress" {
                        if let Some(status) = update_progress(&mut progress, &message["params"]) {
                            let status = server_health_error
                                .as_ref()
                                .map_or(status, |message| Status::Error(message.clone()));
                            settled = matches!(status, Status::Ready);
                            send_status(&events, server_root, status);
                        }
                    } else if method == "experimental/serverStatus" {
                        let status = server_status(&message["params"], &mut progress);
                        server_health_error = match &status {
                            Status::Error(message) => Some(message.clone()),
                            _ => None,
                        };
                        settled = matches!(status, Status::Ready);
                        send_status(&events, server_root, status);
                    } else if let Some(id) = message.get("id").filter(|_| !method.is_empty()) {
                        let response =
                            server_request_response(method, id, &message["params"], server_root);
                        let _ = server.send(&response);
                    }
                }
                Err(error) if error.kind() != std::io::ErrorKind::TimedOut => {
                    session = None;
                    pending.clear();
                    documents.clear();
                    progress.clear();
                    settled = false;
                    server_health_error = None;
                    restarting = true;
                    retry_delay = Duration::from_secs(1);
                    restart_at = Some(std::time::Instant::now() + Duration::from_millis(200));
                    send_status(&events, server_root, Status::Restarting);
                }
                _ => {}
            }
        }
    }
    if let Some(server) = session.take() {
        let _ = server.shutdown(Duration::from_secs(2));
    }
}

fn launch(root: &std::path::Path) -> std::io::Result<ferritemcu_lsp::Session> {
    let cache = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("FerriteMCU")
        .join("rust-analyzer");
    ferritemcu_lsp::resolve_rust_analyzer(root, &cache)
        .and_then(|binary| ferritemcu_lsp::Session::start(&binary, root, Duration::from_secs(30)))
}

fn send_status(
    events: &futures::channel::mpsc::UnboundedSender<Event>,
    root: &std::path::Path,
    status: Status,
) {
    let _ = events.unbounded_send(Event::Status {
        root: root.to_path_buf(),
        status,
    });
}

fn update_progress(
    progress: &mut HashMap<serde_json::Value, (String, Option<u32>)>,
    params: &serde_json::Value,
) -> Option<Status> {
    let token = params.get("token")?.clone();
    let value = &params["value"];
    let kind = value["kind"].as_str().unwrap_or_default();
    if kind == "end" {
        progress.remove(&token);
        return Some(progress_status(progress));
    }
    let raw_token = params["token"].as_str().unwrap_or_default();
    let token_task = if raw_token.to_ascii_lowercase().contains("roots scanned") {
        "Roots Scanned"
    } else if raw_token.to_ascii_lowercase().contains("index") {
        "Indexing"
    } else {
        ""
    };
    let task = if !token_task.is_empty() {
        token_task.to_owned()
    } else {
        value["message"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| value["title"].as_str())
            .unwrap_or("Indexing")
            .to_owned()
    };
    let percentage = value["percentage"].as_u64().map(|n| n.min(100) as u32);
    progress.insert(token, (task.clone(), percentage));
    Some(Status::Indexing { task, percentage })
}

fn progress_status(progress: &HashMap<serde_json::Value, (String, Option<u32>)>) -> Status {
    progress
        .values()
        .max_by_key(|(task, _)| match task.as_str() {
            "Roots Scanned" => 2,
            "Indexing" => 1,
            _ => 0,
        })
        .map_or(Status::Ready, |(task, percentage)| Status::Indexing {
            task: task.clone(),
            percentage: *percentage,
        })
}

fn server_status(
    params: &serde_json::Value,
    progress: &mut HashMap<serde_json::Value, (String, Option<u32>)>,
) -> Status {
    let health = params["health"].as_str().unwrap_or_default();
    let message = params["message"].as_str().unwrap_or_default();
    if health == "error" {
        Status::Error(if message.is_empty() {
            "rust-analyzer reported an internal error".into()
        } else {
            message.to_owned()
        })
    } else if health == "ok" && params["quiescent"].as_bool() == Some(true) {
        progress.clear();
        Status::Ready
    } else {
        Status::Indexing {
            task: if message.is_empty() {
                "Indexing".into()
            } else {
                message.to_owned()
            },
            percentage: None,
        }
    }
}

fn server_request_response(
    method: &str,
    id: &serde_json::Value,
    params: &serde_json::Value,
    root: &std::path::Path,
) -> serde_json::Value {
    let result = match method {
        "workspace/configuration" => serde_json::Value::Array(
            params["items"]
                .as_array()
                .map(|items| vec![serde_json::Value::Null; items.len()])
                .unwrap_or_default(),
        ),
        "workspace/workspaceFolders" => ferritemcu_lsp::path_to_uri(root)
            .ok()
            .map(|uri| {
                serde_json::json!([{
                    "uri": uri,
                    "name": root.file_name().unwrap_or_default().to_string_lossy()
                }])
            })
            .unwrap_or_else(|| serde_json::json!([])),
        "window/workDoneProgress/create"
        | "client/registerCapability"
        | "client/unregisterCapability"
        | "window/showMessageRequest" => serde_json::Value::Null,
        _ => {
            return serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("Method not found: {method}")}
            });
        }
    };
    serde_json::json!({"jsonrpc":"2.0", "id":id, "result":result})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_analyzer_progress_tokens_are_presented_with_task_and_percent() {
        let mut progress = HashMap::new();
        let status = update_progress(
            &mut progress,
            &serde_json::json!({
                "token": "rustAnalyzer/Indexing",
                "value": {"kind": "begin", "title": "Indexing", "percentage": 3}
            }),
        );
        assert_eq!(
            status,
            Some(Status::Indexing {
                task: "Indexing".into(),
                percentage: Some(3)
            })
        );

        let status = update_progress(
            &mut progress,
            &serde_json::json!({
                "token": "rustAnalyzer/Roots Scanned",
                "value": {"kind": "report", "percentage": 45}
            }),
        );
        assert_eq!(
            status,
            Some(Status::Indexing {
                task: "Roots Scanned".into(),
                percentage: Some(45)
            })
        );
        assert_eq!(
            status.expect("progress state").label(),
            "Indexing 45% · Roots Scanned"
        );
    }

    #[test]
    fn ending_the_last_progress_task_marks_the_server_ready() {
        let mut progress = HashMap::from([(
            serde_json::Value::String("rustAnalyzer/Indexing".into()),
            (String::from("Indexing"), Some(100)),
        )]);
        let status = update_progress(
            &mut progress,
            &serde_json::json!({"token": "rustAnalyzer/Indexing", "value": {"kind": "end"}}),
        );
        assert_eq!(status, Some(Status::Ready));
        assert!(progress.is_empty());
    }

    #[test]
    fn numeric_and_string_progress_tokens_are_distinct_and_end_independently() {
        let mut progress = HashMap::new();
        update_progress(
            &mut progress,
            &serde_json::json!({"token":"42", "value":{"kind":"begin", "title":"String task"}}),
        );
        update_progress(
            &mut progress,
            &serde_json::json!({"token":42, "value":{"kind":"begin", "title":"Integer task"}}),
        );
        assert_eq!(progress.len(), 2);
        let status = update_progress(
            &mut progress,
            &serde_json::json!({"token":"42", "value":{"kind":"end"}}),
        );
        assert_eq!(
            status,
            Some(Status::Indexing {
                task: "Integer task".into(),
                percentage: None
            })
        );
        assert_eq!(progress.len(), 1);
        assert_eq!(
            update_progress(
                &mut progress,
                &serde_json::json!({"token":42, "value":{"kind":"end"}})
            ),
            Some(Status::Ready)
        );
        assert!(progress.is_empty());
    }

    #[test]
    fn server_requests_get_typed_responses_and_unknown_methods_get_method_not_found() {
        let root = std::env::current_dir().expect("workspace path");
        let config = server_request_response(
            "workspace/configuration",
            &serde_json::json!(3),
            &serde_json::json!({"items":[{}, {}, {}]}),
            &root,
        );
        assert_eq!(config["result"], serde_json::json!([null, null, null]));
        let folders = server_request_response(
            "workspace/workspaceFolders",
            &serde_json::json!(4),
            &serde_json::Value::Null,
            &root,
        );
        assert_eq!(folders["result"].as_array().map(Vec::len), Some(1));
        let unknown = server_request_response(
            "workspace/unknownMethod",
            &serde_json::json!(5),
            &serde_json::Value::Null,
            &root,
        );
        assert_eq!(unknown["error"]["code"], -32601);

        for method in [
            "window/workDoneProgress/create",
            "client/registerCapability",
            "client/unregisterCapability",
            "window/showMessageRequest",
        ] {
            let response = server_request_response(
                method,
                &serde_json::json!(6),
                &serde_json::Value::Null,
                &root,
            );
            assert_eq!(response["result"], serde_json::Value::Null, "{method}");
            assert!(response.get("error").is_none(), "{method}");
        }
    }

    #[test]
    fn server_status_notification_overrides_stale_progress_when_quiescent() {
        let mut progress = HashMap::from([(
            serde_json::Value::String("index".into()),
            ("Indexing".into(), Some(99)),
        )]);
        assert_eq!(
            server_status(
                &serde_json::json!({"health":"ok", "quiescent":true}),
                &mut progress,
            ),
            Status::Ready
        );
        assert!(progress.is_empty());
        assert_eq!(
            server_status(
                &serde_json::json!({"health":"error", "quiescent":false, "message":"cargo metadata failed"}),
                &mut progress,
            ),
            Status::Error("cargo metadata failed".into())
        );
    }
}
