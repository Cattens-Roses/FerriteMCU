//! Acquisition of the official rust-analyzer server; all operations are blocking.
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const API: &str = "https://api.github.com/repos/rust-lang/rust-analyzer/releases/latest";
const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;
const MAX_BINARY: u64 = 256 * 1024 * 1024;

fn asset_name() -> io::Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok("rust-analyzer-x86_64-pc-windows-msvc.zip"),
        ("windows", "aarch64") => Ok("rust-analyzer-aarch64-pc-windows-msvc.zip"),
        ("macos", "x86_64") => Ok("rust-analyzer-x86_64-apple-darwin.gz"),
        ("macos", "aarch64") => Ok("rust-analyzer-aarch64-apple-darwin.gz"),
        ("linux", "x86_64") => Ok("rust-analyzer-x86_64-unknown-linux-gnu.gz"),
        ("linux", "aarch64") => Ok("rust-analyzer-aarch64-unknown-linux-gnu.gz"),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unsupported rust-analyzer platform",
        )),
    }
}

fn fetch(agent: &ureq::Agent, url: &str, max: u64) -> io::Result<Vec<u8>> {
    let response = agent
        .get(url)
        .header("User-Agent", "FerriteMCU")
        .call()
        .map_err(io::Error::other)?;
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(max + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "download exceeds size limit",
        ));
    }
    Ok(bytes)
}

/// Download and install the latest official stable server into an application-owned directory.
/// The GitHub release digest is checked before decompression. Call on a worker thread.
pub fn install(cache: &Path) -> io::Result<PathBuf> {
    let asset = asset_name()?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .build()
        .into();
    let metadata: serde_json::Value =
        serde_json::from_slice(&fetch(&agent, API, 2 * 1024 * 1024)?).map_err(io::Error::other)?;
    let entry = metadata["assets"]
        .as_array()
        .and_then(|assets| assets.iter().find(|a| a["name"].as_str() == Some(asset)))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "official release has no asset for this platform",
            )
        })?;
    let download = entry["browser_download_url"]
        .as_str()
        .ok_or_else(|| io::Error::other("missing release download URL"))?;
    let parsed = url::Url::parse(download).map_err(io::Error::other)?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("github.com")
        || !parsed
            .path()
            .starts_with("/rust-lang/rust-analyzer/releases/download/")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid rust-analyzer download URL",
        ));
    }
    let expected = entry["digest"]
        .as_str()
        .and_then(|v| v.strip_prefix("sha256:"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "official release is missing its SHA-256 digest",
            )
        })?;
    let archive = fetch(&agent, download, MAX_ARCHIVE)?;
    let actual = format!("{:x}", Sha256::digest(&archive));
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "rust-analyzer archive checksum mismatch",
        ));
    }
    let mut binary = Vec::new();
    if asset.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(io::Cursor::new(archive)).map_err(io::Error::other)?;
        let member = zip.by_name("rust-analyzer.exe").map_err(io::Error::other)?;
        member.take(MAX_BINARY + 1).read_to_end(&mut binary)?;
    } else {
        flate2::read::GzDecoder::new(&archive[..])
            .take(MAX_BINARY + 1)
            .read_to_end(&mut binary)?;
    }
    if binary.is_empty() || binary.len() as u64 > MAX_BINARY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid rust-analyzer binary size",
        ));
    }
    std::fs::create_dir_all(cache)?;
    let target = cache.join(if cfg!(windows) {
        "rust-analyzer.exe"
    } else {
        "rust-analyzer"
    });
    let temp = cache.join(format!(
        "rust-analyzer-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&binary)?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
        }
        drop(file);
        // The destination is installed only after the complete verified file is ready.
        std::fs::rename(&temp, &target)?;
        Ok(target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}
