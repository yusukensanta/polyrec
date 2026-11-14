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
mod tests;
