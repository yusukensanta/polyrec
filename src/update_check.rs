//! Lightweight update check against GitHub Releases — no auto-download/replace,
//! just a "a newer version exists" signal the UI can surface with a link, for a
//! newer release whose checksums are signed with the release key (two small
//! files fetched to check that). Failure (no internet, GitHub unreachable,
//! rate-limited, repo has no releases yet, release not signed) is never
//! surfaced to the user as an error; it just means no update banner shows.

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
}

/// Compares a release tag (e.g. "v0.2.0" or "0.2.0") against the running version
/// (e.g. from `env!("CARGO_PKG_VERSION")`). `None` means either string failed to
/// parse as semver — treated the same as "no update" by the caller, not an error.
fn is_newer(current_version: &str, latest_tag: &str) -> Option<bool> {
    let current = semver::Version::parse(current_version).ok()?;
    let latest = tag_version(latest_tag)?;
    Some(latest > current)
}

/// The version a release tag (`"v0.2.0"` or `"0.2.0"`) names, if it names
/// one.
pub(crate) fn tag_version(tag: &str) -> Option<semver::Version> {
    semver::Version::parse(tag.trim_start_matches('v')).ok()
}

/// The update to offer for the newest release, `tag`: only one newer than
/// `current_version` whose checksums carry the release key's signature
/// (`signed`). PolyRec wouldn't install any other, so the banner doesn't
/// advertise one -- or point to its page, from which it could be
/// downloaded by hand.
fn offered_update(current_version: &str, tag: &str, signed: bool) -> Option<AvailableUpdate> {
    (signed && is_newer(current_version, tag)?).then(|| AvailableUpdate {
        version: tag.to_string(),
        url: format!("https://github.com/{REPO}/releases/tag/{tag}"),
    })
}

pub async fn check_for_update(current_version: &str) -> Option<AvailableUpdate> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let client = reqwest::Client::builder()
        .user_agent(concat!("polyrec-update-check/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;
    let release: GithubRelease = client.get(&url).send().await.ok()?.json().await.ok()?;
    if !is_newer(current_version, &release.tag_name)? {
        return None;
    }
    let signed = match crate::self_update::fetch_signed_sums(&release.tag_name).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("not offering {}: {e}", release.tag_name);
            false
        }
    };
    offered_update(current_version, &release.tag_name, signed)
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

    #[test]
    fn a_newer_signed_release_is_offered_with_its_own_page() {
        assert_eq!(
            offered_update("0.5.160", "v0.5.161", true),
            Some(AvailableUpdate {
                version: "v0.5.161".into(),
                url: "https://github.com/yusukensanta/polyrec/releases/tag/v0.5.161".into(),
            })
        );
    }

    /// Published without the release key's signature -- by someone in the
    /// maintainer's GitHub account, say: Update Now would refuse it, and its
    /// page shouldn't be pointed to either.
    #[test]
    fn a_newer_release_that_isnt_signed_isnt_offered() {
        assert_eq!(offered_update("0.5.160", "v0.5.161", false), None);
    }

    #[test]
    fn an_older_signed_release_isnt_offered() {
        assert_eq!(offered_update("0.5.161", "v0.5.160", true), None);
    }

    /// Hits the real GitHub API. Ignored by default since it needs network — run
    /// manually with `--ignored --nocapture`. Doesn't assert `Some`/`None` since
    /// that depends on the latest published release at run time (a repo with
    /// none 404s, which this function treats as "no update" by design); this
    /// just proves the request completes and the client is built correctly,
    /// without panicking or hanging.
    #[tokio::test]
    #[ignore]
    async fn check_for_update_reaches_real_github_api() {
        let result = check_for_update("0.0.0").await;
        println!("check_for_update result: {result:?}");
    }
}
