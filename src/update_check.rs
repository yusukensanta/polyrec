//! Lightweight update check against GitHub Releases — no auto-download/replace,
//! just a "a newer version exists" signal the UI can surface with a link. Failure
//! (no internet, GitHub unreachable, rate-limited, repo has no releases yet) is
//! never surfaced to the user as an error; it just means no update banner shows.

use serde::Deserialize;

const REPO: &str = "yusukensanta/polyrec";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub version: String,
    pub url: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
}

/// Compares a release tag (e.g. "v0.2.0" or "0.2.0") against the running version
/// (e.g. from `env!("CARGO_PKG_VERSION")`). `None` means either string failed to
/// parse as semver — treated the same as "no update" by the caller, not an error.
fn is_newer(current_version: &str, latest_tag: &str) -> Option<bool> {
    let latest = latest_tag.trim_start_matches('v');
    let current = semver::Version::parse(current_version).ok()?;
    let latest = semver::Version::parse(latest).ok()?;
    Some(latest > current)
}

pub async fn check_for_update(current_version: &str) -> Option<AvailableUpdate> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let client = reqwest::Client::builder()
        .user_agent(concat!("polyrec-update-check/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;
    let release: GithubRelease = client.get(&url).send().await.ok()?.json().await.ok()?;

    if is_newer(current_version, &release.tag_name)? {
        Some(AvailableUpdate {
            version: release.tag_name,
            url: release.html_url,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_detects_newer_release() {
        assert_eq!(is_newer("0.1.0", "v0.2.0"), Some(true));
    }

    #[test]
    fn is_newer_rejects_same_version() {
        assert_eq!(is_newer("0.1.0", "v0.1.0"), Some(false));
    }

    #[test]
    fn is_newer_rejects_older_release() {
        assert_eq!(is_newer("0.2.0", "v0.1.0"), Some(false));
    }

    #[test]
    fn is_newer_handles_tag_without_v_prefix() {
        assert_eq!(is_newer("0.1.0", "0.2.0"), Some(true));
    }

    #[test]
    fn is_newer_none_on_unparseable_current_version() {
        assert_eq!(is_newer("not-a-version", "v0.2.0"), None);
    }

    #[test]
    fn is_newer_none_on_unparseable_tag() {
        assert_eq!(is_newer("0.1.0", "latest"), None);
    }

    /// Hits the real GitHub API. Ignored by default since it needs network — run
    /// manually with `--ignored --nocapture`. Doesn't assert `Some`/`None` since
    /// that depends on whether this repo has a published release at run time
    /// (currently it doesn't — GET .../releases/latest 404s, which this function
    /// treats as "no update" by design); this just proves the request completes
    /// and the client is built correctly, without panicking or hanging.
    #[tokio::test]
    #[ignore]
    async fn check_for_update_reaches_real_github_api() {
        let result = check_for_update("0.0.0").await;
        println!("check_for_update result: {result:?}");
    }
}
