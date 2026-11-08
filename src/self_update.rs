//! Performs the actual update once the user clicks "Update Now" on the
//! version banner `update_check` surfaced -- downloads the right release
//! asset for how this copy of PolyRec is installed, verifies its SHA256
//! against the release's published `SHA256SUMS.txt` -- which has to be
//! signed with the release key (`verify_signed_sums`) -- and requires
//! GitHub's build-provenance attestation saying PolyRec's own release
//! workflow built it (none of it ever skipped: it's what stands between a
//! compromised or corrupted download and either replacing the running
//! executable or launching an installer), then either swaps the running exe
//! in place (portable) or silently re-runs the installer (installed via
//! Inno Setup).

use crate::error::{AppError, ResultExt};
use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::{Path, PathBuf};

const REPO: &str = "yusukensanta/polyrec";

/// The workflow that builds and publishes releases, as GitHub's
/// build-provenance attestations name it.
const RELEASE_WORKFLOW: &str = ".github/workflows/release.yml";

/// The in-toto predicate type of a GitHub Actions build-provenance
/// attestation.
const SLSA_PROVENANCE_V1: &str = "https://slsa.dev/provenance/v1";

/// Far more than any release file (about 13 MB, the exe inside it a little
/// over twice that): a download, or a zip entry, bigger than this is
/// refused rather than read into memory.
const MAX_UPDATE_BYTES: u64 = 256 * 1024 * 1024;

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
    let exe = std::env::current_exe().update_ctx("current_exe")?;
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

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// GitHub itself: github.com, its API, and where release files are served
/// from (`*.githubusercontent.com`). The only hosts an update follows a
/// redirect to.
fn is_github_host(host: &str) -> bool {
    host == "github.com" || host == "api.github.com" || host.ends_with(".githubusercontent.com")
}

fn http_client() -> Result<reqwest::Client, AppError> {
    reqwest::Client::builder()
        .user_agent(concat!("polyrec-self-update/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else if attempt.url().host_str().is_some_and(is_github_host) {
                attempt.follow()
            } else {
                let to = attempt.url().to_string();
                attempt.error(format!("redirected away from GitHub, to {to}"))
            }
        }))
        .build()
        .update_ctx("building HTTP client")
}

fn too_big(what: &str) -> AppError {
    AppError::Update(format!(
        "{what} is bigger than any PolyRec release ({MAX_UPDATE_BYTES} bytes)"
    ))
}

async fn download_bytes(url: &str) -> Result<Vec<u8>, AppError> {
    let client = http_client()?;
    let mut resp = client
        .get(url)
        .send()
        .await
        .update_ctx(format_args!("GET {url}"))?
        .error_for_status()
        .update_ctx(format_args!("GET {url}"))?;
    if resp
        .content_length()
        .is_some_and(|len| len > MAX_UPDATE_BYTES)
    {
        return Err(too_big(url));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .update_ctx(format_args!("reading body of {url}"))?
    {
        body.extend_from_slice(&chunk);
        if body.len() as u64 > MAX_UPDATE_BYTES {
            return Err(too_big(url));
        }
    }
    Ok(body)
}

fn release_asset_url(version_tag: &str, filename: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/{version_tag}/{filename}")
}

/// The minisign public keys a release's `SHA256SUMS.txt` has to be signed
/// with, either one: the release key every release is signed with, and a
/// backup kept offline should it be lost. Their secret halves are never on
/// GitHub -- the maintainer signs each release on their own machine
/// (`.github/scripts/Sign-Release.ps1`) -- so someone able to publish a
/// release (a stolen login or token, a compromised workflow) still can't
/// make one this installs.
const RELEASE_KEYS: [&str; 2] = [
    include_str!("../keys/polyrec-release.pub"),
    include_str!("../keys/polyrec-release-backup.pub"),
];

/// What a release's signature has to say it's for -- its trusted comment,
/// which the signature covers -- so one release's signed checksums can't
/// be passed off as another's.
fn signed_for(version_tag: &str) -> String {
    format!("polyrec {version_tag}")
}

/// Checks `signature` (a `.minisig` file) is one of `keys`' over `sums`,
/// made for `version_tag`.
fn verify_signed_sums(
    sums: &[u8],
    signature: &str,
    keys: &[&str],
    version_tag: &str,
) -> Result<(), AppError> {
    let signature = minisign_verify::Signature::decode(signature)
        .update_ctx("SHA256SUMS.txt.minisig isn't a minisign signature")?;
    let signed_with_a_key = keys.iter().any(|key| {
        minisign_verify::PublicKey::decode(key)
            .is_ok_and(|key| key.verify(sums, &signature, false).is_ok())
    });
    if !signed_with_a_key {
        return Err(AppError::Update(
            "SHA256SUMS.txt isn't signed with PolyRec's release key".into(),
        ));
    }
    let expected = signed_for(version_tag);
    if signature.trusted_comment() != expected {
        return Err(AppError::Update(format!(
            "SHA256SUMS.txt was signed for {:?}, not {expected:?}",
            signature.trusted_comment()
        )));
    }
    Ok(())
}

/// Downloads `version_tag`'s `SHA256SUMS.txt` and its signature, and
/// returns its text once it's verified to be signed with a release key, for
/// that release (`verify_signed_sums`). `version_tag` goes into URLs: only
/// ever a version (`update_check::tag_version`).
pub(crate) async fn fetch_signed_sums(version_tag: &str) -> Result<String, AppError> {
    let bytes = download_bytes(&release_asset_url(version_tag, "SHA256SUMS.txt")).await?;
    let signature =
        download_bytes(&release_asset_url(version_tag, "SHA256SUMS.txt.minisig")).await?;
    let signature =
        String::from_utf8(signature).update_ctx("SHA256SUMS.txt.minisig wasn't valid UTF-8")?;
    verify_signed_sums(&bytes, &signature, &RELEASE_KEYS, version_tag)?;
    String::from_utf8(bytes).update_ctx("SHA256SUMS.txt wasn't valid UTF-8")
}

async fn fetch_expected_sha256(version_tag: &str, filename: &str) -> Result<String, AppError> {
    let text = fetch_signed_sums(version_tag).await?;
    parse_sha256sums(&text, filename).ok_or_else(|| {
        AppError::Update(format!(
            "no checksum entry for {filename} in SHA256SUMS.txt"
        ))
    })
}

/// The parts of the GitHub API's attestations for a digest that
/// `is_release_build_of` reads: each one's Sigstore bundle.
#[derive(serde::Deserialize)]
struct AttestationsResponse {
    #[serde(default)]
    attestations: Vec<Attestation>,
}

#[derive(serde::Deserialize)]
struct Attestation {
    bundle: Option<Bundle>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bundle {
    dsse_envelope: DsseEnvelope,
    verification_material: VerificationMaterial,
}

#[derive(serde::Deserialize)]
struct DsseEnvelope {
    /// The in-toto `Statement`, as base64 JSON.
    payload: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct VerificationMaterial {
    /// The signing certificate (bundle v0.3).
    certificate: Option<RawCertificate>,
    /// The same, first in a chain (bundles before v0.3).
    x509_certificate_chain: Option<CertificateChain>,
}

#[derive(serde::Deserialize)]
struct CertificateChain {
    #[serde(default)]
    certificates: Vec<RawCertificate>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCertificate {
    /// DER, as base64.
    raw_bytes: String,
}

impl Bundle {
    /// The DER certificate the attestation was signed with.
    fn signing_certificate(&self) -> Option<Vec<u8>> {
        let material = &self.verification_material;
        let raw = material.certificate.as_ref().or_else(|| {
            material
                .x509_certificate_chain
                .as_ref()
                .and_then(|chain| chain.certificates.first())
        })?;
        base64::engine::general_purpose::STANDARD
            .decode(&raw.raw_bytes)
            .ok()
    }

    fn statement(&self) -> Option<Statement> {
        let json = base64::engine::general_purpose::STANDARD
            .decode(&self.dsse_envelope.payload)
            .ok()?;
        serde_json::from_slice(&json).ok()
    }
}

/// What an attestation says: which files (`subject`) it's about, and what
/// about them (`predicate`, of `predicate_type`).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Statement {
    predicate_type: String,
    #[serde(default)]
    subject: Vec<Subject>,
    #[serde(default)]
    predicate: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct Subject {
    #[serde(default)]
    name: String,
    digest: SubjectDigest,
}

#[derive(serde::Deserialize)]
struct SubjectDigest {
    sha256: Option<String>,
}

/// The identity GitHub's signing certificate gives the release workflow run
/// for `version_tag` -- its Subject Alternative Name.
fn release_workflow_identity(version_tag: &str) -> String {
    format!("https://github.com/{REPO}/{RELEASE_WORKFLOW}@refs/tags/{version_tag}")
}

/// Whether DER certificate `der` has `uri` among its Subject Alternative
/// Names: as a GeneralName's uniformResourceIdentifier -- tag `[6]` (0x86)
/// and its length, then the URI -- so a longer URI that starts the same
/// (`...@refs/tags/v0.5.139` for `...@refs/tags/v0.5.13`) doesn't count.
fn has_san_uri(der: &[u8], uri: &str) -> bool {
    let mut needle = vec![0x86];
    match uri.len() {
        len @ 0..0x80 => needle.push(len as u8),
        len @ 0x80..0x100 => needle.extend([0x81, len as u8]),
        _ => return false,
    }
    needle.extend_from_slice(uri.as_bytes());
    der.windows(needle.len()).any(|window| window == needle)
}

/// Whether `attestation` is the build provenance PolyRec's release workflow,
/// run for `version_tag` in this repo, recorded for `filename` with SHA-256
/// `digest_hex`: a SLSA provenance statement about that file, naming that
/// workflow, repo and tag, signed with the certificate GitHub gives that
/// workflow run.
fn is_release_build_of(
    attestation: &Attestation,
    version_tag: &str,
    filename: &str,
    digest_hex: &str,
) -> bool {
    let Some(bundle) = &attestation.bundle else {
        return false;
    };
    let Some(statement) = bundle.statement() else {
        return false;
    };
    let about_the_file = statement.subject.iter().any(|subject| {
        subject.name == filename
            && subject
                .digest
                .sha256
                .as_deref()
                .is_some_and(|sha256| sha256.eq_ignore_ascii_case(digest_hex))
    });
    let workflow = |field: &str| {
        statement
            .predicate
            .pointer(&format!(
                "/buildDefinition/externalParameters/workflow/{field}"
            ))
            .and_then(serde_json::Value::as_str)
    };
    let repository = format!("https://github.com/{REPO}");
    let tag_ref = format!("refs/tags/{version_tag}");
    statement.predicate_type == SLSA_PROVENANCE_V1
        && about_the_file
        && workflow("repository") == Some(repository.as_str())
        && workflow("path") == Some(RELEASE_WORKFLOW)
        && workflow("ref") == Some(tag_ref.as_str())
        && bundle
            .signing_certificate()
            .is_some_and(|der| has_san_uri(&der, &release_workflow_identity(version_tag)))
}

/// Requires GitHub's build-provenance attestation for the file with SHA-256
/// `digest_hex`: one PolyRec's release workflow (`release.yml`), run for
/// `version_tag`, recorded for `filename` (`is_release_build_of`).
///
/// The release key's signature on `SHA256SUMS.txt` says the maintainer
/// published this release; this says its file is what PolyRec's own
/// release build produced, not something uploaded by hand alongside --
/// should the release key ever sign the wrong thing. Anyone able to run
/// the release workflow can get such an attestation, which is why it isn't
/// enough on its own.
///
/// The attestation's Sigstore signature isn't checked here: this trusts
/// GitHub's API, over TLS, to hand back the attestations it holds. The
/// maintainer checks it in full before signing (`Sign-Release.ps1`).
///
/// Blocking: an update whose build can't be confirmed -- offline, or
/// GitHub's API rate limit -- isn't installed, and says why. That can be
/// retried; installing one that shouldn't have been can't be undone.
async fn check_build_provenance(
    version_tag: &str,
    filename: &str,
    digest_hex: &str,
) -> Result<(), AppError> {
    let url = format!("https://api.github.com/repos/{REPO}/attestations/sha256:{digest_hex}");
    let found: AttestationsResponse = http_client()?
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .update_ctx("looking up the update's build provenance")?
        .error_for_status()
        .update_ctx("looking up the update's build provenance")?
        .json()
        .await
        .update_ctx("reading the update's build provenance")?;
    if found
        .attestations
        .iter()
        .any(|a| is_release_build_of(a, version_tag, filename, digest_hex))
    {
        tracing::info!(
            "{filename}: built by {}",
            release_workflow_identity(version_tag)
        );
        Ok(())
    } else {
        Err(AppError::Update(format!(
            "{filename} has no build provenance from PolyRec's release workflow for {version_tag}"
        )))
    }
}

/// Downloads `filename` for `version_tag`, verifies it against the
/// release's published `SHA256SUMS.txt` and requires its build provenance
/// (`check_build_provenance`) before returning its bytes. This is the
/// single gate every self-update path goes through -- nothing downstream
/// ever sees unverified bytes.
async fn download_and_verify(version_tag: &str, filename: &str) -> Result<Vec<u8>, AppError> {
    let expected = fetch_expected_sha256(version_tag, filename).await?;
    let bytes = download_bytes(&release_asset_url(version_tag, filename)).await?;
    verify_sha256(&bytes, &expected)?;
    check_build_provenance(version_tag, filename, &expected).await?;
    Ok(bytes)
}

/// Extracts the single `polyrec.exe` entry from a downloaded release zip
/// (which also contains README.md/LICENSE, see `release.yml`'s "Package
/// zip" step -- everything else is ignored), refusing one bigger than `max`
/// bytes whatever the zip says its size is.
fn extract_exe_from_zip(zip_bytes: &[u8], max: u64) -> Result<Vec<u8>, AppError> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).update_ctx("opening update zip")?;
    let mut file = archive
        .by_name("polyrec.exe")
        .update_ctx("update zip has no polyrec.exe")?;
    let mut out = Vec::new();
    std::io::copy(&mut std::io::Read::take(&mut file, max + 1), &mut out)
        .update_ctx("reading polyrec.exe from update zip")?;
    if out.len() as u64 > max {
        return Err(too_big("polyrec.exe in the update zip"));
    }
    Ok(out)
}

/// A verified update, written to a folder of its own under %TEMP%, made
/// for it -- so nothing already there can be taken for it -- and removed
/// with it when dropped.
struct Staged {
    dir: PathBuf,
    path: PathBuf,
    sha256: String,
}

impl Staged {
    fn write(name: &str, bytes: &[u8]) -> Result<Self, AppError> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("polyrec-update-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&dir).update_ctx(format_args!("creating {}", dir.display()))?;
        let staged = Self {
            path: dir.join(name),
            dir,
            sha256: hex_encode(Sha256::digest(bytes).as_slice()),
        };
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged.path)
            .and_then(|mut file| file.write_all(bytes))
            .update_ctx(format_args!("writing {}", staged.path.display()))?;
        Ok(staged)
    }

    /// Opens the staged file so that, while it's held open, nothing can
    /// change, replace or remove it -- and checks it's still the bytes that
    /// were verified.
    fn open_verified(&self) -> Result<std::fs::File, AppError> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .open(&self.path)
            .update_ctx(format_args!("opening {}", self.path.display()))?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes)
            .update_ctx(format_args!("reading {}", self.path.display()))?;
        let actual = hex_encode(Sha256::digest(&bytes).as_slice());
        if actual == self.sha256 {
            Ok(file)
        } else {
            Err(AppError::Update(format!(
                "{} changed after it was verified",
                self.path.display()
            )))
        }
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Performs the update for `version_tag` (e.g. `"v0.5.0"`, matching
/// `update_check::AvailableUpdate::version`/the release's git tag). On
/// success, the running exe has already been replaced and a fresh process
/// spawned (portable), or the installer has already been launched
/// (installed) -- the caller's only remaining job is to close its own
/// window.
pub async fn perform_self_update(version_tag: String) -> Result<(), AppError> {
    // It goes into URLs and file names: only ever a version, as
    // `update_check` found it.
    if crate::update_check::tag_version(&version_tag).is_none() {
        return Err(AppError::Update(format!("{version_tag:?} isn't a version")));
    }
    match detect_install_kind()? {
        InstallKind::Portable => update_portable(&version_tag).await,
        InstallKind::Installed => update_installed(&version_tag).await,
    }
}

async fn update_portable(version_tag: &str) -> Result<(), AppError> {
    let zip_filename = format!("polyrec-{version_tag}-windows-x64.zip");
    let zip_bytes = download_and_verify(version_tag, &zip_filename).await?;
    let exe_bytes = extract_exe_from_zip(&zip_bytes, MAX_UPDATE_BYTES)?;

    let staged = Staged::write("polyrec.exe", &exe_bytes)?;
    // Checked once more just before use; not held open through it, since
    // `self_replace` moves files around it.
    drop(staged.open_verified()?);
    self_replace::self_replace(&staged.path).update_ctx("self_replace failed")?;

    let current_exe = std::env::current_exe().update_ctx("current_exe after self_replace")?;
    // This copy is still closing (see `single_instance`).
    std::process::Command::new(&current_exe)
        .arg(crate::single_instance::AFTER_UPDATE_ARG)
        .spawn()
        .update_ctx("spawning updated exe")?;
    Ok(())
}

/// How an update runs the installer. /VERYSILENT+/SUPPRESSMSGBOXES suppress
/// Inno's own wizard UI; they do NOT suppress the OS-level UAC elevation
/// prompt (independent, driven by the installer's manifest) -- that prompt
/// is expected. /NORESTART since a PolyRec update never needs a reboot.
/// /RELAUNCH=1 has installer/polyrec.iss open PolyRec again once it's done.
/// Its `RestartApplications` doesn't: that's for apps the installer itself
/// closed, and PolyRec closes itself first -- so after an installed update
/// PolyRec just didn't come back.
const INSTALLER_ARGS: [&str; 4] = [
    "/VERYSILENT",
    "/SUPPRESSMSGBOXES",
    "/NORESTART",
    "/RELAUNCH=1",
];

async fn update_installed(version_tag: &str) -> Result<(), AppError> {
    let setup_filename = format!("polyrec-{version_tag}-windows-x64-setup.exe");
    let setup_bytes = download_and_verify(version_tag, &setup_filename).await?;

    let staged = Staged::write(&setup_filename, &setup_bytes)?;
    // Held, so it can't be changed or replaced, until the installer is
    // under way -- the UAC prompt it brings up says it's PolyRec's.
    let _held = staged.open_verified()?;

    // We deliberately don't wait for this to finish -- we can't reliably
    // wait through its elevation prompt, and PolyRec has to have closed for
    // its files to be replaced -- so the installer reopens it
    // (`INSTALLER_ARGS`).
    std::process::Command::new(&staged.path)
        .args(INSTALLER_ARGS)
        .spawn()
        .update_ctx("launching installer")?;
    // The running installer keeps its file (and so the folder) from being
    // removed when `staged` drops; Windows' temp cleanup takes it later.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// After an installed update, PolyRec didn't come back: the installer's
    /// `RestartApplications` only reopens what it closed itself, and PolyRec
    /// closes itself first.
    #[test]
    fn the_installer_reopens_polyrec_after_an_update() {
        const ISS: &str = include_str!("../installer/polyrec.iss");
        assert!(INSTALLER_ARGS.contains(&"/RELAUNCH=1"));
        assert!(ISS.contains("ExpandConstant('{param:RELAUNCH|0}') = '1'"));
        assert!(ISS.contains("Check: RelaunchAfterUpdate"));
    }

    /// The GitHub API's real answer for v0.5.139's zip: GitHub's own
    /// release attestation, then the build provenance `release.yml` made.
    const V0_5_139_ZIP_ATTESTATIONS: &str =
        include_str!("../tests/fixtures/attestations_v0.5.139_zip.json");
    const V0_5_139_ZIP: &str = "polyrec-v0.5.139-windows-x64.zip";
    const V0_5_139_ZIP_SHA256: &str =
        "27904c60da11f267d9aaa1fc1cadc92f8458e058dcb65ea540c5261fc42bca83";

    fn v0_5_139_zip_attestations() -> AttestationsResponse {
        serde_json::from_str(V0_5_139_ZIP_ATTESTATIONS).unwrap()
    }

    fn any_release_build_of(
        found: &AttestationsResponse,
        version_tag: &str,
        filename: &str,
        digest_hex: &str,
    ) -> bool {
        found
            .attestations
            .iter()
            .any(|a| is_release_build_of(a, version_tag, filename, digest_hex))
    }

    /// `attestations` with each statement passed through `change`.
    fn with_statements_changed(
        mut found: AttestationsResponse,
        change: impl Fn(&mut serde_json::Value),
    ) -> AttestationsResponse {
        let b64 = base64::engine::general_purpose::STANDARD;
        for bundle in found
            .attestations
            .iter_mut()
            .filter_map(|a| a.bundle.as_mut())
        {
            let mut statement: serde_json::Value =
                serde_json::from_slice(&b64.decode(&bundle.dsse_envelope.payload).unwrap())
                    .unwrap();
            change(&mut statement);
            bundle.dsse_envelope.payload = b64.encode(serde_json::to_vec(&statement).unwrap());
        }
        found
    }

    #[test]
    fn a_real_release_build_is_recognised() {
        assert!(any_release_build_of(
            &v0_5_139_zip_attestations(),
            "v0.5.139",
            V0_5_139_ZIP,
            V0_5_139_ZIP_SHA256
        ));
    }

    #[test]
    fn a_file_the_release_build_didnt_produce_isnt() {
        let found = v0_5_139_zip_attestations();
        let other = "0".repeat(64);
        assert!(!any_release_build_of(
            &found,
            "v0.5.139",
            V0_5_139_ZIP,
            &other
        ));
        // The build's installer, by the zip's name.
        let setup = "08615281a0d9aff91fbe5ae64557571b3e1ca2cd8b69228e01c8fcc84e2694dc";
        assert!(!any_release_build_of(
            &found,
            "v0.5.139",
            V0_5_139_ZIP,
            setup
        ));
        // The zip, by the installer's name.
        let setup_name = "polyrec-v0.5.139-windows-x64-setup.exe";
        assert!(!any_release_build_of(
            &found,
            "v0.5.139",
            setup_name,
            V0_5_139_ZIP_SHA256
        ));
    }

    /// An old release's file, offered as a newer one -- or as an older one
    /// whose tag it starts with.
    #[test]
    fn a_release_build_for_another_tag_isnt() {
        let found = v0_5_139_zip_attestations();
        for tag in ["v0.5.140", "v0.5.13", "v0.5.1"] {
            assert!(
                !any_release_build_of(&found, tag, V0_5_139_ZIP, V0_5_139_ZIP_SHA256),
                "{tag}"
            );
        }
    }

    /// GitHub attests to every release it publishes; that says nothing about
    /// how its files were built.
    #[test]
    fn githubs_own_release_attestation_isnt_a_build() {
        let mut found = v0_5_139_zip_attestations();
        found.attestations.truncate(1);
        assert!(!any_release_build_of(
            &found,
            "v0.5.139",
            V0_5_139_ZIP,
            V0_5_139_ZIP_SHA256
        ));
    }

    #[test]
    fn a_build_by_another_workflow_or_repo_isnt() {
        let changes: [(&str, &str); 3] = [
            ("path", ".github/workflows/other.yml"),
            ("repository", "https://github.com/someone/polyrec"),
            ("ref", "refs/heads/main"),
        ];
        for (field, value) in changes {
            let found = with_statements_changed(v0_5_139_zip_attestations(), |statement| {
                if let Some(workflow) =
                    statement.pointer_mut("/predicate/buildDefinition/externalParameters/workflow")
                {
                    workflow[field] = value.into();
                }
            });
            assert!(
                !any_release_build_of(&found, "v0.5.139", V0_5_139_ZIP, V0_5_139_ZIP_SHA256),
                "{field} = {value}"
            );
        }
    }

    /// The statement says release.yml, but the certificate it was signed
    /// with isn't the release workflow's.
    #[test]
    fn a_statement_not_signed_as_the_release_workflow_isnt() {
        let mut found = v0_5_139_zip_attestations();
        for bundle in found
            .attestations
            .iter_mut()
            .filter_map(|a| a.bundle.as_mut())
        {
            bundle.verification_material.certificate = None;
        }
        assert!(!any_release_build_of(
            &found,
            "v0.5.139",
            V0_5_139_ZIP,
            V0_5_139_ZIP_SHA256
        ));
    }

    #[test]
    fn a_san_uri_matches_whole_not_as_the_start_of_a_longer_one() {
        let uri = "https://github.com/a/b/.github/workflows/release.yml@refs/tags/v1.2.30";
        let mut der = vec![0x30, 0x82, 0x01, 0x00, 0x86, uri.len() as u8];
        der.extend_from_slice(uri.as_bytes());
        assert!(has_san_uri(&der, uri));
        assert!(!has_san_uri(&der, &uri[..uri.len() - 1]));
        assert!(!has_san_uri(&der, "https://github.com/a/b"));
    }

    #[test]
    fn only_githubs_own_hosts_are_followed() {
        for host in [
            "github.com",
            "api.github.com",
            "objects.githubusercontent.com",
            "release-assets.githubusercontent.com",
        ] {
            assert!(is_github_host(host), "{host}");
        }
        for host in [
            "githubusercontent.com.evil.example",
            "evilgithubusercontent.com",
            "github.com.evil.example",
            "example.com",
        ] {
            assert!(!is_github_host(host), "{host}");
        }
    }

    fn zip_with_exe(exe: &[u8]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("polyrec.exe", options).unwrap();
        zip.write_all(exe).unwrap();
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn the_exe_is_extracted_from_the_zip() {
        let zip = zip_with_exe(b"MZ the exe");
        assert_eq!(extract_exe_from_zip(&zip, 1024).unwrap(), b"MZ the exe");
    }

    /// Zeroes deflate to almost nothing: a few KB of zip can hold gigabytes.
    #[test]
    fn an_exe_bigger_than_any_release_is_refused() {
        let zip = zip_with_exe(&vec![0u8; 1024 * 1024]);
        assert!(zip.len() < 64 * 1024);
        assert!(extract_exe_from_zip(&zip, 512 * 1024).is_err());
    }

    #[test]
    fn a_staged_update_changed_after_it_was_written_is_refused() {
        let staged = Staged::write("polyrec.exe", b"MZ the update").unwrap();
        std::fs::write(&staged.path, b"MZ something else").unwrap();
        assert!(staged.open_verified().is_err());
    }

    /// Held open, as through the installer's UAC prompt, a staged update
    /// can't be changed or replaced.
    #[test]
    fn a_staged_update_cant_be_changed_while_held() {
        let staged = Staged::write("polyrec.exe", b"MZ the update").unwrap();
        let held = staged.open_verified().unwrap();
        assert!(std::fs::write(&staged.path, b"MZ something else").is_err());
        assert!(std::fs::remove_file(&staged.path).is_err());
        drop(held);
        let dir = staged.dir.clone();
        drop(staged);
        assert!(!dir.exists());
    }

    /// Signed with throwaway keys made for these tests (`minisign -G -W`,
    /// then `minisign -S -t "polyrec v0.5.139"`; `-l` for the legacy one),
    /// whose secret halves were deleted straight after.
    mod signed_sums {
        use super::*;

        const SUMS: &[u8] = include_bytes!("../tests/fixtures/release_signing/SHA256SUMS.txt");
        const MAIN: &str = include_str!("../tests/fixtures/release_signing/main.pub");
        const BACKUP: &str = include_str!("../tests/fixtures/release_signing/backup.pub");
        const OTHER: &str = include_str!("../tests/fixtures/release_signing/other.pub");
        const BY_MAIN: &str = include_str!("../tests/fixtures/release_signing/by_main.minisig");
        const BY_BACKUP: &str = include_str!("../tests/fixtures/release_signing/by_backup.minisig");
        const BY_OTHER: &str = include_str!("../tests/fixtures/release_signing/by_other.minisig");
        const BY_MAIN_FOR_V0_5_138: &str =
            include_str!("../tests/fixtures/release_signing/by_main_for_v0.5.138.minisig");
        const LEGACY_BY_MAIN: &str =
            include_str!("../tests/fixtures/release_signing/legacy_by_main.minisig");

        fn verify(signature: &str, version_tag: &str) -> Result<(), AppError> {
            verify_signed_sums(SUMS, signature, &[MAIN, BACKUP], version_tag)
        }

        #[test]
        fn checksums_signed_with_the_release_key_are_accepted() {
            verify(BY_MAIN, "v0.5.139").unwrap();
        }

        #[test]
        fn checksums_signed_with_the_backup_key_are_accepted() {
            verify(BY_BACKUP, "v0.5.139").unwrap();
        }

        /// Someone able to publish a release, but without the release key,
        /// signing with a key of their own.
        #[test]
        fn checksums_signed_with_any_other_key_are_refused() {
            assert!(verify(BY_OTHER, "v0.5.139").is_err());
            assert!(verify_signed_sums(SUMS, BY_OTHER, &[OTHER], "v0.5.139").is_ok());
        }

        #[test]
        fn checksums_changed_after_signing_are_refused() {
            let mut changed = SUMS.to_vec();
            changed[0] = b'3';
            assert!(verify_signed_sums(&changed, BY_MAIN, &[MAIN, BACKUP], "v0.5.139").is_err());
        }

        /// An older release's signed checksums, offered as a newer release's
        /// -- or as one whose tag starts the same.
        #[test]
        fn checksums_signed_for_another_release_are_refused() {
            assert!(verify(BY_MAIN_FOR_V0_5_138, "v0.5.139").is_err());
            assert!(verify(BY_MAIN, "v0.5.13").is_err());
            assert!(verify(BY_MAIN, "v0.5.1390").is_err());
        }

        #[test]
        fn a_legacy_signature_is_refused() {
            assert!(verify(LEGACY_BY_MAIN, "v0.5.139").is_err());
        }

        #[test]
        fn something_that_isnt_a_signature_is_refused() {
            for signature in ["", "not a signature", MAIN] {
                assert!(verify(signature, "v0.5.139").is_err(), "{signature:?}");
            }
        }

        /// The keys built in are the two real ones, not these tests' -- and
        /// not one key twice, which would leave no backup.
        #[test]
        fn the_release_keys_are_two_different_real_keys() {
            let lines: Vec<&str> = RELEASE_KEYS
                .iter()
                .map(|key| {
                    minisign_verify::PublicKey::decode(key).expect("a minisign public key");
                    key.lines().nth(1).unwrap()
                })
                .collect();
            assert_ne!(lines[0], lines[1]);
            for test_key in [MAIN, BACKUP, OTHER] {
                assert!(!lines.contains(&test_key.lines().nth(1).unwrap()));
            }
        }

        /// The keys the user guide tells people to check a download with.
        #[test]
        fn the_user_guide_shows_the_release_keys() {
            const GUIDE: &str = include_str!("../docs/USER_GUIDE.md");
            for key in RELEASE_KEYS {
                let key = key.lines().nth(1).unwrap();
                assert!(GUIDE.contains(&format!("-P {key}")), "{key}");
            }
        }
    }

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
        assert_eq!(
            parse_sha256sums(text, "real-file.zip"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn parse_sha256sums_handles_binary_mode_asterisk_prefix() {
        // `sha256sum` prefixes the filename with "*" in binary mode.
        let text = "abc123  *real-file.zip\n";
        assert_eq!(
            parse_sha256sums(text, "real-file.zip"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn parse_sha256sums_is_case_insensitive_on_the_hash() {
        let text = "ABC123  real-file.zip\n";
        assert_eq!(
            parse_sha256sums(text, "real-file.zip"),
            Some("abc123".to_string())
        );
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
        assert!(
            verify_sha256(
                data,
                "0000000000000000000000000000000000000000000000000000000000000"
            )
            .is_err()
        );
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

    /// v0.5.160, published before releases were signed. Needs network, so
    /// it's ignored by default -- run with `--ignored`.
    #[tokio::test]
    #[ignore]
    async fn a_real_release_without_a_signature_is_refused() {
        let refused = fetch_signed_sums("v0.5.160").await.unwrap_err();
        println!("{refused}");
    }

    /// End-to-end against a real, already-published release -- v0.5.161, the
    /// first signed with the release key: downloads the real zip,
    /// SHA256SUMS.txt and its signature over the network, verifies the real
    /// signature, checksum and build provenance, and extracts the real
    /// polyrec.exe entry -- proving every custom bit of logic in this file
    /// (URL construction, the signature against `Sign-Release.ps1`'s actual
    /// output, SHA256SUMS.txt parsing against CI's actual output format, the
    /// attestation lookup, zip extraction against CI's actual archive)
    /// against genuine GitHub data. Deliberately does NOT call
    /// self_replace/spawn a process -- that mechanism was validated
    /// separately, live, in complete isolation (a throwaway scratch binary
    /// pair, not this repo's own exe), since doing it here would mean
    /// replacing the test binary's own exe file. Needs network access, so
    /// it's ignored by default -- run with `--ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn download_and_verify_and_extract_against_a_real_published_release() {
        let zip_bytes = download_and_verify("v0.5.161", "polyrec-v0.5.161-windows-x64.zip")
            .await
            .expect("download_and_verify failed against the real v0.5.161 release");
        println!("downloaded {} bytes", zip_bytes.len());

        let exe_bytes = extract_exe_from_zip(&zip_bytes, MAX_UPDATE_BYTES)
            .expect("extract_exe_from_zip failed");
        println!("extracted polyrec.exe: {} bytes", exe_bytes.len());
        assert!(
            exe_bytes.len() > 1_000_000,
            "extracted exe suspiciously small"
        );
        // "MZ" -- the DOS header magic bytes every valid Windows PE starts with.
        assert_eq!(
            &exe_bytes[0..2],
            b"MZ",
            "extracted file isn't a valid PE executable"
        );
    }
}
