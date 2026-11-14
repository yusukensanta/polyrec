use super::*;

/// After an installed update, PolyRec didn't come back: the installer's
/// `RestartApplications` only reopens what it closed itself, and PolyRec
/// closes itself first.
#[test]
fn the_installer_reopens_polyrec_after_an_update() {
    const ISS: &str = include_str!("../../installer/polyrec.iss");
    assert!(INSTALLER_ARGS.contains(&"/RELAUNCH=1"));
    assert!(ISS.contains("ExpandConstant('{param:RELAUNCH|0}') = '1'"));
    assert!(ISS.contains("Check: RelaunchAfterUpdate"));
}

/// The GitHub API's real answer for v0.5.139's zip: GitHub's own
/// release attestation, then the build provenance `release.yml` made.
const V0_5_139_ZIP_ATTESTATIONS: &str =
    include_str!("../../tests/fixtures/attestations_v0.5.139_zip.json");
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
            serde_json::from_slice(&b64.decode(&bundle.dsse_envelope.payload).unwrap()).unwrap();
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

    const SUMS: &[u8] = include_bytes!("../../tests/fixtures/release_signing/SHA256SUMS.txt");
    const MAIN: &str = include_str!("../../tests/fixtures/release_signing/main.pub");
    const BACKUP: &str = include_str!("../../tests/fixtures/release_signing/backup.pub");
    const OTHER: &str = include_str!("../../tests/fixtures/release_signing/other.pub");
    const BY_MAIN: &str = include_str!("../../tests/fixtures/release_signing/by_main.minisig");
    const BY_BACKUP: &str = include_str!("../../tests/fixtures/release_signing/by_backup.minisig");
    const BY_OTHER: &str = include_str!("../../tests/fixtures/release_signing/by_other.minisig");
    const BY_MAIN_FOR_V0_5_138: &str =
        include_str!("../../tests/fixtures/release_signing/by_main_for_v0.5.138.minisig");
    const LEGACY_BY_MAIN: &str =
        include_str!("../../tests/fixtures/release_signing/legacy_by_main.minisig");

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
        const GUIDE: &str = include_str!("../../docs/USER_GUIDE.md");
        for key in RELEASE_KEYS {
            let key = key.lines().nth(1).unwrap();
            assert!(GUIDE.contains(&format!("-P {key}")), "{key}");
        }
    }
}

#[test]
fn parse_sha256sums_finds_matching_line() {
    let text =
        "abc123  polyrec-v0.4.0-windows-x64.zip\ndef456  polyrec-v0.4.0-windows-x64-setup.exe\n";
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

    let exe_bytes =
        extract_exe_from_zip(&zip_bytes, MAX_UPDATE_BYTES).expect("extract_exe_from_zip failed");
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
