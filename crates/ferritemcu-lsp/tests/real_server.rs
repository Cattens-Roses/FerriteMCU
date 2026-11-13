use std::path::Path;
use std::time::Duration;

fn remove_fixture(root: &Path) {
    for _ in 0..50 {
        match std::fs::remove_dir_all(root) {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

fn reply_to_server_request(
    session: &mut ferritemcu_lsp::Session,
    root: &Path,
    message: &serde_json::Value,
) {
    let Some(id) = message
        .get("id")
        .filter(|_| message.get("method").is_some())
    else {
        return;
    };
    let method = message["method"].as_str().unwrap_or_default();
    let params = &message["params"];
    let result = match method {
        "workspace/configuration" => serde_json::Value::Array(
            params["items"]
                .as_array()
                .map(|items| vec![serde_json::Value::Null; items.len()])
                .unwrap_or_default(),
        ),
        "workspace/workspaceFolders" => {
            serde_json::json!([{"uri":ferritemcu_lsp::path_to_uri(root).unwrap(),"name":root.file_name().unwrap_or_default().to_string_lossy()}])
        }
        "window/workDoneProgress/create"
        | "client/registerCapability"
        | "client/unregisterCapability"
        | "window/showMessageRequest" => serde_json::Value::Null,
        other => {
            session.send(&serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Method not found: {other}")}})).unwrap();
            return;
        }
    };
    session
        .send(&serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}))
        .unwrap();
}

#[test]
#[ignore = "requires FERRITEMCU_TEST_RA pointing to an installed rust-analyzer"]
fn real_rust_analyzer_handshake() {
    let Some(binary) = std::env::var_os("FERRITEMCU_TEST_RA") else {
        return;
    };
    let root =
        std::env::temp_dir().join(format!("ferritemcu-lsp-handshake-{}", std::process::id()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"lsp_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn answer() -> u32 { 42 }\n").unwrap();
    let session =
        ferritemcu_lsp::Session::start(Path::new(&binary), &root, Duration::from_secs(30)).unwrap();
    session.shutdown(Duration::from_secs(15)).unwrap();
    remove_fixture(&root);
}

#[test]
#[ignore = "downloads an official rust-analyzer release"]
fn official_download_starts() {
    if std::env::var_os("FERRITEMCU_TEST_DOWNLOAD").is_none() {
        return;
    }
    let root = std::env::temp_dir().join(format!("ferritemcu-lsp-download-{}", std::process::id()));
    std::fs::create_dir_all(root.join("project/src")).unwrap();
    std::fs::write(
        root.join("project/Cargo.toml"),
        "[package]\nname = \"lsp_download_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("project/src/lib.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )
    .unwrap();
    let binary = ferritemcu_lsp::install::install(&root.join("server")).unwrap();
    let session =
        ferritemcu_lsp::Session::start(&binary, &root.join("project"), Duration::from_secs(30))
            .unwrap();
    session.shutdown(Duration::from_secs(15)).unwrap();
    remove_fixture(&root);
}

#[test]
#[ignore = "requires FERRITEMCU_TEST_RA pointing to an installed rust-analyzer"]
fn real_server_receives_document_changes() {
    let Some(binary) = std::env::var_os("FERRITEMCU_TEST_RA") else {
        return;
    };
    let root = std::env::temp_dir().join(format!("ferritemcu-lsp-sync-{}", std::process::id()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"sync_fixture\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
    )
    .unwrap();
    let path = root.join("src/lib.rs");
    std::fs::write(&path, "pub fn answer() -> u32 { 42 }\n").unwrap();
    let mut session =
        ferritemcu_lsp::Session::start(Path::new(&binary), &root, Duration::from_secs(30)).unwrap();
    let mut doc = ferritemcu_lsp::Document::open_at_version(
        &mut session,
        &path,
        "pub fn answer() -> u32 { 42 }\n",
        7,
    )
    .unwrap();
    doc.change_at_version(
        &mut session,
        "pub fn changed() -> u32 { let text = \"😀\"; missing_value }\n",
        9,
    )
    .unwrap();
    assert_eq!(doc.version(), 9);
    session.send(&serde_json::json!({"jsonrpc":"2.0","id": 100, "method":"textDocument/documentSymbol", "params":{"textDocument":{"uri":doc.uri()}}})).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut response = None;
    while std::time::Instant::now() < deadline {
        let message = session
            .receive(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap();
        if message.get("id") == Some(&serde_json::json!(100)) && message.get("method").is_none() {
            response = Some(message);
            break;
        }
        if let Some(id) = message
            .get("id")
            .filter(|_| message.get("method").is_some())
        {
            let result = if message["method"] == "workspace/configuration" {
                serde_json::json!(message["params"]["items"]
                    .as_array()
                    .map(|items| vec![serde_json::Value::Null; items.len()])
                    .unwrap_or_default())
            } else {
                serde_json::Value::Null
            };
            session
                .send(&serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}))
                .unwrap();
        }
    }
    let response = response.expect("documentSymbol response");
    assert!(response.get("error").is_none(), "{response}");
    assert!(
        response["result"].to_string().contains("changed"),
        "{response}"
    );
    doc.save(&mut session).unwrap();
    doc.close(&mut session).unwrap();
    session.shutdown(Duration::from_secs(15)).unwrap();
    remove_fixture(&root);
}

#[test]
#[ignore = "requires FERRITEMCU_TEST_RA pointing to an installed rust-analyzer"]
fn real_server_editor_features() {
    let binary = std::env::var_os("FERRITEMCU_TEST_RA")
        .expect("FERRITEMCU_TEST_RA must point to rust-analyzer");
    let root = std::env::temp_dir().join(format!("ferritemcu-lsp-features-{}", std::process::id()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"lsp_features\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    let path = root.join("src/lib.rs");
    let source = "pub fn meaning( ) -> u32{42}\npub fn caller( )->u32 { meaning( ) }\n";
    std::fs::write(&path, source).unwrap();
    let mut session =
        ferritemcu_lsp::Session::start(Path::new(&binary), &root, Duration::from_secs(45)).unwrap();
    let doc = ferritemcu_lsp::Document::open(&mut session, &path, source).unwrap();
    // Give Cargo metadata and the fixture's initial analysis time to settle before
    // asserting the semantic feature responses (LSP permits null during indexing).
    std::thread::sleep(Duration::from_secs(8));

    fn request(
        session: &mut ferritemcu_lsp::Session,
        root: &Path,
        id: u64,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        session
            .send(&serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        loop {
            let message = session
                .receive(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("rust-analyzer response timeout");
            if std::env::var_os("FERRITEMCU_LSP_TRACE").is_some()
                && message["method"] == "experimental/serverStatus"
            {
                eprintln!("rust-analyzer status: {}", message["params"]);
            }
            if message.get("id") == Some(&serde_json::json!(id)) && message.get("method").is_none()
            {
                return message;
            }
            reply_to_server_request(session, root, &message);
        }
    }
    let uri = ferritemcu_lsp::path_to_uri(&path).unwrap();
    let completion = request(
        &mut session,
        &root,
        21,
        "textDocument/completion",
        serde_json::json!({"textDocument":{"uri":uri},"position":{"line":1,"character":25},"context":{"triggerKind":1}}),
    );
    assert!(
        completion.get("error").is_none(),
        "completion: {completion}"
    );
    let completion = &completion["result"];
    assert!(
        completion.is_null() || completion.is_array() || completion["items"].is_array(),
        "completion shape: {completion}"
    );
    let hover = request(
        &mut session,
        &root,
        60,
        "textDocument/hover",
        serde_json::json!({"textDocument":{"uri":uri},"position":{"line":0,"character":9}}),
    );
    assert!(hover.get("error").is_none(), "hover: {hover}");
    assert!(hover["result"].is_object(), "hover result: {hover}");
    let definition = request(
        &mut session,
        &root,
        61,
        "textDocument/definition",
        serde_json::json!({"textDocument":{"uri":uri},"position":{"line":1,"character":25}}),
    );
    assert!(
        definition.get("error").is_none(),
        "definition: {definition}"
    );
    assert!(
        !definition["result"].is_null(),
        "definition result: {definition}"
    );
    let formatting = request(
        &mut session,
        &root,
        62,
        "textDocument/formatting",
        serde_json::json!({"textDocument":{"uri":uri},"options":{"tabSize":4,"insertSpaces":true}}),
    );
    assert!(
        formatting.get("error").is_none(),
        "formatting: {formatting}"
    );
    assert!(
        formatting["result"].is_array(),
        "formatting result: {formatting}"
    );
    doc.close(&mut session).unwrap();
    session.shutdown(Duration::from_secs(15)).unwrap();
    remove_fixture(&root);
}

#[test]
#[ignore = "diagnostic trace: requires FERRITEMCU_TEST_RA and FERRITEMCU_LSP_TRACE"]
fn trace_real_workspace_progress_and_server_status() {
    use std::collections::HashMap;

    let Some(binary) = std::env::var_os("FERRITEMCU_TEST_RA") else {
        panic!("FERRITEMCU_TEST_RA must point to rust-analyzer");
    };
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .ancestors()
        .find(|path| path.join("Cargo.toml").is_file() && path.join("src/main.rs").is_file())
        .expect("FerriteMCU workspace root")
        .canonicalize()
        .unwrap();
    let file = root.join("src/main.rs");
    let text = std::fs::read_to_string(&file).unwrap();
    let mut session =
        ferritemcu_lsp::Session::start(Path::new(&binary), &root, Duration::from_secs(60)).unwrap();
    let document = ferritemcu_lsp::Document::open(&mut session, &file, &text).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    let mut progress: HashMap<serde_json::Value, String> = HashMap::new();
    let mut progress_ends = 0usize;
    let mut server_statuses = Vec::new();
    let mut quiescent_at = None;
    while std::time::Instant::now() < deadline {
        if quiescent_at.is_some_and(|at: std::time::Instant| at.elapsed() >= Duration::from_secs(2))
        {
            break;
        }
        let message = match session.receive(Duration::from_secs(1)) {
            Ok(message) => message,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(error) => panic!("rust-analyzer trace receive failed: {error}"),
        };
        match message["method"].as_str().unwrap_or_default() {
            "$/progress" => {
                let token = message["params"]["token"].clone();
                let value = &message["params"]["value"];
                match value["kind"].as_str().unwrap_or_default() {
                    "end" => {
                        progress.remove(&token);
                        progress_ends += 1;
                    }
                    _ => {
                        progress.insert(
                            token,
                            value["message"]
                                .as_str()
                                .or_else(|| value["title"].as_str())
                                .unwrap_or("<unnamed>")
                                .to_owned(),
                        );
                    }
                }
            }
            "experimental/serverStatus" => {
                let params = &message["params"];
                let health = params["health"].as_str().unwrap_or_default().to_owned();
                let quiescent = params["quiescent"].as_bool().unwrap_or(false);
                let detail = params["message"].as_str().unwrap_or_default().to_owned();
                server_statuses.push((health, quiescent, detail));
                if quiescent {
                    quiescent_at = Some(std::time::Instant::now());
                }
            }
            method if !method.is_empty() && message.get("id").is_some() => {
                let id = &message["id"];
                let params = &message["params"];
                let result = match method {
                    "workspace/configuration" => serde_json::Value::Array(
                        params["items"]
                            .as_array()
                            .map(|items| vec![serde_json::Value::Null; items.len()])
                            .unwrap_or_default(),
                    ),
                    "workspace/workspaceFolders" => serde_json::json!([{
                        "uri": ferritemcu_lsp::path_to_uri(&root).unwrap(),
                        "name": root.file_name().unwrap_or_default().to_string_lossy()
                    }]),
                    "window/workDoneProgress/create"
                    | "client/registerCapability"
                    | "client/unregisterCapability"
                    | "window/showMessageRequest" => serde_json::Value::Null,
                    _ => {
                        session
                            .send(&serde_json::json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32601,"message":format!("Method not found: {method}")}}))
                            .unwrap();
                        continue;
                    }
                };
                session
                    .send(&serde_json::json!({"jsonrpc":"2.0", "id":id, "result":result}))
                    .unwrap();
            }
            _ => {}
        }
    }
    eprintln!(
        "RA trace summary: progress_ends={progress_ends}, active_progress={progress:?}, server_statuses={server_statuses:?}"
    );
    assert!(
        server_statuses
            .iter()
            .all(|(_, _, detail)| !detail.contains("unexpected variable")),
        "Cargo metadata still received a brace-containing temp path: {server_statuses:?}"
    );
    document.close(&mut session).unwrap();
    session.shutdown(Duration::from_secs(15)).unwrap();
}
