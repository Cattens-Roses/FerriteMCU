# LSP Editor Integration Plan

- [x] Add per-document path, version, last-synced text, and diagnostics state to the main editor model.
- [x] Synchronize every Rust tab with didOpen/didChange/didSave/didClose; close/open on active-tab switches and discard stale diagnostic versions.
- [x] Render current diagnostics as editor underlines and show the diagnostic message on hover.
- [x] Connect completion (Ctrl+Space, click or Enter to apply), go to definition (F12), hover, and formatting (Alt+Shift+F) through versioned LSP requests.
- [x] Verify formatting, targeted tests, a real rust-analyzer session, and the application build.
- [x] On Windows, isolate rust-analyzer's TEMP/TMP from sandbox paths containing Cargo-invalid `{}` interpolation; use a unique app-data temp directory with a project `target` fallback and clean it up with the session.
