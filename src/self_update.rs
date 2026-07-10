//! Performs the actual update once the user clicks "Update Now" on the
//! version banner `update_check` surfaced -- downloads the right release
//! asset for how this copy of PolyRec is installed, verifies its SHA256
//! against the release's published `SHA256SUMS.txt` (never skipped: this is
//! the only thing standing between a compromised/corrupted download and
//! either replacing the running executable or launching an installer), then
//! either swaps the running exe in place (portable) or silently re-runs the
//! installer (installed via Inno Setup).

use crate::error::AppError;
use sha2::{Digest, Sha256};
use std::path::Path;

const REPO: &str = "yusukensanta/polyrec";

/// Inno Setup always writes this next to the installed exe -- its presence
/// is the simplest reliable "was this installed via the .exe installer, or
/// just unzipped" signal, no registry lookup needed.
const UNINSTALLER_MARKER: &str = "unins000.exe";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    /// A bare `polyrec.exe`, unzipped somewhere -- no installer, no
    /// uninstaller, no registry entries.
    Portable,
    /// Installed via the Inno Setup installer (Program Files, uninstaller,
    /// Start Menu shortcut, Add/Remove Programs entry).
    Installed,
}

fn detect_install_kind_at(exe_path: &Path) -> InstallKind {
    let has_uninstaller = exe_path
        .parent()
        .is_some_and(|dir| dir.join(UNINSTALLER_MARKER).exists());
    if has_uninstaller {
        InstallKind::Installed
    } else {
        InstallKind::Portable
    }
}

pub fn detect_install_kind() -> Result<InstallKind, AppError> {
    let exe = std::env::current_exe()
        .map_err(|e| AppError::Update(format!("current_exe: {e}")))?;
    Ok(detect_install_kind_at(&exe))
}

/// Parses `SHA256SUMS.txt` (one `<hex-hash>  <filename>` line per asset, the
/// exact format `sha256sum`/CI's "Generate checksums" step produces) and
/// returns the hash for `filename`, if present. Tolerant of the filename
/// appearing with either one or more spaces/tabs between hash and name
/// (`sha256sum`'s own output uses two spaces for binary mode, one for text
/// mode) and of trailing whitespace/blank lines.
fn parse_sha256sums(text: &str, filename: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?;
        // sha256sum text-mode entries can be prefixed with "*" for binary mode.
        (name.trim_start_matches('*') == filename).then(|| hash.to_ascii_lowercase())
    })
}

fn verify_sha256(data: &[u8], expected_hex: &str) -> Result<(), AppError> {
    let actual = hex_encode(Sha256::digest(data).as_slice());
    if actual.eq_ignore_ascii_case(expected_hex) {
        Ok(())
    } else {
        Err(AppError::Update(format!(
            "checksum mismatch: expected {expected_hex}, got {actual}"
        )))
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn http_client() -> Result<reqwest::Client, AppError> {
    reqwest::Client::builder()
        .user_agent(concat!("polyrec-self-update/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| AppError::Update(format!("building HTTP client: {e}")))
}

async fn download_bytes(url: &str) -> Result<Vec<u8>, AppError> {
    let client = http_client()?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::Update(format!("GET {url}: {e}")))?
        .error_for_status()
        .map_err(|e| AppError::Update(format!("GET {url}: {e}")))?;
    Ok(resp
        .bytes()
        .await
        .map_err(|e| AppError::Update(format!("reading body of {url}: {e}")))?
        .to_vec())
}

fn release_asset_url(version_tag: &str, filename: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/{version_tag}/{filename}")
}

async fn fetch_expected_sha256(version_tag: &str, filename: &str) -> Result<String, AppError> {
    let sums_url = release_asset_url(version_tag, "SHA256SUMS.txt");
    let bytes = download_bytes(&sums_url).await?;
    let text = String::from_utf8(bytes)
        .map_err(|e| AppError::Update(format!("SHA256SUMS.txt wasn't valid UTF-8: {e}")))?;
    parse_sha256sums(&text, filename)
        .ok_or_else(|| AppError::Update(format!("no checksum entry for {filename} in SHA256SUMS.txt")))
}

#[derive(serde::Deserialize)]
struct AttestationsResponse {
    #[serde(default)]
    attestations: Vec<serde::de::IgnoredAny>,
}

/// Confirms GitHub has a build-provenance attestation on file for this exact
/// artifact (see `release.yml`'s "Attest build provenance" step, which runs
/// unconditionally on every release regardless of SignPath's signing
/// status) -- ties the downloaded bytes back to a specific GitHub Actions
/// run/commit that produced them, on top of the `SHA256SUMS.txt` check in
/// `download_and_verify`. An attacker able to replace both the release
/// asset and `SHA256SUMS.txt` (the one scenario the checksum check alone
/// can't catch) would also have to forge or delete this attestation record,
/// which lives in GitHub's own attestation store rather than the release
/// itself.
///
/// Deliberately advisory, not blocking: rejecting a real update every time
/// this lookup has a network hiccup or GitHub API rate limit would be worse
/// than the marginal coverage this check adds on top of the hash check
/// above. Only logged, never returned as an error.
async fn check_build_attestation(digest_hex: &str) {
    let url = format!("https://api.github.com/repos/{REPO}/attestations/sha256:{digest_hex}");
    let Ok(client) = http_client() else { return };
    match client.get(&url).header("Accept", "application/vnd.github+json").send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<AttestationsResponse>().await {
            Ok(parsed) if parsed.attestations.is_empty() => {
                tracing::warn!(
                    "no GitHub build-provenance attestation found for sha256:{digest_hex} -- proceeding on checksum verification alone"
                );
            }
            Ok(_) => tracing::info!("build-provenance attestation confirmed for sha256:{digest_hex}"),
            Err(e) => tracing::warn!("could not parse attestation lookup response: {e}"),
        },
        Ok(resp) => {
            tracing::warn!("attestation lookup returned {}: proceeding on checksum verification alone", resp.status());
        }
        Err(e) => tracing::warn!("attestation lookup failed: {e} -- proceeding on checksum verification alone"),
    }
}

/// Downloads `filename` for `version_tag` and verifies it against the
/// release's published `SHA256SUMS.txt` before returning its bytes. This is
/// the single gate every self-update path goes through -- nothing
/// downstream ever sees unverified bytes. Also checks (advisory-only, see
/// `check_build_attestation`) for a matching GitHub build-provenance
/// attestation as defense-in-depth against a compromised release.
async fn download_and_verify(version_tag: &str, filename: &str) -> Result<Vec<u8>, AppError> {
    let expected = fetch_expected_sha256(version_tag, filename).await?;
    let bytes = download_bytes(&release_asset_url(version_tag, filename)).await?;
    verify_sha256(&bytes, &expected)?;
    check_build_attestation(&expected).await;
    Ok(bytes)
}

/// Extracts the single `polyrec.exe` entry from a downloaded release zip
/// (which also contains README.md/LICENSE, see `release.yml`'s "Package
/// zip" step -- everything else is ignored).
fn extract_exe_from_zip(zip_bytes: &[u8]) -> Result<Vec<u8>, AppError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| AppError::Update(format!("opening update zip: {e}")))?;
    let mut file = archive
        .by_name("polyrec.exe")
        .map_err(|e| AppError::Update(format!("update zip has no polyrec.exe: {e}")))?;
    let mut out = Vec::with_capacity(file.size() as usize);
    std::io::copy(&mut file, &mut out)
        .map_err(|e| AppError::Update(format!("reading polyrec.exe from update zip: {e}")))?;
    Ok(out)
}

/// Performs the update for `version_tag` (e.g. `"v0.5.0"`, matching
/// `update_check::AvailableUpdate::version`/the release's git tag). On
/// success, the running exe has already been replaced and a fresh process
/// spawned (portable), or the installer has already been launched
/// (installed) -- the caller's only remaining job is to close its own
/// window.
pub async fn perform_self_update(version_tag: String) -> Result<(), AppError> {
    match detect_install_kind()? {
        InstallKind::Portable => update_portable(&version_tag).await,
        InstallKind::Installed => update_installed(&version_tag).await,
    }
}

async fn update_portable(version_tag: &str) -> Result<(), AppError> {
    let zip_filename = format!("polyrec-{version_tag}-windows-x64.zip");
    let zip_bytes = download_and_verify(version_tag, &zip_filename).await?;
    let exe_bytes = extract_exe_from_zip(&zip_bytes)?;

    let temp_path = std::env::temp_dir().join(format!("polyrec-update-{version_tag}.exe"));
    std::fs::write(&temp_path, &exe_bytes)
        .map_err(|e| AppError::Update(format!("writing downloaded exe to {}: {e}", temp_path.display())))?;

    self_replace::self_replace(&temp_path)
        .map_err(|e| AppError::Update(format!("self_replace failed: {e}")))?;
    let _ = std::fs::remove_file(&temp_path);

    let current_exe = std::env::current_exe()
        .map_err(|e| AppError::Update(format!("current_exe after self_replace: {e}")))?;
    std::process::Command::new(&current_exe)
        .spawn()
        .map_err(|e| AppError::Update(format!("spawning updated exe: {e}")))?;
    Ok(())
}

async fn update_installed(version_tag: &str) -> Result<(), AppError> {
    let setup_filename = format!("polyrec-{version_tag}-windows-x64-setup.exe");
    let setup_bytes = download_and_verify(version_tag, &setup_filename).await?;

    let temp_path = std::env::temp_dir().join(format!("polyrec-update-{version_tag}-setup.exe"));
    std::fs::write(&temp_path, &setup_bytes)
        .map_err(|e| AppError::Update(format!("writing installer to {}: {e}", temp_path.display())))?;

    // /VERYSILENT+/SUPPRESSMSGBOXES suppress Inno's own wizard UI; they do
    // NOT suppress the OS-level UAC elevation prompt (independent, driven by
    // the installer's manifest, not something these flags touch) -- that
    // prompt is expected. /NORESTART since a PolyRec update never needs a
    // reboot. We deliberately don't wait for this to finish or track/relaunch
    // the app ourselves -- installer/polyrec.iss's RestartApplications=yes
    // handles reopening PolyRec once the (possibly UAC-delayed) install
    // completes, since we can't reliably wait through an elevation prompt.
    std::process::Command::new(&temp_path)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        .spawn()
        .map_err(|e| AppError::Update(format!("launching installer: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sha256sums_finds_matching_line() {
        let text = "abc123  polyrec-v0.4.0-windows-x64.zip\ndef456  polyrec-v0.4.0-windows-x64-setup.exe\n";
        assert_eq!(
            parse_sha256sums(text, "polyrec-v0.4.0-windows-x64.zip"),
            Some("abc123".to_string())
        );
        assert_eq!(
            parse_sha256sums(text, "polyrec-v0.4.0-windows-x64-setup.exe"),
            Some("def456".to_string())
        );
    }

    #[test]
    fn parse_sha256sums_returns_none_for_unknown_filename() {
        let text = "abc123  polyrec-v0.4.0-windows-x64.zip\n";
        assert_eq!(parse_sha256sums(text, "nonexistent.zip"), None);
    }

    #[test]
    fn parse_sha256sums_ignores_malformed_lines() {
        let text = "not-a-valid-line\n\nabc123  real-file.zip\n";
        assert_eq!(parse_sha256sums(text, "real-file.zip"), Some("abc123".to_string()));
    }

    #[test]
    fn parse_sha256sums_handles_binary_mode_asterisk_prefix() {
        // `sha256sum` prefixes the filename with "*" in binary mode.
        let text = "abc123  *real-file.zip\n";
        assert_eq!(parse_sha256sums(text, "real-file.zip"), Some("abc123".to_string()));
    }

    #[test]
    fn parse_sha256sums_is_case_insensitive_on_the_hash() {
        let text = "ABC123  real-file.zip\n";
        assert_eq!(parse_sha256sums(text, "real-file.zip"), Some("abc123".to_string()));
    }

    #[test]
    fn verify_sha256_accepts_matching_hash() {
        let data = b"hello world";
        let expected = hex_encode(Sha256::digest(data).as_slice());
        assert!(verify_sha256(data, &expected).is_ok());
    }

    #[test]
    fn verify_sha256_accepts_different_case() {
        let data = b"hello world";
        let expected = hex_encode(Sha256::digest(data).as_slice()).to_uppercase();
        assert!(verify_sha256(data, &expected).is_ok());
    }

    #[test]
    fn verify_sha256_rejects_mismatched_hash() {
        let data = b"hello world";
        assert!(verify_sha256(data, "0000000000000000000000000000000000000000000000000000000000000").is_err());
    }

    #[test]
    fn detect_install_kind_is_portable_without_uninstaller() {
        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("polyrec.exe");
        assert_eq!(detect_install_kind_at(&exe_path), InstallKind::Portable);
    }

    #[test]
    fn detect_install_kind_is_installed_with_uninstaller_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("unins000.exe"), b"fake").unwrap();
        let exe_path = dir.path().join("polyrec.exe");
        assert_eq!(detect_install_kind_at(&exe_path), InstallKind::Installed);
    }

    #[test]
    fn release_asset_url_matches_github_releases_download_convention() {
        assert_eq!(
            release_asset_url("v0.4.0", "polyrec-v0.4.0-windows-x64.zip"),
            "https://github.com/yusukensanta/polyrec/releases/download/v0.4.0/polyrec-v0.4.0-windows-x64.zip"
        );
    }

    /// End-to-end against the real, already-published v0.4.1 release:
    /// downloads the real zip and SHA256SUMS.txt over the network, verifies
    /// the real checksum, and extracts the real polyrec.exe entry -- proving
    /// every custom bit of logic in this file (URL construction,
    /// SHA256SUMS.txt parsing against CI's actual output format, zip
    /// extraction against CI's actual archive) against genuine GitHub data.
    /// Deliberately does NOT call self_replace/spawn a process -- that
    /// mechanism was validated separately, live, in complete isolation (a
    /// throwaway scratch binary pair, not this repo's own exe), since doing
    /// it here would mean replacing the test binary's own exe file. Needs
    /// network access, so it's ignored by default -- run with
    /// `--ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn download_and_verify_and_extract_against_a_real_published_release() {
        let zip_bytes = download_and_verify("v0.4.1", "polyrec-v0.4.1-windows-x64.zip")
            .await
            .expect("download_and_verify failed against the real v0.4.1 release");
        println!("downloaded {} bytes", zip_bytes.len());

        let exe_bytes = extract_exe_from_zip(&zip_bytes).expect("extract_exe_from_zip failed");
        println!("extracted polyrec.exe: {} bytes", exe_bytes.len());
        assert!(exe_bytes.len() > 1_000_000, "extracted exe suspiciously small");
        // "MZ" -- the DOS header magic bytes every valid Windows PE starts with.
        assert_eq!(&exe_bytes[0..2], b"MZ", "extracted file isn't a valid PE executable");
    }
}
