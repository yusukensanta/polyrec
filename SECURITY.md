# Security Policy

## Supported Versions

Only the latest release on the `main` branch receives security fixes.
Older versions are not patched; please upgrade to the latest release.

| Version | Supported |
| ------- | --------- |
| latest  | ✅        |
| < latest | ❌       |

## Reporting a Vulnerability

**Do not open a public GitHub issue for security vulnerabilities.**

Please report vulnerabilities by emailing:

**yusukensanta@gmail.com**

Include in your report:
- Description of the vulnerability
- Steps to reproduce (proof of concept if available)
- Affected version(s)
- Suggested severity (CVSS score or qualitative: low/medium/high/critical)
- Any suggested remediation

You will receive an acknowledgement within **5 business days**.
We aim to release a fix within **30 days** of confirmation, depending on severity.

## Disclosure Policy

We follow **coordinated disclosure**:

1. Reporter submits vulnerability privately.
2. We confirm and work on a fix.
3. We release the fix and publish a [GitHub Security Advisory](https://github.com/yusukensanta/polyrec/security/advisories).
4. Reporter is credited in the advisory (unless they prefer to remain anonymous).

## Scope

This policy covers PolyRec itself: the Rust application and its release/CI pipeline.

**In scope:**
- Privilege escalation or arbitrary code execution via a malicious recording target, config file, or hotkey binding.
- The update-check mechanism (`src/update_check.rs`) executing or opening anything other than the intended GitHub release page.
- The self-updater (`src/self_update.rs`) installing anything whose `SHA256SUMS.txt` isn't signed with the release key (see "How updates are verified" below), or that doesn't match it.
- Path traversal or unintended file writes via the output directory or export path.
- Credential/token leakage in logs or the release pipeline.
- Supply-chain issues in the release pipeline itself (unpinned dependencies, mutable Action references, unverifiable build artifacts).

**Out of scope:**
- Vulnerabilities in the Rust toolchain, Windows itself, or third-party crates (report those to the respective upstream projects — `cargo audit` findings for crates PolyRec doesn't actually exercise on Windows are tracked, not necessarily fixed; see recent security-audit commits for the reasoning).
- Issues that require the attacker to already have local code-execution or admin access on the machine running PolyRec.
- SmartScreen/AV warnings on the executable — releases are currently **not** Authenticode-signed by a certificate Windows trusts. The release workflow still asks SignPath to sign each build, but those requests fail (the free tier's yearly size quota is used up), and the few early releases that did carry a signature (v0.5.39 and before) used a self-signed certificate Windows doesn't trust. Windows therefore shows "Unknown Publisher". Verify a download with the published `SHA256SUMS.txt`, its signature and its build provenance attestation instead (see "Verifying a download" in the [User Guide](docs/USER_GUIDE.md#verifying-a-download-optional)).

## How updates are verified

From v0.5.161, PolyRec's self-updater installs a release only if all of these hold:

1. Its `SHA256SUMS.txt` is signed (minisign, Ed25519) with one of the two public keys built into PolyRec, the same ones as in [`keys/`](keys): the release key, or a backup key kept offline should it be lost. The signature's trusted comment must name that exact release (`polyrec vX.Y.Z`), so one release's signed checksums can't be passed off as another's.
2. The downloaded file's SHA-256 matches its line in that file.
3. GitHub holds a build provenance attestation saying `release.yml` built that file for that tag.

Neither secret key is ever on GitHub. The release workflow leaves each release as a draft, and the maintainer checks it, signs it and publishes it from their own machine (`.github/scripts/Sign-Release.ps1`, run from a copy kept outside the repo).

That check covers four things:
- the keys the release is built with are the maintainer's;
- every change since the last signed release;
- the files' build provenance;
- the exact files published.

The update banner, too, only appears for a release signed this way. So someone who gets the maintainer's GitHub login or a token elsewhere, or compromises the workflow, can publish a release but can't make PolyRec offer or install it.

What this does **not** cover:
- **A compromised maintainer machine.** The release key is password-protected, but it lives on the maintainer's development machine. Someone in control of that machine could capture the password as it's typed and sign anything.
- **Revoking a key.** Every build trusts both keys for good. Losing one is covered by the other, but if one is stolen, already-installed copies keep trusting it. Replacing it means a new release, and older copies would still accept the stolen key.
- **v0.5.160 and older.** These check build provenance only, not this signature. Until they've updated, someone able to run the release workflow could still ship them an update.
- **Freezing updates.** Someone in control of the GitHub repository can still stop updates from appearing, for example by publishing an unsigned release as the latest. Users then just don't see an update.

## Security Advisories

Published advisories are available at:
<https://github.com/yusukensanta/polyrec/security/advisories>
