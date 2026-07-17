//! Knowing about, and moving to, newer releases.
//!
//! Three jobs, all against this project's GitHub releases:
//!
//! * tell `--version` whether it is behind, without making `--version` slow;
//! * replace the running binary with the latest one (`logscout upgrade`);
//! * take the binary back off the machine (`logscout uninstall`).
//!
//! The update check is cached in `~/.log-scouter/update.json` and refreshed at most daily.
//! `--version` is called by scripts, and blocking every one of them on api.github.com --
//! and burning the 60/hour unauthenticated rate limit doing it -- would be a poor trade for
//! a notice that changes at most once a release.

use crate::core::filters::{home_dir, USER_DIR};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The repo releases are published from.
pub const RELEASE_REPO: &str = "mangosteen-lab/log-scouter";

/// The version compiled into this binary.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long a "latest is X" answer is good for.
pub const UPDATE_CHECK_TTL: chrono::Duration = chrono::Duration::hours(24);

/// Setting this to anything non-empty stops the update check reaching the network.
pub const NO_UPDATE_CHECK_VAR: &str = "LOGSCOUT_NO_UPDATE_CHECK";

/// Short: a stale notice is worth less than a fast `--version`.
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);
/// Long: a download is the thing the user asked for and is worth waiting on.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// A release binary is a few megabytes; far past that, something is wrong.
const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;

/// What the last check found, so the next `--version` need not ask again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCache {
    /// The newest version GitHub reported, without the `v`.
    pub latest: String,
    /// When it reported it, RFC 3339.
    pub checked_at: String,
}

/// `~/.log-scouter/update.json`.
pub fn update_cache_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(USER_DIR).join("update.json"))
}

impl UpdateCache {
    pub fn load() -> Option<Self> {
        let body = std::fs::read_to_string(update_cache_path()?).ok()?;
        serde_json::from_str(&body).ok()
    }

    /// Best-effort: a cache we cannot write just means the next run asks again.
    pub fn save(&self) {
        let Some(path) = update_cache_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(body) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, body);
        }
    }

    fn is_fresh(&self, now: chrono::DateTime<chrono::Local>, ttl: chrono::Duration) -> bool {
        chrono::DateTime::parse_from_rfc3339(&self.checked_at)
            .map(|checked| now.signed_duration_since(checked) < ttl)
            .unwrap_or(false)
    }
}

/// Compare two dotted numeric versions.
///
/// Hand-rolled rather than a semver dependency: these are our own tags, they are strictly
/// `MAJOR.MINOR.PATCH`, and the only question ever asked is "is theirs newer than mine".
/// A part that will not parse sorts as 0, so a malformed tag can never look like an upgrade.
pub fn is_newer(latest: &str, current: &str) -> bool {
    parts(latest) > parts(current)
}

fn parts(version: &str) -> Vec<u64> {
    version
        .trim()
        .trim_start_matches('v')
        // Ignore any `-rc1` style suffix: a pre-release is not an upgrade.
        .split(['.', '-', '+'])
        .take(3)
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

/// The release asset for the platform this binary was built for, or `None` when no release
/// is published for it.
///
/// The names match what `.github/workflows/release.yml` uploads; a target missing here is a
/// target the workflow does not build.
pub fn asset_name() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "log-scouter-x86_64-unknown-linux-musl.tar.gz",
        ("macos", "aarch64") => "log-scouter-aarch64-apple-darwin.tar.gz",
        ("macos", "x86_64") => "log-scouter-x86_64-apple-darwin.tar.gz",
        ("windows", "x86_64") => "log-scouter-x86_64-pc-windows-msvc.zip",
        _ => return None,
    })
}

/// The newest published version, without the `v`. Asks GitHub; no cache.
pub fn fetch_latest_version() -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{RELEASE_REPO}/releases/latest");
    let body = get(&url, CHECK_TIMEOUT, "application/vnd.github+json")?;
    let json: serde_json::Value =
        serde_json::from_slice(&body).map_err(|error| format!("bad response: {error}"))?;
    let tag = json
        .get("tag_name")
        .and_then(|tag| tag.as_str())
        .ok_or_else(|| "no tag_name in the release response".to_string())?;
    Ok(tag.trim_start_matches('v').to_string())
}

/// The version to tell the user about, or `None` when this build is current.
///
/// Cached: at most one request a day. `LOGSCOUT_NO_UPDATE_CHECK` and a missing home
/// directory both mean "do not ask", and every failure is silent -- a version banner is
/// never worth an error.
pub fn available_update(current: &str) -> Option<String> {
    if std::env::var(NO_UPDATE_CHECK_VAR).is_ok_and(|value| !value.trim().is_empty()) {
        return None;
    }
    let now = chrono::Local::now();
    let cached = UpdateCache::load();
    let latest = match &cached {
        Some(cache) if cache.is_fresh(now, UPDATE_CHECK_TTL) => cache.latest.clone(),
        _ => {
            let latest = fetch_latest_version().ok()?;
            UpdateCache {
                latest: latest.clone(),
                checked_at: now.to_rfc3339(),
            }
            .save();
            latest
        }
    };
    is_newer(&latest, current).then_some(latest)
}

/// The notice `--version` prints under the version, or nothing when up to date.
pub fn update_notice(current: &str) -> Option<String> {
    let latest = available_update(current)?;
    Some(format!(
        "\nA new release of logscout is available: {current} -> {latest}\n\
         https://github.com/{RELEASE_REPO}/releases/tag/v{latest}\n\
         Run `logscout upgrade` to update.",
    ))
}

/// What an upgrade did, for the caller to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upgraded {
    /// Already on the newest version.
    AlreadyCurrent(String),
    /// Replaced the binary at this path, and the version now installed.
    Replaced { path: PathBuf, version: String },
}

/// Replace the running binary with `version` (or the latest when `None`).
///
/// Targets `current_exe`, not a fixed install directory: the binary you ran is the one you
/// meant to upgrade, wherever it lives. `self_replace` does the swap, which on Windows means
/// the rename dance a running `.exe` needs, and everywhere means the old binary stays valid
/// for this process.
pub fn upgrade(
    current: &str,
    version: Option<&str>,
    progress: &mut dyn FnMut(&str),
) -> Result<Upgraded, String> {
    let target = match version {
        Some(version) => version.trim_start_matches('v').to_string(),
        None => {
            progress("checking for a newer release");
            fetch_latest_version()?
        }
    };
    // An explicit version is an instruction, not a suggestion: it may be a downgrade.
    if version.is_none() && !is_newer(&target, current) {
        return Ok(Upgraded::AlreadyCurrent(current.to_string()));
    }

    let asset = asset_name().ok_or_else(|| {
        format!(
            "no published binary for {}-{}; build from source instead",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let url = format!("https://github.com/{RELEASE_REPO}/releases/download/v{target}/{asset}");

    progress(&format!("downloading {asset}"));
    let archive = get(&url, DOWNLOAD_TIMEOUT, "application/octet-stream")
        .map_err(|error| format!("could not download v{target}: {error}"))?;

    // Unpack beside the binary being replaced rather than in a temp dir: `self_replace`
    // needs the new file on the same filesystem to swap it atomically.
    let exe = std::env::current_exe().map_err(|error| format!("no current exe: {error}"))?;
    let staging = exe.with_extension("upgrade-tmp");
    let _ = std::fs::remove_file(&staging);
    extract_binary(&archive, asset, &staging).inspect_err(|_| {
        let _ = std::fs::remove_file(&staging);
    })?;

    progress(&format!("replacing {}", exe.display()));
    let result = self_replace::self_replace(&staging)
        .map_err(|error| format!("could not replace {}: {error}", exe.display()));
    let _ = std::fs::remove_file(&staging);
    result?;

    Ok(Upgraded::Replaced {
        path: exe,
        version: target,
    })
}

/// Pull the `logscout` binary out of a release archive and write it to `dest`, executable.
fn extract_binary(archive: &[u8], asset: &str, dest: &Path) -> Result<(), String> {
    if asset.ends_with(".zip") {
        extract_zip(archive, dest)?;
    } else {
        extract_tar_gz(archive, dest)?;
    }
    if !dest.exists() {
        return Err(format!("no logscout binary inside {asset}"));
    }
    set_executable(dest);
    Ok(())
}

fn extract_tar_gz(archive: &[u8], dest: &Path) -> Result<(), String> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    let entries = tar
        .entries()
        .map_err(|error| format!("not a gzipped tarball: {error}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|error| format!("corrupt archive: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("corrupt archive: {error}"))?
            .into_owned();
        if is_binary_entry(&path) {
            // Unpack by hand to a known path: the archive's own layout must not decide
            // where anything lands.
            let mut out = std::fs::File::create(dest)
                .map_err(|error| format!("{}: {error}", dest.display()))?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|error| format!("{}: {error}", dest.display()))?;
            return Ok(());
        }
    }
    Err("no logscout binary in the archive".to_string())
}

#[cfg(windows)]
fn extract_zip(archive: &[u8], dest: &Path) -> Result<(), String> {
    let reader = std::io::Cursor::new(archive);
    let mut zip = zip::ZipArchive::new(reader).map_err(|error| format!("not a zip: {error}"))?;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| format!("corrupt archive: {error}"))?;
        let Some(path) = entry.enclosed_name() else {
            continue;
        };
        if is_binary_entry(&path) {
            let mut out = std::fs::File::create(dest)
                .map_err(|error| format!("{}: {error}", dest.display()))?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|error| format!("{}: {error}", dest.display()))?;
            return Ok(());
        }
    }
    Err("no logscout.exe in the archive".to_string())
}

#[cfg(not(windows))]
fn extract_zip(_archive: &[u8], _dest: &Path) -> Result<(), String> {
    // Only the Windows release is a zip, and only a Windows build asks for one.
    Err("zip release assets are only used on Windows".to_string())
}

/// Whether a path inside a release archive is the binary we want, wherever it sits.
fn is_binary_entry(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("logscout") | Some("logscout.exe")
    )
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

/// What an uninstall removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Uninstalled {
    pub binary: Option<PathBuf>,
    pub library: Option<PathBuf>,
}

/// The user-level library an uninstall may purge.
pub fn user_library_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join(USER_DIR))
}

/// Remove the running binary, and with `purge` the user-level library too.
///
/// Project-local `.logscouter` folders are never touched: they belong to the log folders,
/// not to the install, and removing the tool is not a reason to lose a project's filters.
pub fn uninstall(purge: bool) -> Result<Uninstalled, String> {
    let exe = std::env::current_exe().map_err(|error| format!("no current exe: {error}"))?;
    let mut removed = Uninstalled::default();

    // `self_delete` handles the platforms where a running binary cannot simply be unlinked.
    self_replace::self_delete()
        .map_err(|error| format!("could not remove {}: {error}", exe.display()))?;
    removed.binary = Some(exe);

    if purge {
        if let Some(dir) = user_library_dir() {
            if dir.is_dir() {
                std::fs::remove_dir_all(&dir)
                    .map_err(|error| format!("could not remove {}: {error}", dir.display()))?;
                removed.library = Some(dir);
            }
        }
    }
    Ok(removed)
}

/// One blocking GET on a private runtime, returning the body.
fn get(url: &str, timeout: Duration, accept: &str) -> Result<Vec<u8>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start async runtime: {error}"))?;

    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| format!("could not build http client: {error}"))?;
        let mut request = client
            .get(url)
            // GitHub's API rejects requests without one.
            .header("User-Agent", format!("logscout/{CURRENT_VERSION}"))
            .header("Accept", accept);
        // The same token the hubs use, for anyone behind a rate limit or a private mirror.
        if let Some(token) = ["LOGSCOUT_HUB_TOKEN", "GITHUB_TOKEN"]
            .iter()
            .find_map(|key| std::env::var(key).ok())
            .filter(|value| !value.trim().is_empty())
        {
            request = request.header("Authorization", format!("Bearer {token}"));
        }

        let response = request
            .send()
            .await
            .map_err(|error| format!("fetch failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(match status.as_u16() {
                404 => format!("not found: {url}"),
                403 => "rate limited by GitHub; try again later or set GITHUB_TOKEN".to_string(),
                _ => format!("fetch failed: {status}"),
            });
        }
        if response.content_length().unwrap_or(0) > MAX_ASSET_BYTES {
            return Err("refusing an implausibly large download".to_string());
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("download failed: {error}"))?;
        if bytes.len() as u64 > MAX_ASSET_BYTES {
            return Err("refusing an implausibly large download".to_string());
        }
        Ok(bytes.to_vec())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions_are_recognised() {
        assert!(is_newer("0.0.17", "0.0.16"));
        assert!(is_newer("0.1.0", "0.0.99"));
        assert!(is_newer("1.0.0", "0.9.9"));
        // The `v` prefix on a tag is noise.
        assert!(is_newer("v0.0.17", "0.0.16"));

        assert!(!is_newer("0.0.16", "0.0.16"));
        assert!(!is_newer("0.0.15", "0.0.16"));
        assert!(!is_newer("0.0.9", "0.0.10"), "compare numbers, not strings");
    }

    /// A tag we cannot read must never look like an upgrade: telling someone to upgrade to
    /// something that does not exist is worse than saying nothing.
    #[test]
    fn unreadable_versions_are_not_upgrades() {
        for junk in ["", "latest", "not-a-version", "v", "..."] {
            assert!(!is_newer(junk, "0.0.16"), "{junk:?} is not an upgrade");
        }
        // A pre-release of the version we are on is not an upgrade either.
        assert!(!is_newer("0.0.16-rc1", "0.0.16"));
    }

    #[test]
    fn the_cache_goes_stale_after_the_ttl() {
        let now = chrono::Local::now();
        let fresh = UpdateCache {
            latest: "0.0.17".into(),
            checked_at: (now - chrono::Duration::hours(2)).to_rfc3339(),
        };
        let stale = UpdateCache {
            latest: "0.0.17".into(),
            checked_at: (now - chrono::Duration::hours(30)).to_rfc3339(),
        };
        assert!(fresh.is_fresh(now, UPDATE_CHECK_TTL));
        assert!(!stale.is_fresh(now, UPDATE_CHECK_TTL));

        // A stamp we cannot read counts as stale, so a corrupt cache re-checks rather than
        // pinning the answer forever.
        let broken = UpdateCache {
            latest: "0.0.17".into(),
            checked_at: "last tuesday".into(),
        };
        assert!(!broken.is_fresh(now, UPDATE_CHECK_TTL));
    }

    #[test]
    fn the_asset_matches_what_the_release_workflow_builds() {
        // Whatever this test runs on must be a target we publish, or `upgrade` cannot work.
        let asset = asset_name().expect("a published asset for this platform");
        assert!(asset.starts_with("log-scouter-"));
        assert!(asset.ends_with(".tar.gz") || asset.ends_with(".zip"));
    }

    #[test]
    fn the_binary_is_found_wherever_the_archive_puts_it() {
        assert!(is_binary_entry(Path::new("logscout")));
        assert!(is_binary_entry(Path::new("log-scouter-x86_64/logscout")));
        assert!(is_binary_entry(Path::new("logscout.exe")));
        assert!(!is_binary_entry(Path::new("README.md")));
        assert!(!is_binary_entry(Path::new("logscout.txt")));
    }

    /// The whole unpack path, against a tarball shaped like a release asset.
    #[test]
    fn a_release_tarball_yields_an_executable_binary() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("logscout-new");

        let mut builder = tar::Builder::new(Vec::new());
        for (path, body) in [
            ("log-scouter-x86_64/README.md", "not the binary"),
            ("log-scouter-x86_64/logscout", "#!/bin/sh\necho hi\n"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, path, body.as_bytes())
                .unwrap();
        }
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &builder.into_inner().unwrap()).unwrap();
        let archive = encoder.finish().unwrap();

        extract_binary(
            &archive,
            "log-scouter-x86_64-unknown-linux-musl.tar.gz",
            &dest,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "#!/bin/sh\necho hi\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "the new binary is executable");
        }
    }

    /// An archive with no binary in it is an error, not a zero-byte `logscout`.
    #[test]
    fn an_archive_without_a_binary_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("logscout-new");

        let mut builder = tar::Builder::new(Vec::new());
        let body = b"nothing useful";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, "junk/README.md", &body[..])
            .unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &builder.into_inner().unwrap()).unwrap();
        let archive = encoder.finish().unwrap();

        let error = extract_binary(&archive, "x.tar.gz", &dest).unwrap_err();
        assert!(error.contains("no logscout binary"), "{error}");
        assert!(!dest.exists());
    }
}
