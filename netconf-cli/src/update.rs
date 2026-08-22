use flate2::read::GzDecoder;
use log::info;
use netconf_async::error::{NetconfClientError, NetconfClientResult};
use reqwest::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tar::Archive;

pub const DEFAULT_OWNER: &str = "Mrflatt";
pub const DEFAULT_REPO: &str = "netconf-rust";
pub const BIN_NAME: &str = "netconf-cli";
pub const TAG_PREFIX: &str = "netconf-cli-v";
const DEFAULT_API: &str = "https://api.github.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: Version,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    pub download_url: String,
    pub digest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Polled {
    pub current: Version,
    pub latest: Release,
}

impl Polled {
    pub fn update_available(&self) -> bool {
        self.latest.version > self.current
    }
}

pub struct ReleasePoller {
    client: Client,
    owner: String,
    repo: String,
    api_base: String,
}

impl ReleasePoller {
    pub fn new(owner: impl Into<String>, repo: impl Into<String>) -> NetconfClientResult<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&format!("{BIN_NAME}/{}", env!("CARGO_PKG_VERSION")))
                .map_err(|err| NetconfClientError::new(format!("invalid User-Agent: {err}")))?,
        );
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        if let Some(token) = github_token() {
            let value = format!("Bearer {token}");
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&value).map_err(|err| {
                    NetconfClientError::new(format!("invalid GitHub token: {err}"))
                })?,
            );
        }
        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|err| NetconfClientError::new(format!("http client: {err}")))?;
        Ok(Self {
            client,
            owner: owner.into(),
            repo: repo.into(),
            api_base: DEFAULT_API.to_string(),
        })
    }

    pub fn with_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.api_base = api_base.into();
        self
    }

    pub async fn cli_releases(&self) -> NetconfClientResult<Vec<Release>> {
        let url = format!(
            "{}/repos/{}/{}/releases?per_page=100",
            self.api_base, self.owner, self.repo
        );
        let response =
            self.client.get(&url).send().await.map_err(|err| {
                NetconfClientError::new(format!("GitHub releases request: {err}"))
            })?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| NetconfClientError::new(format!("GitHub releases body: {err}")))?;
        if !status.is_success() {
            let hint = if status.as_u16() == 403 || status.as_u16() == 429 {
                " (set GH_TOKEN or NETCONF_GITHUB_TOKEN to raise the rate limit)"
            } else {
                ""
            };
            return Err(NetconfClientError::new(format!(
                "GitHub API {status}{hint}: {}",
                truncate(&body, 300)
            )));
        }
        parse_cli_releases(&body)
    }

    pub async fn download(&self, url: &str) -> NetconfClientResult<Vec<u8>> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|err| NetconfClientError::new(format!("download {url}: {err}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(NetconfClientError::new(format!(
                "download {url} failed: {status}"
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|err| NetconfClientError::new(format!("download {url} body: {err}")))?;
        Ok(bytes.to_vec())
    }
}

pub struct Updater {
    poller: ReleasePoller,
    current_version: Version,
    target: String,
}

impl Updater {
    pub fn from_env() -> NetconfClientResult<Self> {
        let repo = std::env::var("NETCONF_RELEASE_REPO").ok();
        let (owner, repo) = parse_repo(repo.as_deref())?;
        let mut poller = ReleasePoller::new(owner, repo)?;
        if let Ok(base) = std::env::var("NETCONF_GITHUB_API")
            && !base.is_empty()
        {
            poller = poller.with_api_base(base);
        }
        Self::new(poller, env!("CARGO_PKG_VERSION"), current_target())
    }

    pub fn new(
        poller: ReleasePoller,
        current_version: &str,
        target: impl Into<String>,
    ) -> NetconfClientResult<Self> {
        Ok(Self {
            poller,
            current_version: parse_cli_version(current_version)?,
            target: target.into(),
        })
    }

    pub async fn poll(&self) -> NetconfClientResult<Polled> {
        let releases = self.poller.cli_releases().await?;
        let Some(latest) = latest_cli_release(&releases).cloned() else {
            return Err(NetconfClientError::new(
                "no netconf-cli GitHub releases found".to_string(),
            ));
        };
        Ok(Polled {
            current: self.current_version.clone(),
            latest,
        })
    }

    pub async fn apply(&self, release: &Release) -> NetconfClientResult<()> {
        let archive = self.fetch_and_verify(release).await?;
        let tmp = tempfile::tempdir()
            .map_err(|err| NetconfClientError::new(format!("temp dir for update: {err}")))?;
        let bin = extract_binary(&archive, tmp.path())?;
        replace_current(&bin)?;
        Ok(())
    }

    pub async fn fetch_and_verify(&self, release: &Release) -> NetconfClientResult<Vec<u8>> {
        let asset = matching_asset(&release.assets, &release.version.to_string(), &self.target)
            .ok_or_else(|| {
                NetconfClientError::new(format!(
                    "no binary for target '{}' in {}",
                    self.target, release.tag
                ))
            })?;
        let expected = parse_github_digest(asset.digest.as_deref())?;
        info!("Downloading {} ({} bytes)", asset.name, asset.size);
        let archive = self.poller.download(&asset.download_url).await?;
        verify_digest(&archive, &asset.name, &expected)?;
        Ok(archive)
    }
}

pub fn current_target() -> &'static str {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        "aarch64-pc-windows-msvc"
    } else {
        "unknown"
    }
}

pub fn archive_name(version: &str, target: &str) -> String {
    format!("{BIN_NAME}-{version}-{target}.tar.gz")
}

pub fn parse_repo(raw: Option<&str>) -> NetconfClientResult<(String, String)> {
    match raw {
        None | Some("") => Ok((DEFAULT_OWNER.to_string(), DEFAULT_REPO.to_string())),
        Some(value) => {
            let Some((owner, repo)) = value.split_once('/') else {
                return Err(NetconfClientError::new(format!(
                    "invalid NETCONF_RELEASE_REPO '{value}' (expected owner/repo)"
                )));
            };
            if owner.is_empty() || repo.is_empty() {
                return Err(NetconfClientError::new(format!(
                    "invalid NETCONF_RELEASE_REPO '{value}' (expected owner/repo)"
                )));
            }
            Ok((owner.to_string(), repo.to_string()))
        }
    }
}

pub fn parse_cli_version(raw: &str) -> NetconfClientResult<Version> {
    Version::parse(raw.trim().trim_start_matches('v'))
        .map_err(|err| NetconfClientError::new(format!("invalid version '{raw}': {err}")))
}

pub fn parse_cli_tag_version(tag: &str) -> NetconfClientResult<Version> {
    let stripped = tag
        .strip_prefix(TAG_PREFIX)
        .or_else(|| tag.strip_prefix("netconf-cli-"))
        .unwrap_or(tag);
    parse_cli_version(stripped)
}

pub fn latest_cli_release(releases: &[Release]) -> Option<&Release> {
    releases.iter().max_by(|a, b| a.version.cmp(&b.version))
}

pub fn matching_asset<'a>(assets: &'a [Asset], version: &str, target: &str) -> Option<&'a Asset> {
    let expected = archive_name(version, target);
    assets
        .iter()
        .find(|asset| asset.name == expected)
        .or_else(|| {
            assets
                .iter()
                .find(|asset| asset.name.contains(target) && asset.name.ends_with(".tar.gz"))
        })
}

pub fn parse_github_digest(digest: Option<&str>) -> NetconfClientResult<String> {
    let Some(digest) = digest.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(NetconfClientError::new(
            "release asset has no GitHub digest".to_string(),
        ));
    };
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(NetconfClientError::new(format!(
            "invalid GitHub digest '{digest}'"
        )));
    }
    Ok(hex.to_ascii_lowercase())
}

pub fn verify_digest(bytes: &[u8], name: &str, expected: &str) -> NetconfClientResult<()> {
    let actual = hex_encode(&Sha256::digest(bytes));
    if actual != expected {
        return Err(NetconfClientError::new(format!(
            "sha256 mismatch for {name}: expected {expected}, got {actual}"
        )));
    }
    Ok(())
}

pub fn extract_binary(archive: &[u8], dest_dir: &Path) -> NetconfClientResult<PathBuf> {
    let decoder = GzDecoder::new(archive);
    let mut tar = Archive::new(decoder);
    tar.unpack(dest_dir)
        .map_err(|err| NetconfClientError::new(format!("extract archive: {err}")))?;

    let unix = dest_dir.join(BIN_NAME);
    let windows = dest_dir.join(format!("{BIN_NAME}.exe"));
    let path = if unix.is_file() {
        unix
    } else if windows.is_file() {
        windows
    } else {
        return Err(NetconfClientError::new(format!(
            "archive has no {BIN_NAME} binary"
        )));
    };
    set_executable(&path)?;
    Ok(path)
}

pub fn replace_current(new_bin: &Path) -> NetconfClientResult<()> {
    self_replace::self_replace(new_bin)
        .map_err(|err| NetconfClientError::new(format!("replace running binary: {err}")))
}

fn parse_cli_releases(body: &str) -> NetconfClientResult<Vec<Release>> {
    let parsed: Vec<GitHubRelease> = serde_json::from_str(body)
        .map_err(|err| NetconfClientError::new(format!("decode GitHub releases: {err}")))?;
    let mut releases = Vec::new();
    for item in parsed {
        if item.draft || item.prerelease {
            continue;
        }
        if !item.tag_name.starts_with("netconf-cli-") {
            continue;
        }
        let Ok(version) = parse_cli_tag_version(&item.tag_name) else {
            continue;
        };
        releases.push(Release {
            tag: item.tag_name,
            version,
            assets: item
                .assets
                .into_iter()
                .map(|asset| Asset {
                    name: asset.name,
                    size: asset.size,
                    download_url: asset.browser_download_url,
                    digest: asset.digest,
                })
                .collect(),
        });
    }
    Ok(releases)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn github_token() -> Option<String> {
    ["NETCONF_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()))
}

fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(max).collect();
    format!("{cut}...")
}

fn set_executable(path: &Path) -> NetconfClientResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)
            .map_err(|err| NetconfClientError::new(format!("stat {path:?}: {err}")))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)
            .map_err(|err| NetconfClientError::new(format!("chmod {path:?}: {err}")))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    size: u64,
    browser_download_url: String,
    digest: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const RELEASES_JSON: &str = r#"
    [
      {
        "tag_name": "netconf-async-v0.3.0",
        "draft": false,
        "prerelease": false,
        "assets": []
      },
      {
        "tag_name": "netconf-cli-v0.1.0",
        "draft": false,
        "prerelease": false,
        "assets": [
          {
            "name": "netconf-cli-0.1.0-x86_64-unknown-linux-gnu.tar.gz",
            "size": 10,
            "browser_download_url": "http://example.test/old.tar.gz"
          }
        ]
      },
      {
        "tag_name": "netconf-cli-v0.2.0",
        "draft": false,
        "prerelease": false,
        "assets": [
          {
            "name": "netconf-cli-0.2.0-x86_64-unknown-linux-gnu.tar.gz",
            "size": 20,
            "browser_download_url": "http://example.test/new.tar.gz",
            "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
          }
        ]
      },
      {
        "tag_name": "netconf-cli-v0.3.0-rc.1",
        "draft": false,
        "prerelease": true,
        "assets": []
      },
      {
        "tag_name": "netconf-cli-v9.9.9",
        "draft": true,
        "prerelease": false,
        "assets": []
      }
    ]
    "#;

    #[test]
    fn parse_cli_tag_version_variants() {
        assert_eq!(
            parse_cli_tag_version("netconf-cli-v0.2.0").unwrap(),
            Version::parse("0.2.0").unwrap()
        );
        assert_eq!(
            parse_cli_tag_version("netconf-cli-0.2.0").unwrap(),
            Version::parse("0.2.0").unwrap()
        );
        assert_eq!(
            parse_cli_tag_version("v0.2.0").unwrap(),
            Version::parse("0.2.0").unwrap()
        );
        assert_eq!(
            parse_cli_tag_version("0.2.0").unwrap(),
            Version::parse("0.2.0").unwrap()
        );
        assert!(parse_cli_tag_version("netconf-cli-vnot-a-version").is_err());
    }

    #[test]
    fn parse_repo_default_and_override() {
        assert_eq!(
            parse_repo(None).unwrap(),
            (DEFAULT_OWNER.to_string(), DEFAULT_REPO.to_string())
        );
        assert_eq!(
            parse_repo(Some("")).unwrap(),
            (DEFAULT_OWNER.to_string(), DEFAULT_REPO.to_string())
        );
        assert_eq!(
            parse_repo(Some("acme/netconf")).unwrap(),
            ("acme".to_string(), "netconf".to_string())
        );
        assert!(parse_repo(Some("noslash")).is_err());
        assert!(parse_repo(Some("/repo")).is_err());
        assert!(parse_repo(Some("owner/")).is_err());
    }

    #[test]
    fn parse_releases_skips_other_packages_and_drafts() {
        let releases = parse_cli_releases(RELEASES_JSON).unwrap();
        let tags: Vec<&str> = releases.iter().map(|r| r.tag.as_str()).collect();
        assert_eq!(tags, ["netconf-cli-v0.1.0", "netconf-cli-v0.2.0"]);
    }

    #[test]
    fn latest_stable_skips_prerelease() {
        let releases = parse_cli_releases(RELEASES_JSON).unwrap();
        let latest = latest_cli_release(&releases).unwrap();
        assert_eq!(latest.version, Version::parse("0.2.0").unwrap());
    }

    #[test]
    fn matching_asset_prefers_exact_name() {
        let releases = parse_cli_releases(RELEASES_JSON).unwrap();
        let release = latest_cli_release(&releases).unwrap();
        let asset = matching_asset(&release.assets, "0.2.0", "x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(
            asset.name,
            "netconf-cli-0.2.0-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert!(matching_asset(&release.assets, "0.2.0", "aarch64-apple-darwin").is_none());
    }

    #[test]
    fn archive_name_format() {
        assert_eq!(
            archive_name("0.2.0", "aarch64-apple-darwin"),
            "netconf-cli-0.2.0-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn current_target_is_set() {
        assert!(!current_target().is_empty());
        assert!(current_target().contains('-'));
    }

    #[test]
    fn parse_and_verify_github_digest() {
        let payload = b"hello-netconf";
        let hex = hex_encode(&Sha256::digest(payload));
        assert_eq!(
            parse_github_digest(Some(&format!("sha256:{hex}"))).unwrap(),
            hex
        );
        assert_eq!(parse_github_digest(Some(&hex)).unwrap(), hex);
        assert!(parse_github_digest(None).is_err());
        assert!(parse_github_digest(Some("")).is_err());
        assert!(parse_github_digest(Some("sha256:abc")).is_err());
        verify_digest(payload, "asset.tar.gz", &hex).unwrap();
        assert!(verify_digest(b"nope", "asset.tar.gz", &hex).is_err());
    }

    #[test]
    fn extract_binary_from_tar_gz() {
        let archive = make_tar_gz(&[(BIN_NAME, b"#!/bin/sh\necho ok\n")]);
        let dir = tempfile::tempdir().unwrap();
        let path = extract_binary(&archive, dir.path()).unwrap();
        assert_eq!(path.file_name().unwrap(), BIN_NAME);
        assert_eq!(std::fs::read(&path).unwrap(), b"#!/bin/sh\necho ok\n");
    }

    #[test]
    fn extract_binary_accepts_exe_name() {
        let archive = make_tar_gz(&[("netconf-cli.exe", b"MZ")]);
        let dir = tempfile::tempdir().unwrap();
        let path = extract_binary(&archive, dir.path()).unwrap();
        assert_eq!(path.file_name().unwrap(), "netconf-cli.exe");
    }

    #[test]
    fn extract_binary_rejects_empty_archive() {
        let archive = make_tar_gz(&[("README.txt", b"nope")]);
        let dir = tempfile::tempdir().unwrap();
        assert!(extract_binary(&archive, dir.path()).is_err());
    }

    #[test]
    fn polled_update_available() {
        let latest = Release {
            tag: "netconf-cli-v0.2.0".into(),
            version: Version::parse("0.2.0").unwrap(),
            assets: vec![],
        };
        let older = Polled {
            current: Version::parse("0.1.0").unwrap(),
            latest: latest.clone(),
        };
        assert!(older.update_available());
        let same = Polled {
            current: Version::parse("0.2.0").unwrap(),
            latest,
        };
        assert!(!same.update_available());
    }

    #[tokio::test]
    async fn poller_fetches_and_filters_releases() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/Mrflatt/netconf-rust/releases"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(RELEASES_JSON, "application/json"),
            )
            .mount(&server)
            .await;

        let poller = ReleasePoller::new(DEFAULT_OWNER, DEFAULT_REPO)
            .unwrap()
            .with_api_base(server.uri());
        let releases = poller.cli_releases().await.unwrap();
        assert_eq!(releases.len(), 2);
        let updater = Updater::new(poller, "0.1.0", "x86_64-unknown-linux-gnu").unwrap();
        let polled = updater.poll().await.unwrap();
        assert!(polled.update_available());
        assert_eq!(polled.latest.version, Version::parse("0.2.0").unwrap());
    }

    #[tokio::test]
    async fn fetch_and_verify_checks_digest() {
        let archive = make_tar_gz(&[(BIN_NAME, b"payload")]);
        let digest = hex_encode(&Sha256::digest(&archive));
        let asset_name = "netconf-cli-0.2.0-x86_64-unknown-linux-gnu.tar.gz";

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/Mrflatt/netconf-rust/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"[{{
                        "tag_name": "netconf-cli-v0.2.0",
                        "draft": false,
                        "prerelease": false,
                        "assets": [
                          {{
                            "name": "{asset_name}",
                            "size": {},
                            "browser_download_url": "{}/bin.tar.gz",
                            "digest": "sha256:{digest}"
                          }}
                        ]
                    }}]"#,
                    archive.len(),
                    server.uri()
                ),
                "application/json",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/bin.tar.gz"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(archive.clone()))
            .mount(&server)
            .await;

        let poller = ReleasePoller::new(DEFAULT_OWNER, DEFAULT_REPO)
            .unwrap()
            .with_api_base(server.uri());
        let updater = Updater::new(poller, "0.1.0", "x86_64-unknown-linux-gnu").unwrap();
        let polled = updater.poll().await.unwrap();
        let bytes = updater.fetch_and_verify(&polled.latest).await.unwrap();
        assert_eq!(bytes, archive);
    }

    #[tokio::test]
    async fn poller_maps_http_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/Mrflatt/netconf-rust/releases"))
            .respond_with(ResponseTemplate::new(403).set_body_string("rate limited"))
            .mount(&server)
            .await;
        let poller = ReleasePoller::new(DEFAULT_OWNER, DEFAULT_REPO)
            .unwrap()
            .with_api_base(server.uri());
        let err = poller.cli_releases().await.unwrap_err().to_string();
        assert!(err.contains("403"), "{err}");
        assert!(err.contains("GH_TOKEN"), "{err}");
    }

    fn make_tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar_buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            for (name, data) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder.append_data(&mut header, name, *data).unwrap();
            }
            builder.finish().unwrap();
        }
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&tar_buf).unwrap();
        encoder.finish().unwrap()
    }
}
