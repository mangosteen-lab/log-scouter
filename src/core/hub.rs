//! Hubs: remote libraries of schemas, filters and saved searches.
//!
//! A hub is an ordinary git repo -- on GitHub, or a self-hosted GitLab or Gitea -- laid out
//! like the user-level library:
//!
//! ```text
//! <repo>/
//!   schemas/*.json
//!   filters/*.json
//!   searches/*.json
//! ```
//!
//! Configured hubs live in `~/.log-scouter/hubs.json`; syncing one downloads the repo
//! tarball over HTTP(S) and unpacks those three folders into `~/.log-scouter/hubs/<name>/`.
//! Each forge serves that tarball from its own URL and wants its token in its own header
//! (`Forge`); a host we do not recognise is identified once, by asking it, and remembered.
//! That cache is a *read-only tier of its own*, below the project and the user's own
//! `~/.log-scouter/{schemas,filters,searches}` -- sync never writes into the folders the
//! user hand-maintains, so re-syncing or removing a hub cannot destroy their work.
//!
//! Every item a hub provides is namespaced `<hub>/<name>`, so two hubs shipping a
//! `Gateway-Access` schema both stay visible and usable rather than one silently
//! shadowing the other. Precedence still decides which schema *detects* a log first:
//! project, then user, then hubs in configured order, then bundled.
//!
//! The **official hub** (`OFFICIAL_HUB_REPO`) is the exception: it publishes the same
//! schemas the binary bundles, so its items keep their bare names and shadow the bundled
//! copies from one tier up. The bundle is the offline floor -- it works on a first run with
//! no network -- and the hub is the update channel, which is why a fix there reaches users
//! without a release, and why a `project.json` naming `Spring Boot` keeps resolving.
//! Every install is configured with it (`ensure_official`) and refreshes it in the
//! background at most once a day (`due_for_auto_sync`), unless the user turns that off.

use crate::core::extractor::{load_schemas_from_folder, SchemaFile, USER_SCHEMAS_SUBDIR};
use crate::core::filters::{
    home_dir, load_filters_from_folder, sanitize_file_component, FilterFile, USER_DIR,
    USER_FILTERS_SUBDIR,
};
use crate::core::search::{load_searches_from_folder, SearchFile, USER_SEARCHES_SUBDIR};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `~/.log-scouter/hubs` -- one subfolder per hub, each a snapshot of the remote repo.
pub const HUBS_SUBDIR: &str = "hubs";
/// `~/.log-scouter/hubs.json` -- the configured hub list.
pub const HUBS_FILE: &str = "hubs.json";

/// The folders a hub can publish. Anything else in the repo (README, CI, ...) is ignored.
pub const HUB_FOLDERS: &[&str] = &[
    USER_SCHEMAS_SUBDIR,
    USER_FILTERS_SUBDIR,
    USER_SEARCHES_SUBDIR,
];

/// Give up rather than freeze the UI when a host stops responding mid-download.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// A tarball this big is not a hub. Guards against a hostile or wrong URL filling the disk.
const MAX_TARBALL_BYTES: u64 = 32 * 1024 * 1024;

/// The hub every install is configured with, and the repo it tracks.
pub const OFFICIAL_HUB_NAME: &str = "official";
pub const OFFICIAL_HUB_REPO: &str = "mangosteen-lab/log-scouter-hub";

/// How stale the official hub's cache may get before a start refreshes it. A day keeps
/// launches quiet and well inside GitHub's unauthenticated rate limit.
pub const AUTO_SYNC_TTL: chrono::Duration = chrono::Duration::hours(24);

/// Setting this to anything non-empty stops log-scouter from touching the network on start.
pub const NO_AUTO_SYNC_VAR: &str = "LOGSCOUT_NO_HUB_SYNC";

/// One configured remote hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hub {
    /// Local, unique, and the namespace its items appear under. Defaults to the repo name.
    pub name: String,
    /// The remote, as the user typed it (`owner/repo`, an HTTPS URL, or an SSH URL).
    pub url: String,
    /// Branch to track. `None` means the repo's default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// A disabled hub stays configured and cached but contributes nothing.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// When the cache was last refreshed, RFC 3339. `None` until the first sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced: Option<String>,
    /// The first-party hub: its items are *not* namespaced.
    ///
    /// This hub publishes the same schemas the binary bundles, so its `Spring Boot` is
    /// meant to *be* `Spring Boot` -- shadowing the bundled copy from one tier up rather
    /// than sitting next to it under a second name. That keeps a `project.json` that
    /// already references `Spring Boot` working, and lets a schema fix ship without a
    /// release. Third-party hubs stay namespaced, so they still cannot collide.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub official: bool,
    /// The kind of host this hub is on, learned on the first successful sync and kept so
    /// later syncs go straight to the right URL instead of probing again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge: Option<Forge>,
}

fn default_enabled() -> bool {
    true
}

impl Hub {
    /// The namespace this hub's items take, or `None` for the official hub's bare names.
    pub fn namespace(&self) -> Option<&str> {
        (!self.official).then_some(self.name.as_str())
    }

    /// Whether the cache is old enough to refresh. Never synced counts as stale; an
    /// unparseable `last_synced` does too, since re-syncing is the cheap way to be sure.
    pub fn is_stale(&self, ttl: chrono::Duration, now: chrono::DateTime<chrono::Local>) -> bool {
        let Some(last) = &self.last_synced else {
            return true;
        };
        match chrono::DateTime::parse_from_rfc3339(last) {
            Ok(last) => now.signed_duration_since(last) >= ttl,
            Err(_) => true,
        }
    }
}

/// `~/.log-scouter/hubs.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubConfig {
    #[serde(default)]
    pub hubs: Vec<Hub>,
    /// Refresh stale hubs in the background on start. Off means log-scouter never reaches
    /// the network unless the user asks it to.
    #[serde(default = "default_enabled")]
    pub auto_sync: bool,
    /// The user removed the official hub. A tombstone, so the next start does not helpfully
    /// add back the thing they just deleted. `add`ing it again clears it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub official_removed: bool,
}

/// Hand-written rather than derived: a derived `Default` would leave `auto_sync` false, and
/// `load()` returns the default when `hubs.json` does not exist yet -- which is exactly the
/// first run that most needs the official hub.
impl Default for HubConfig {
    fn default() -> Self {
        Self {
            hubs: Vec::new(),
            auto_sync: true,
            official_removed: false,
        }
    }
}

/// The path to `hubs.json`, or `None` when no home directory can be resolved.
pub fn hub_config_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(USER_DIR).join(HUBS_FILE))
}

/// `~/.log-scouter/hubs`, the cache root.
pub fn hub_cache_root() -> Option<PathBuf> {
    home_dir().map(|home| home.join(USER_DIR).join(HUBS_SUBDIR))
}

/// Where a hub's snapshot lives.
pub fn hub_cache_dir(name: &str) -> Option<PathBuf> {
    hub_cache_root().map(|root| root.join(sanitize_file_component(name)))
}

impl HubConfig {
    /// The configured hubs. A missing file is an empty list; a corrupt one is an error the
    /// caller can surface rather than a silent reset of the user's hub list.
    pub fn load() -> io::Result<Self> {
        let Some(path) = hub_config_path() else {
            return Ok(Self::default());
        };
        match fs::read_to_string(&path) {
            Ok(body) => serde_json::from_str(&body).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: {error}", path.display()),
                )
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    /// Write via a temp file and rename, so an interrupted save cannot leave a half-written
    /// hub list behind.
    pub fn save(&self) -> io::Result<()> {
        let path = hub_config_path()
            .ok_or_else(|| io::Error::other("no home directory: cannot save hubs.json"))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, body)?;
        fs::rename(tmp, path)
    }

    pub fn get(&self, name: &str) -> Option<&Hub> {
        self.hubs.iter().find(|hub| hub.name == name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Hub> {
        self.hubs.iter_mut().find(|hub| hub.name == name)
    }

    /// Hubs that contribute items, in configured order -- which is their precedence order.
    pub fn active(&self) -> impl Iterator<Item = &Hub> {
        self.hubs.iter().filter(|hub| hub.enabled)
    }

    /// Add a hub. The name must be free: silently replacing one would repoint every item
    /// under that namespace at a different repo.
    pub fn add(&mut self, hub: Hub) -> Result<(), String> {
        if hub.name.trim().is_empty() {
            return Err("a hub needs a name".to_string());
        }
        if self.get(&hub.name).is_some() {
            return Err(format!("hub '{}' already exists", hub.name));
        }
        // Adding the official repo back is the user changing their mind: lift the tombstone.
        if hub.official || repo_is_official(&hub.url) {
            self.official_removed = false;
        }
        self.hubs.push(hub);
        Ok(())
    }

    /// Forget a hub and return it. The caller deletes its cache; nothing under
    /// `~/.log-scouter/{schemas,filters,searches}` is touched, because sync never put
    /// anything there.
    pub fn remove(&mut self, name: &str) -> Option<Hub> {
        let index = self.hubs.iter().position(|hub| hub.name == name)?;
        let hub = self.hubs.remove(index);
        if hub.official {
            self.official_removed = true;
        }
        Some(hub)
    }

    /// Add the official hub if it is not configured, reporting whether it was added.
    ///
    /// Identity is the `official` flag, not the name: a user who removed the official hub
    /// on purpose must not have it silently reappear on the next start, so this only fires
    /// when nothing is marked official -- and `remove` leaves a tombstone for exactly that
    /// reason. A user hub that happens to be called `official` blocks the seed by name, and
    /// that is fine: theirs was there first.
    pub fn ensure_official(&mut self) -> bool {
        if self.hubs.iter().any(|hub| hub.official) || self.official_removed {
            return false;
        }
        if self.get(OFFICIAL_HUB_NAME).is_some() {
            return false;
        }
        self.hubs.push(Hub {
            name: OFFICIAL_HUB_NAME.to_string(),
            url: OFFICIAL_HUB_REPO.to_string(),
            branch: None,
            enabled: true,
            last_synced: None,
            official: true,
            forge: Some(Forge::GitHub),
        });
        true
    }

    /// Hubs a start should refresh: enabled, stale, and only when auto-sync is on.
    ///
    /// Answers from the config alone. Whether the *environment* permits a start-up sync is
    /// the caller's question (`auto_sync_disabled_by_env`), which keeps this decidable from
    /// a `HubConfig` and a clock, and keeps its test independent of the shell it runs in.
    ///
    /// Returns owned hubs because the refresh runs on another thread.
    pub fn due_for_auto_sync(&self, now: chrono::DateTime<chrono::Local>) -> Vec<Hub> {
        if !self.auto_sync {
            return Vec::new();
        }
        self.active()
            .filter(|hub| hub.is_stale(AUTO_SYNC_TTL, now))
            .cloned()
            .collect()
    }
}

/// One hub on one line: what it is, what it holds, and how current it is. Shared by the
/// Hubs popup, the `list` status line and `logscout hub list`, so a hub reads the same
/// everywhere it is named.
pub fn describe_hub(hub: &Hub) -> String {
    let counts = format!(
        "{} schema(s), {} filter(s), {} search(es)",
        hub_schemas(hub).len(),
        hub_filters(hub).len(),
        hub_searches(hub).len()
    );
    let state = match (hub.enabled, hub.last_synced.as_deref()) {
        (false, _) => "disabled".to_string(),
        (true, None) => "never synced".to_string(),
        // The date is the useful part; the time it happened is noise.
        (true, Some(when)) => format!("synced {}", when.split('T').next().unwrap_or(when)),
    };
    // Mark the first-party hub, unless it is sitting under the name that already says so.
    let official = if hub.official && hub.name != OFFICIAL_HUB_NAME {
        " (official)"
    } else {
        ""
    };
    format!("{}{official} [{}] — {counts}, {state}", hub.name, hub.url)
}

/// Add `repo` to `config` and fetch it, returning the hub as configured and what it holds.
///
/// A hub that will not fetch is not added: a configured hub that has never resolved is an
/// entry the user would only have to clean up. On success the config is saved, so the
/// caller does not have to remember to.
///
/// Shared by the Hubs prompt and `logscout hub add` -- the rollback and the official-repo
/// handling are too easy to get subtly different in two places.
pub fn add_and_sync(
    config: &mut HubConfig,
    repo: &str,
    name: Option<String>,
) -> Result<(Hub, SyncReport), String> {
    let parsed = parse_repo(repo)?;
    // Adding the official repo by hand gets the official hub's behaviour -- bare names,
    // shadowing the bundled copies -- however the user spelled the URL. Otherwise a
    // re-added official hub would come back as a namespaced stranger.
    let official = repo_is_official(repo);
    let mut hub = Hub {
        name: name.unwrap_or_else(|| {
            if official {
                OFFICIAL_HUB_NAME.to_string()
            } else {
                parsed.default_hub_name()
            }
        }),
        url: repo.to_string(),
        branch: parsed.branch.clone(),
        enabled: true,
        last_synced: None,
        official,
        // Unknown for a host we do not recognise; the first sync settles it.
        forge: parsed.forge,
    };
    config.add(hub.clone())?;

    match sync_hub(&mut hub) {
        Ok(report) => {
            if let Some(slot) = config.get_mut(&hub.name) {
                // Keep the synced copy: it carries `last_synced`.
                *slot = hub.clone();
            }
            config
                .save()
                .map_err(|error| format!("could not save hubs.json: {error}"))?;
            Ok((hub, report))
        }
        Err(error) => {
            config.remove(&hub.name);
            Err(format!("hub '{}' not added: {error}", hub.name))
        }
    }
}

/// What a multi-hub sync did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncSummary {
    pub synced: usize,
    pub items: usize,
    /// One `name: reason` per hub that failed.
    pub failures: Vec<String>,
}

/// Sync one hub, or every configured hub when `name` is `None`, saving `last_synced`.
///
/// A hub that fails is reported rather than fatal: one unreachable hub must not stop the
/// others from refreshing.
pub fn sync_named(config: &mut HubConfig, name: Option<&str>) -> Result<SyncSummary, String> {
    let targets: Vec<String> = match name {
        Some(name) => {
            if config.get(name).is_none() {
                return Err(format!("no hub '{name}'"));
            }
            vec![name.to_string()]
        }
        None => config.hubs.iter().map(|hub| hub.name.clone()).collect(),
    };
    if targets.is_empty() {
        return Err("no hubs configured — try: add acme/log-scouter-hub".to_string());
    }

    let mut summary = SyncSummary::default();
    for target in &targets {
        let Some(hub) = config.get_mut(target) else {
            continue;
        };
        let mut updated = hub.clone();
        match sync_hub(&mut updated) {
            Ok(report) => {
                *hub = updated;
                summary.synced += 1;
                summary.items += report.total();
            }
            Err(error) => summary.failures.push(format!("{target}: {error}")),
        }
    }
    config
        .save()
        .map_err(|error| format!("could not save hubs.json: {error}"))?;
    Ok(summary)
}

/// Whether a URL names the official hub repo, however the user spelled it.
pub fn repo_is_official(url: &str) -> bool {
    let (Ok(parsed), Ok(official)) = (parse_repo(url), parse_repo(OFFICIAL_HUB_REPO)) else {
        return false;
    };
    parsed.host.eq_ignore_ascii_case(&official.host)
        && parsed.path.eq_ignore_ascii_case(&official.path)
}

/// Whether the environment forbids the start-up sync.
pub fn auto_sync_disabled_by_env() -> bool {
    std::env::var(NO_AUTO_SYNC_VAR).is_ok_and(|value| !value.trim().is_empty())
}

/// A finished background sync: the hub as it now stands, and how it went.
pub struct SyncOutcome {
    pub hub: Hub,
    pub result: Result<SyncReport, String>,
}

/// Refresh `hubs` on a background thread, reporting each outcome as it lands.
///
/// The render loop cannot block on the network, and unlike the AI worker this thread is
/// one-shot: it syncs what it was handed, sends the outcomes, and ends. A receiver the main
/// thread has dropped just ends the loop early.
pub fn spawn_sync(hubs: Vec<Hub>) -> std::sync::mpsc::Receiver<SyncOutcome> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("logscout-hub-sync".to_string())
        .spawn(move || {
            for mut hub in hubs {
                let result = sync_hub(&mut hub);
                if tx.send(SyncOutcome { hub, result }).is_err() {
                    break;
                }
            }
        })
        // A thread that will not spawn is not worth taking the app down for: the caller
        // simply never receives an outcome and keeps using the cache it already has.
        .ok();
    rx
}

/// The kind of git host a hub lives on. Each one serves repo tarballs from a different URL
/// and wants its token in a different header; nothing else about a hub depends on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Forge {
    GitHub,
    GitLab,
    Gitea,
}

/// The forges to try, in order, for a host we have not identified yet. `GitHub` is not in
/// the list: it is recognised by hostname, and its API is not something a self-hosted GitLab
/// or Gitea will answer to.
const PROBE_ORDER: &[Forge] = &[Forge::GitLab, Forge::Gitea];

impl Forge {
    pub fn label(self) -> &'static str {
        match self {
            Forge::GitHub => "github",
            Forge::GitLab => "gitlab",
            Forge::Gitea => "gitea",
        }
    }
}

/// A repo on some git host, parsed out of whatever the user typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    /// `https` unless the user explicitly asked for `http`.
    pub scheme: String,
    pub host: String,
    /// Everything between the host and the branch, `.git` stripped: `owner/repo`, or a
    /// GitLab group path like `team/sub/repo`.
    pub path: String,
    /// Only set when the URL itself named one (`/tree/<branch>`, or GitLab's `/-/tree/`).
    pub branch: Option<String>,
    /// The forge, when the URL settles it. `None` means "ask the host" -- see `sync_hub`.
    pub forge: Option<Forge>,
}

impl RepoRef {
    /// The repo's own name: the last path segment.
    pub fn repo(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// The name a hub gets when the user does not choose one.
    pub fn default_hub_name(&self) -> String {
        sanitize_file_component(self.repo())
    }

    /// Where `forge` serves this repo's tarball.
    ///
    /// With no branch every forge here resolves `HEAD` to the default branch, so a hub
    /// tracking a repo that renames `master` to `main` keeps working.
    pub fn tarball_url(&self, forge: Forge, branch: Option<&str>) -> String {
        let reference = branch.unwrap_or("HEAD");
        match forge {
            // codeload wants a full ref for a branch, but takes a bare `HEAD`.
            Forge::GitHub => {
                let path = match branch {
                    Some(branch) => format!("refs/heads/{branch}"),
                    None => "HEAD".to_string(),
                };
                format!("https://codeload.github.com/{}/tar.gz/{path}", self.path)
            }
            // The file name at the end is cosmetic; GitLab serves the archive whatever it says.
            Forge::GitLab => format!(
                "{}://{}/{}/-/archive/{reference}/{}-{reference}.tar.gz",
                self.scheme,
                self.host,
                self.path,
                self.repo()
            ),
            Forge::Gitea => format!(
                "{}://{}/{}/archive/{reference}.tar.gz",
                self.scheme, self.host, self.path
            ),
        }
    }

    /// The forges worth trying for this repo, best first.
    pub fn candidates(&self) -> Vec<Forge> {
        match self.forge {
            Some(forge) => vec![forge],
            None => PROBE_ORDER.to_vec(),
        }
    }
}

const GITHUB_HOST: &str = "github.com";

/// Parse `owner/repo`, an HTTP(S) URL, or an SSH URL, on GitHub or any self-hosted GitLab
/// or Gitea.
///
/// A bare `owner/repo` means GitHub, which is what it has always meant. Anything with a host
/// in it keeps that host: `github.com` is recognised by name, and any other host is left for
/// `sync_hub` to identify by asking it for a tarball, since a self-hosted GitLab will not
/// answer an unauthenticated `/api/v4/version` and so cannot be identified up front.
pub fn parse_repo(input: &str) -> Result<RepoRef, String> {
    let text = input.trim();
    if text.is_empty() {
        return Err("a hub needs a repo: owner/repo or a repo URL".to_string());
    }

    let (scheme, host, rest) = split_location(text)?;
    let forge = (host == GITHUB_HOST).then_some(Forge::GitHub);

    let rest = rest.trim_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let parts: Vec<&str> = rest.split('/').filter(|part| !part.is_empty()).collect();

    // `/tree/<branch>` is how a browser names a branch; GitLab writes it `/-/tree/<branch>`.
    // Anything after the branch is a path into the tree, which is not something to pin to.
    let (path_parts, branch) = match parts
        .iter()
        .position(|part| *part == "-" || *part == "tree")
    {
        Some(index) => {
            let branch = parts
                .iter()
                .skip(index)
                .skip_while(|part| **part != "tree")
                .nth(1)
                .ok_or_else(|| format!("not a repo: '{text}' (expected owner/repo)"))?;
            (&parts[..index], Some((*branch).to_string()))
        }
        None => (&parts[..], None),
    };

    // Every forge here needs at least an owner and a repo; GitLab allows more in between.
    if path_parts.len() < 2 {
        return Err(format!("not a repo: '{text}' (expected owner/repo)"));
    }
    let path = path_parts.join("/");
    let path = path.strip_suffix(".git").unwrap_or(&path).to_string();

    Ok(RepoRef {
        scheme,
        host,
        path,
        branch,
        forge,
    })
}

/// Pull the scheme, host and remaining path out of the many ways a repo gets written down.
fn split_location(text: &str) -> Result<(String, String, &str), String> {
    // `git@host:path` and `ssh://git@host[:port]/path`: we cannot fetch over SSH, but the
    // host and path are all we need to build an HTTPS archive URL.
    if let Some(rest) = text.strip_prefix("ssh://") {
        let rest = rest.split_once('@').map(|(_, rest)| rest).unwrap_or(rest);
        let (authority, path) = rest
            .split_once('/')
            .ok_or_else(|| format!("not a repo: '{text}'"))?;
        let host = authority.split(':').next().unwrap_or(authority);
        return Ok(("https".to_string(), host.to_string(), path));
    }
    if let Some((authority, path)) = text.split_once(':') {
        if !authority.contains('/') && !path.starts_with("//") {
            let host = authority
                .split_once('@')
                .map(|(_, host)| host)
                .unwrap_or(authority);
            return Ok(("https".to_string(), host.to_string(), path));
        }
    }

    for (prefix, scheme) in [("https://", "https"), ("http://", "http")] {
        if let Some(rest) = text.strip_prefix(prefix) {
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            let host = host.split_once('@').map(|(_, host)| host).unwrap_or(host);
            return Ok((scheme.to_string(), host.to_string(), path));
        }
    }

    // No scheme: either `host/owner/repo` or the bare `owner/repo` shorthand for GitHub.
    // A first segment with a dot in it is a hostname; `owner.name/repo` is not a thing.
    if let Some((maybe_host, path)) = text.split_once('/') {
        if maybe_host.contains('.') && !maybe_host.ends_with(".git") {
            return Ok(("https".to_string(), maybe_host.to_string(), path));
        }
    }
    Ok(("https".to_string(), GITHUB_HOST.to_string(), text))
}

/// What a sync brought in, for the status line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub schemas: usize,
    pub filters: usize,
    pub searches: usize,
}

impl SyncReport {
    pub fn total(&self) -> usize {
        self.schemas + self.filters + self.searches
    }

    pub fn describe(&self) -> String {
        format!(
            "{} schema(s), {} filter(s), {} search(es)",
            self.schemas, self.filters, self.searches
        )
    }
}

/// Download `hub`'s repo and replace its cache with the result.
///
/// Blocking: it runs the request on a private current-thread runtime. The payload is a
/// handful of small JSON files, and the client carries a timeout, so the frame this costs
/// is bounded.
///
/// A private repo needs a token in `LOGSCOUT_HUB_TOKEN` or `GITHUB_TOKEN`.
pub fn sync_hub(hub: &mut Hub) -> Result<SyncReport, String> {
    let repo = parse_repo(&hub.url)?;
    let branch = hub.branch.clone().or_else(|| repo.branch.clone());
    let dest = hub_cache_dir(&hub.name)
        .ok_or_else(|| "no home directory: cannot cache a hub".to_string())?;

    // Which forge is this? A hub that has synced before remembers. A new one on a host we
    // do not recognise gets asked: try each candidate's archive URL and keep the one that
    // answers with an archive. Identifying by probe rather than by an API version endpoint
    // is what makes a self-hosted GitLab work -- it will not answer `/api/v4/version` to an
    // anonymous request, but it will serve a public repo's tarball.
    let candidates: Vec<Forge> = hub
        .forge
        .map(|forge| vec![forge])
        .unwrap_or_else(|| repo.candidates());

    let mut last_error = String::new();
    for (index, forge) in candidates.iter().enumerate() {
        let url = repo.tarball_url(*forge, branch.as_deref());
        let token = token_for(*forge);
        match fetch(&url, token.as_ref().map(|(k, v)| (*k, v.as_str()))) {
            Ok(tarball) => {
                let report = unpack_hub(&tarball, &dest)?;
                hub.forge = Some(*forge);
                hub.last_synced = Some(chrono::Local::now().to_rfc3339());
                return Ok(report);
            }
            // Keep the first forge's error: it is the one the user most likely meant, and
            // a later candidate's 404 is just "not that kind of host either".
            Err(error) => {
                if index == 0 {
                    last_error = error;
                }
            }
        }
    }
    Err(last_error)
}

/// The token to send to `forge`, if the user has set one, and the header it belongs in.
///
/// Scoped to the forge on purpose. `GITHUB_TOKEN` is set automatically all over CI, and a
/// hub is a URL the user names: sending whatever `GITHUB_TOKEN` happens to be in the
/// environment to an arbitrary host would hand that host a GitHub credential it has no
/// business seeing. So each forge only ever reads its own variable.
///
/// `LOGSCOUT_HUB_TOKEN` is the exception: it is set by hand, for hubs, and goes to whichever
/// host the hub is on. With hubs on more than one host, prefer the per-forge variables.
fn token_for(forge: Forge) -> Option<(&'static str, String)> {
    let specific = match forge {
        Forge::GitHub => "GITHUB_TOKEN",
        Forge::GitLab => "GITLAB_TOKEN",
        Forge::Gitea => "GITEA_TOKEN",
    };
    let value = [specific, "LOGSCOUT_HUB_TOKEN"]
        .iter()
        .find_map(|key| std::env::var(key).ok())
        .filter(|value| !value.trim().is_empty())?;
    // Each forge takes its token its own way; the wrong header is simply ignored, which
    // looks exactly like a bad token.
    let header = match forge {
        Forge::GitHub => "Authorization",
        Forge::GitLab => "PRIVATE-TOKEN",
        Forge::Gitea => "Authorization",
    };
    let value = match forge {
        Forge::GitHub => format!("Bearer {value}"),
        Forge::GitLab => value,
        Forge::Gitea => format!("token {value}"),
    };
    Some((header, value))
}

fn fetch(url: &str, token: Option<(&str, &str)>) -> Result<Vec<u8>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start async runtime: {error}"))?;

    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .map_err(|error| format!("could not build http client: {error}"))?;

        let mut request = client.get(url).header("User-Agent", "log-scouter");
        if let Some((header, value)) = token {
            request = request.header(header, value);
        }

        let response = request
            .send()
            .await
            .map_err(|error| format!("fetch failed: {error}"))?;

        let status = response.status();
        if !status.is_success() {
            return Err(match status.as_u16() {
                404 => format!(
                    "not found: {url}\n(a private repo needs LOGSCOUT_HUB_TOKEN or GITHUB_TOKEN)"
                ),
                401 | 403 => format!("access denied ({status}): check LOGSCOUT_HUB_TOKEN"),
                _ => format!("fetch failed: {status}"),
            });
        }

        if let Some(length) = response.content_length() {
            if length > MAX_TARBALL_BYTES {
                return Err(format!(
                    "refusing a {length}-byte tarball: too big for a hub"
                ));
            }
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("download failed: {error}"))?;
        if bytes.len() as u64 > MAX_TARBALL_BYTES {
            return Err(format!(
                "refusing a {}-byte tarball: too big for a hub",
                bytes.len()
            ));
        }
        Ok(bytes.to_vec())
    })
}

/// Unpack the `schemas/`, `filters/` and `searches/` JSON out of a repo tarball into
/// `dest`, replacing whatever was there.
///
/// Builds the new snapshot beside the old one and swaps at the end, so a download that
/// fails halfway leaves the previous snapshot intact.
pub fn unpack_hub(tarball: &[u8], dest: &Path) -> Result<SyncReport, String> {
    let parent = dest
        .parent()
        .ok_or_else(|| "invalid hub cache path".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;

    let staging = dest.with_extension("incoming");
    let _ = fs::remove_dir_all(&staging);
    let report = extract_into(tarball, &staging).inspect_err(|_| {
        let _ = fs::remove_dir_all(&staging);
    })?;

    if report.total() == 0 {
        let _ = fs::remove_dir_all(&staging);
        return Err(
            "no schemas/, filters/ or searches/ JSON in that repo -- is it a hub?".to_string(),
        );
    }

    // Rename onto the live path. `rename` will not replace a non-empty directory, so the
    // old snapshot goes first; the window where neither exists is a few microseconds, and
    // a crash inside it costs a re-sync, not data.
    let _ = fs::remove_dir_all(dest);
    fs::rename(&staging, dest).map_err(|error| format!("{}: {error}", dest.display()))?;
    Ok(report)
}

fn extract_into(tarball: &[u8], staging: &Path) -> Result<SyncReport, String> {
    let decoder = flate2::read::GzDecoder::new(tarball);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| format!("not a gzipped tarball: {error}"))?;

    let mut report = SyncReport::default();
    for entry in entries {
        let mut entry = entry.map_err(|error| format!("corrupt tarball: {error}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|error| format!("corrupt tarball: {error}"))?
            .into_owned();
        let Some((folder, file)) = hub_entry_target(&path) else {
            continue;
        };

        let out_dir = staging.join(folder);
        fs::create_dir_all(&out_dir).map_err(|error| format!("{}: {error}", out_dir.display()))?;
        entry
            .unpack(out_dir.join(file))
            .map_err(|error| format!("{path:?}: {error}"))?;

        match folder {
            USER_SCHEMAS_SUBDIR => report.schemas += 1,
            USER_FILTERS_SUBDIR => report.filters += 1,
            _ => report.searches += 1,
        }
    }
    Ok(report)
}

/// Where a tar entry belongs in the cache, or `None` to skip it.
///
/// GitHub wraps everything in one `<repo>-<ref>/` directory, so the first component is
/// dropped. What survives is exactly `<one of HUB_FOLDERS>/<file>.json` -- a flat, known
/// destination, which is also why a `../..` entry cannot escape the cache: it never
/// matches the shape.
fn hub_entry_target(path: &Path) -> Option<(&'static str, String)> {
    let parts: Vec<_> = path
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    let [_wrapper, folder, file] = parts.as_slice() else {
        return None;
    };
    let folder = HUB_FOLDERS.iter().find(|known| *known == folder)?;
    if !file.to_lowercase().ends_with(".json") || file.starts_with('.') {
        return None;
    }
    // The file name is used verbatim, so it must be a plain name, not a path.
    let file = file.to_string();
    if file.contains('/') || file.contains('\\') || file.contains("..") {
        return None;
    }
    Some((folder, file))
}

/// Delete a hub's snapshot. A hub that was never synced has no cache, which is not an error.
pub fn remove_hub_cache(name: &str) -> io::Result<()> {
    let Some(dir) = hub_cache_dir(name) else {
        return Ok(());
    };
    match fs::remove_dir_all(&dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// `<hub>/<name>` -- how a hub's item is named everywhere the user sees it.
pub fn namespaced(hub: &str, name: &str) -> String {
    format!("{hub}/{name}")
}

/// A hub's cached `<folder>`, or `None` when it has not been synced.
fn hub_folder(hub: &Hub, folder: &str) -> Option<PathBuf> {
    let dir = hub_cache_dir(&hub.name)?.join(folder);
    dir.is_dir().then_some(dir)
}

/// A hub's schemas, each renamed `<hub>/<schema>` (bare for the official hub). An unsynced
/// or unreadable hub contributes nothing: a library that fails to load must never break
/// log detection.
pub fn hub_schemas(hub: &Hub) -> Vec<SchemaFile> {
    match hub_folder(hub, USER_SCHEMAS_SUBDIR) {
        Some(dir) => schemas_in(hub.namespace(), &dir),
        None => Vec::new(),
    }
}

/// A hub's filters, each renamed `<hub>/<filter>` (bare for the official hub).
pub fn hub_filters(hub: &Hub) -> Vec<FilterFile> {
    match hub_folder(hub, USER_FILTERS_SUBDIR) {
        Some(dir) => filters_in(hub.namespace(), &dir),
        None => Vec::new(),
    }
}

/// A hub's saved searches, each renamed `<hub>/<search>` (bare for the official hub).
pub fn hub_searches(hub: &Hub) -> Vec<SearchFile> {
    match hub_folder(hub, USER_SEARCHES_SUBDIR) {
        Some(dir) => searches_in(hub.namespace(), &dir),
        None => Vec::new(),
    }
}

// The loaders below take the folder rather than reading it off `$HOME`, so the namespacing
// is exercised against a real snapshot without a test having to move the home directory.
// `namespace: None` is the official hub, whose items keep the name the repo gave them.

fn rename(namespace: Option<&str>, name: &str) -> String {
    match namespace {
        Some(namespace) => namespaced(namespace, name),
        None => name.to_string(),
    }
}

fn schemas_in(namespace: Option<&str>, dir: &Path) -> Vec<SchemaFile> {
    let mut schemas = load_schemas_from_folder(dir).unwrap_or_default();
    for file in &mut schemas {
        file.name = rename(namespace, &file.name);
        file.schema.name = file.name.clone();
    }
    schemas
}

fn filters_in(namespace: Option<&str>, dir: &Path) -> Vec<FilterFile> {
    let mut filters = load_filters_from_folder(dir).unwrap_or_default();
    for file in &mut filters {
        file.name = rename(namespace, &file.name);
    }
    filters
}

fn searches_in(namespace: Option<&str>, dir: &Path) -> Vec<SearchFile> {
    let mut searches = load_searches_from_folder(dir).unwrap_or_default();
    for file in &mut searches {
        file.name = rename(namespace, &file.name);
    }
    searches
}

/// Every enabled hub's schemas, in configured order -- the hub tier of the schema library.
///
/// Only schemas have an "all hubs" view: they are resolved by name and so must all be
/// offered at once. Filters and searches are imported from one named hub at a time.
pub fn all_hub_schemas(config: &HubConfig) -> Vec<SchemaFile> {
    config.active().flat_map(hub_schemas).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub(name: &str, url: &str) -> Hub {
        Hub {
            name: name.to_string(),
            url: url.to_string(),
            branch: None,
            enabled: true,
            last_synced: None,
            official: false,
            forge: None,
        }
    }

    fn github(path: &str) -> RepoRef {
        RepoRef {
            scheme: "https".into(),
            host: "github.com".into(),
            path: path.into(),
            branch: None,
            forge: Some(Forge::GitHub),
        }
    }

    /// `now`, and a `last_synced` stamp that many hours before it.
    fn hours_ago(hours: i64) -> (chrono::DateTime<chrono::Local>, String) {
        let now = chrono::Local::now();
        (now, (now - chrono::Duration::hours(hours)).to_rfc3339())
    }

    #[test]
    fn parses_the_shorthand_and_url_forms_of_a_repo() {
        let expected = github("acme/log-scouter-hub");
        for input in [
            "acme/log-scouter-hub",
            "https://github.com/acme/log-scouter-hub",
            "https://github.com/acme/log-scouter-hub.git",
            "https://github.com/acme/log-scouter-hub/",
            "github.com/acme/log-scouter-hub",
            "git@github.com:acme/log-scouter-hub.git",
            "ssh://git@github.com/acme/log-scouter-hub.git",
        ] {
            assert_eq!(parse_repo(input).unwrap(), expected, "parsing {input}");
        }
    }

    #[test]
    fn a_tree_url_pins_the_branch_it_names() {
        let parsed = parse_repo("https://github.com/acme/hub/tree/stable").unwrap();
        assert_eq!(parsed.branch.as_deref(), Some("stable"));
        assert_eq!(
            parsed.tarball_url(Forge::GitHub, parsed.branch.as_deref()),
            "https://codeload.github.com/acme/hub/tar.gz/refs/heads/stable"
        );
    }

    /// No branch means HEAD, so a repo that renames its default branch keeps syncing. Every
    /// forge here resolves HEAD the same way.
    #[test]
    fn no_branch_tracks_the_default_branch() {
        let parsed = parse_repo("acme/hub").unwrap();
        assert_eq!(
            parsed.tarball_url(Forge::GitHub, None),
            "https://codeload.github.com/acme/hub/tar.gz/HEAD"
        );
        let self_hosted = parse_repo("http://git.example.com/acme/hub").unwrap();
        assert_eq!(
            self_hosted.tarball_url(Forge::GitLab, None),
            "http://git.example.com/acme/hub/-/archive/HEAD/hub-HEAD.tar.gz"
        );
        assert_eq!(
            self_hosted.tarball_url(Forge::Gitea, None),
            "http://git.example.com/acme/hub/archive/HEAD.tar.gz"
        );
    }

    /// A hub on a self-hosted host: the host and scheme are kept, and the forge is left for
    /// the first sync to settle by asking.
    #[test]
    fn a_self_hosted_url_keeps_its_host_and_scheme() {
        let parsed =
            parse_repo("http://tec-l-1203160.labs.microstrategy.com/qhu/mstr-log-scouter-hub.git")
                .unwrap();
        assert_eq!(parsed.scheme, "http");
        assert_eq!(parsed.host, "tec-l-1203160.labs.microstrategy.com");
        assert_eq!(parsed.path, "qhu/mstr-log-scouter-hub");
        assert_eq!(parsed.repo(), "mstr-log-scouter-hub");
        assert_eq!(parsed.default_hub_name(), "mstr-log-scouter-hub");
        assert_eq!(
            parsed.forge, None,
            "an unknown host is identified by asking it"
        );
        assert_eq!(parsed.candidates(), PROBE_ORDER.to_vec());
        assert_eq!(
            parsed.tarball_url(Forge::GitLab, None),
            "http://tec-l-1203160.labs.microstrategy.com/qhu/mstr-log-scouter-hub\
             /-/archive/HEAD/mstr-log-scouter-hub-HEAD.tar.gz"
        );
    }

    /// GitLab nests groups, so a repo path can be deeper than `owner/repo`.
    #[test]
    fn a_gitlab_group_path_survives_intact() {
        let parsed = parse_repo("https://gitlab.example.com/team/sub/hub").unwrap();
        assert_eq!(parsed.path, "team/sub/hub");
        assert_eq!(parsed.repo(), "hub");
        assert_eq!(
            parsed.tarball_url(Forge::GitLab, Some("main")),
            "https://gitlab.example.com/team/sub/hub/-/archive/main/hub-main.tar.gz"
        );
    }

    /// GitLab writes a browser branch URL `/-/tree/<branch>`.
    #[test]
    fn a_gitlab_tree_url_pins_its_branch() {
        let parsed = parse_repo("https://gitlab.example.com/team/hub/-/tree/stable").unwrap();
        assert_eq!(parsed.path, "team/hub");
        assert_eq!(parsed.branch.as_deref(), Some("stable"));
    }

    /// An SSH remote names a host we can still fetch from over HTTPS.
    #[test]
    fn an_ssh_url_becomes_an_https_fetch() {
        let parsed = parse_repo("ssh://git@git.example.com:22222/qhu/hub.git").unwrap();
        assert_eq!(parsed.scheme, "https");
        assert_eq!(
            parsed.host, "git.example.com",
            "the port is not part of the host"
        );
        assert_eq!(parsed.path, "qhu/hub");
    }

    /// A token belongs to the host it was issued for. `GITHUB_TOKEN` is set automatically
    /// across CI, and a hub URL is user input: shipping it to whatever host a hub names
    /// would hand out a GitHub credential.
    #[test]
    fn tokens_are_scoped_to_their_forge() {
        // Not asserted against the environment -- these tests run in parallel and cannot own
        // it -- but the mapping itself is the property that matters.
        for (forge, expected_var, expected_header) in [
            (Forge::GitHub, "GITHUB_TOKEN", "Authorization"),
            (Forge::GitLab, "GITLAB_TOKEN", "PRIVATE-TOKEN"),
            (Forge::Gitea, "GITEA_TOKEN", "Authorization"),
        ] {
            if let Some((header, _)) = token_for(forge) {
                assert_eq!(
                    header, expected_header,
                    "{expected_var} goes in {expected_header}"
                );
            }
        }
        // The forge picks its own variable, never another forge's.
        assert_ne!(Forge::GitLab.label(), Forge::GitHub.label());
    }

    #[test]
    fn rejects_malformed_repos() {
        for input in ["", "acme", "https://github.com/", "https://gitlab.com/acme"] {
            assert!(parse_repo(input).is_err(), "should reject {input:?}");
        }
    }

    /// A host other than github.com is a hub host now, not an error. This used to be
    /// rejected outright, which is exactly what made a self-hosted GitLab unusable.
    #[test]
    fn any_git_host_is_accepted() {
        for input in [
            "https://gitlab.com/acme/hub",
            "http://gitlab.internal/acme/hub",
            "https://gitea.example.com/acme/hub",
            "git@gitlab.example.com:acme/hub.git",
        ] {
            let parsed = parse_repo(input).unwrap_or_else(|error| panic!("{input}: {error}"));
            assert_ne!(parsed.host, "github.com", "{input} keeps its own host");
            assert_eq!(parsed.path, "acme/hub", "{input}");
        }
    }

    #[test]
    fn adding_a_name_that_is_taken_is_refused() {
        let mut config = HubConfig::default();
        config.add(hub("acme", "acme/hub")).unwrap();
        let error = config.add(hub("acme", "other/hub")).unwrap_err();
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(config.hubs.len(), 1);
        assert_eq!(config.hubs[0].url, "acme/hub");
    }

    #[test]
    fn remove_returns_the_hub_and_forgets_it() {
        let mut config = HubConfig::default();
        config.add(hub("acme", "acme/hub")).unwrap();
        config.add(hub("ops", "ops/hub")).unwrap();
        assert_eq!(config.remove("acme").unwrap().url, "acme/hub");
        assert!(config.remove("acme").is_none());
        assert_eq!(config.hubs.len(), 1);
    }

    #[test]
    fn a_disabled_hub_contributes_nothing() {
        let mut config = HubConfig::default();
        config.add(hub("acme", "acme/hub")).unwrap();
        config.add(hub("ops", "ops/hub")).unwrap();
        config.get_mut("acme").unwrap().enabled = false;
        let active: Vec<_> = config.active().map(|hub| hub.name.as_str()).collect();
        assert_eq!(active, ["ops"]);
    }

    /// The wrapper directory GitHub adds is dropped, and only the three known folders'
    /// JSON survives.
    #[test]
    fn tar_entries_map_to_the_known_folders_only() {
        let target = |path: &str| hub_entry_target(Path::new(path));
        assert_eq!(
            target("hub-main/schemas/spring.json"),
            Some(("schemas", "spring.json".to_string()))
        );
        assert_eq!(
            target("hub-main/filters/noise.JSON"),
            Some(("filters", "noise.JSON".to_string()))
        );
        assert_eq!(
            target("hub-main/searches/errors.json"),
            Some(("searches", "errors.json".to_string()))
        );

        for skipped in [
            "hub-main/README.md",
            "hub-main/schemas/notes.md",
            "hub-main/docs/schemas/spring.json",
            "hub-main/schemas/nested/spring.json",
            "hub-main/.github/workflows/ci.yml",
            "hub-main/schemas/.hidden.json",
            "hub-main/schemas",
        ] {
            assert_eq!(target(skipped), None, "should skip {skipped}");
        }
    }

    /// A traversal entry cannot name a destination, because the only shape accepted is
    /// `<wrapper>/<known folder>/<plain file>.json`.
    #[test]
    fn tar_entries_cannot_escape_the_cache() {
        for hostile in [
            "hub-main/schemas/../../../../etc/passwd.json",
            "../../etc/passwd.json",
            "/etc/passwd.json",
            "hub-main/../filters/evil.json",
        ] {
            assert_eq!(
                hub_entry_target(Path::new(hostile)),
                None,
                "should refuse {hostile}"
            );
        }
    }

    /// Build a tarball shaped like GitHub's: every path under one `<repo>-<ref>/` wrapper.
    fn tarball(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, body.as_bytes())
                .unwrap();
        }
        let tar = builder.into_inner().unwrap();

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &tar).unwrap();
        encoder.finish().unwrap()
    }

    fn schema_json(name: &str) -> String {
        serde_json::json!({
            "name": name,
            "description": "from a hub",
            "schema": {
                "name": name,
                "format": "[<timestamp>] <level> <message>",
                "timestamp_format": "%Y-%m-%d %H:%M:%S",
            }
        })
        .to_string()
    }

    /// The whole local half of a sync: a repo tarball in, a namespaced, loadable library out.
    #[test]
    fn unpacking_a_repo_tarball_yields_a_namespaced_library() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("acme");
        let archive = tarball(&[
            ("hub-main/README.md", "# not a schema"),
            ("hub-main/schemas/spring.json", &schema_json("Spring-Boot")),
            (
                "hub-main/filters/noise.json",
                r#"{"name":"noise","description":"drop trace",
                    "filter":{"field":"level","op":"equals","value":"Trace","action":"exclude"}}"#,
            ),
            (
                "hub-main/searches/errors.json",
                r#"{"name":"errors","description":"all errors","query":"level=Error"}"#,
            ),
        ]);

        let report = unpack_hub(&archive, &dest).unwrap();
        assert_eq!(
            report,
            SyncReport {
                schemas: 1,
                filters: 1,
                searches: 1
            }
        );
        // The wrapper directory is stripped and the README ignored.
        assert!(dest.join("schemas/spring.json").is_file());
        assert!(!dest.join("README.md").exists());

        let schemas = schemas_in(Some("acme"), &dest.join("schemas"));
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "acme/Spring-Boot");
        // The schema's own name is namespaced too, since that is what detection matches on.
        assert_eq!(schemas[0].schema.name, "acme/Spring-Boot");

        let filters = filters_in(Some("acme"), &dest.join("filters"));
        assert_eq!(filters[0].name, "acme/noise");
        assert_eq!(filters[0].filter.value, "Trace");

        let searches = searches_in(Some("acme"), &dest.join("searches"));
        assert_eq!(searches[0].name, "acme/errors");
        assert_eq!(searches[0].query, "level=Error");
    }

    /// Two hubs shipping the same schema name both stay usable -- the point of namespacing.
    #[test]
    fn two_hubs_can_ship_the_same_name_without_collision() {
        let temp = tempfile::tempdir().unwrap();
        for hub in ["acme", "ops"] {
            let archive = tarball(&[("hub-main/schemas/gw.json", &schema_json("Gateway-Access"))]);
            unpack_hub(&archive, &temp.path().join(hub)).unwrap();
        }

        let names: Vec<String> = ["acme", "ops"]
            .iter()
            .flat_map(|hub| schemas_in(Some(hub), &temp.path().join(hub).join("schemas")))
            .map(|file| file.schema.name)
            .collect();
        assert_eq!(names, ["acme/Gateway-Access", "ops/Gateway-Access"]);
    }

    /// A re-sync replaces the snapshot, so a schema deleted upstream stops being offered.
    #[test]
    fn re_syncing_replaces_the_previous_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("acme");

        unpack_hub(
            &tarball(&[
                ("hub-main/schemas/a.json", &schema_json("A")),
                ("hub-main/schemas/gone.json", &schema_json("Gone")),
            ]),
            &dest,
        )
        .unwrap();
        assert_eq!(schemas_in(Some("acme"), &dest.join("schemas")).len(), 2);

        unpack_hub(
            &tarball(&[("hub-main/schemas/a.json", &schema_json("A"))]),
            &dest,
        )
        .unwrap();
        let names: Vec<String> = schemas_in(Some("acme"), &dest.join("schemas"))
            .into_iter()
            .map(|file| file.name)
            .collect();
        assert_eq!(names, ["acme/A"]);
    }

    /// A repo with none of the three folders is a mistake worth reporting, and it must not
    /// destroy the snapshot that is already cached.
    #[test]
    fn a_repo_that_is_not_a_hub_is_refused_and_keeps_the_old_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("acme");
        unpack_hub(
            &tarball(&[("hub-main/schemas/a.json", &schema_json("A"))]),
            &dest,
        )
        .unwrap();

        let error = unpack_hub(&tarball(&[("hub-main/README.md", "hello")]), &dest).unwrap_err();
        assert!(error.contains("is it a hub?"), "{error}");
        assert_eq!(schemas_in(Some("acme"), &dest.join("schemas")).len(), 1);
        // The staging directory is not left behind.
        assert!(!dest.with_extension("incoming").exists());
    }

    /// A hub that has never synced has no cache, and must simply contribute nothing.
    #[test]
    fn an_unsynced_hub_contributes_nothing() {
        let unsynced = hub("never-synced-hub-xyz", "acme/hub");
        assert!(hub_schemas(&unsynced).is_empty());
        assert!(hub_filters(&unsynced).is_empty());
        assert!(hub_searches(&unsynced).is_empty());
    }

    #[test]
    fn items_are_namespaced_under_their_hub() {
        assert_eq!(namespaced("acme", "Spring-Boot"), "acme/Spring-Boot");
    }

    /// The official hub's schemas keep the names the repo gave them, so a project already
    /// referencing `Spring Boot` resolves to the hub's copy instead of missing it.
    #[test]
    fn the_official_hub_keeps_bare_names_and_others_do_not() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("official");
        unpack_hub(
            &tarball(&[("hub-main/schemas/sb.json", &schema_json("Spring Boot"))]),
            &dest,
        )
        .unwrap();

        let mut official = hub(OFFICIAL_HUB_NAME, OFFICIAL_HUB_REPO);
        official.official = true;
        assert_eq!(official.namespace(), None);
        let names: Vec<String> = schemas_in(official.namespace(), &dest.join("schemas"))
            .into_iter()
            .map(|file| file.schema.name)
            .collect();
        assert_eq!(names, ["Spring Boot"]);

        // A third-party hub pointed at the same content still gets namespaced.
        let third_party = hub("acme", "acme/hub");
        assert_eq!(third_party.namespace(), Some("acme"));
        let names: Vec<String> = schemas_in(third_party.namespace(), &dest.join("schemas"))
            .into_iter()
            .map(|file| file.schema.name)
            .collect();
        assert_eq!(names, ["acme/Spring Boot"]);
    }

    #[test]
    fn a_fresh_config_is_seeded_with_the_official_hub_once() {
        let mut config = HubConfig::default();
        assert!(config.ensure_official());
        assert_eq!(config.hubs.len(), 1);
        let official = &config.hubs[0];
        assert!(official.official);
        assert_eq!(official.name, OFFICIAL_HUB_NAME);
        assert_eq!(official.url, OFFICIAL_HUB_REPO);

        // Seeding is idempotent.
        assert!(!config.ensure_official());
        assert_eq!(config.hubs.len(), 1);
    }

    /// Removing the official hub has to stick: the next start must not add back what the
    /// user just deleted.
    #[test]
    fn removing_the_official_hub_sticks_until_it_is_added_back() {
        let mut config = HubConfig::default();
        config.ensure_official();
        config.remove(OFFICIAL_HUB_NAME).unwrap();
        assert!(config.official_removed);

        assert!(!config.ensure_official());
        assert!(config.hubs.is_empty());

        // Adding it back by hand lifts the tombstone, and a later start leaves it alone.
        config
            .add(hub(
                OFFICIAL_HUB_NAME,
                "https://github.com/mangosteen-lab/log-scouter-hub",
            ))
            .unwrap();
        assert!(!config.official_removed);
        assert!(!config.ensure_official());
        assert_eq!(config.hubs.len(), 1);
    }

    /// A hub of the user's own called `official` was there first and is not overwritten.
    #[test]
    fn a_user_hub_named_official_blocks_the_seed() {
        let mut config = HubConfig::default();
        config
            .add(hub(OFFICIAL_HUB_NAME, "acme/their-hub"))
            .unwrap();
        assert!(!config.ensure_official());
        assert_eq!(config.hubs.len(), 1);
        assert_eq!(config.hubs[0].url, "acme/their-hub");
    }

    #[test]
    fn the_official_repo_is_recognised_however_it_is_spelled() {
        for url in [
            "mangosteen-lab/log-scouter-hub",
            "https://github.com/mangosteen-lab/log-scouter-hub",
            "git@github.com:mangosteen-lab/log-scouter-hub.git",
            "https://github.com/Mangosteen-Lab/Log-Scouter-Hub",
        ] {
            assert!(repo_is_official(url), "{url} is the official repo");
        }
        for url in ["acme/log-scouter-hub", "mangosteen-lab/log-scouter", "junk"] {
            assert!(!repo_is_official(url), "{url} is not the official repo");
        }
    }

    #[test]
    fn only_stale_enabled_hubs_are_due_for_a_background_sync() {
        let (now, recent) = hours_ago(2);
        let (_, old) = hours_ago(30);

        let mut config = HubConfig::default();
        config.add(hub("never", "a/b")).unwrap();
        config.add(hub("fresh", "c/d")).unwrap();
        config.add(hub("stale", "e/f")).unwrap();
        config.add(hub("off", "g/h")).unwrap();
        config.get_mut("fresh").unwrap().last_synced = Some(recent);
        config.get_mut("stale").unwrap().last_synced = Some(old.clone());
        config.get_mut("off").unwrap().last_synced = Some(old);
        config.get_mut("off").unwrap().enabled = false;

        let due: Vec<String> = config
            .due_for_auto_sync(now)
            .into_iter()
            .map(|hub| hub.name)
            .collect();
        assert_eq!(due, ["never", "stale"]);

        // Auto-sync off means the start touches nothing at all.
        config.auto_sync = false;
        assert!(config.due_for_auto_sync(now).is_empty());
    }

    /// A `last_synced` we cannot read is treated as stale rather than as "never refresh".
    #[test]
    fn an_unreadable_timestamp_counts_as_stale() {
        let mut hub = hub("acme", "a/b");
        let now = chrono::Local::now();
        hub.last_synced = Some("last tuesday".to_string());
        assert!(hub.is_stale(AUTO_SYNC_TTL, now));
    }

    /// `hubs.json` from a build before hubs existed, and one written by this build, both
    /// load with auto-sync on rather than silently off.
    #[test]
    fn a_config_without_the_new_fields_defaults_to_auto_sync_on() {
        let config: HubConfig = serde_json::from_str(r#"{"hubs":[]}"#).unwrap();
        assert!(config.auto_sync);
        assert!(!config.official_removed);

        let hub: Hub =
            serde_json::from_str(r#"{"name":"acme","url":"a/b","enabled":true}"#).unwrap();
        assert!(!hub.official);
        assert_eq!(hub.namespace(), Some("acme"));
    }
}
