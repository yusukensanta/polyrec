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
- Path traversal or unintended file writes via the output directory or export path.
- Credential/token leakage in logs or the release pipeline.
- Supply-chain issues in the release pipeline itself (unpinned dependencies, mutable Action references, unverifiable build artifacts).

**Out of scope:**
- Vulnerabilities in the Rust toolchain, Windows itself, or third-party crates (report those to the respective upstream projects — `cargo audit` findings for crates PolyRec doesn't actually exercise on Windows are tracked, not necessarily fixed; see recent security-audit commits for the reasoning).
- Issues that require the attacker to already have local code-execution or admin access on the machine running PolyRec.
- SmartScreen/AV warnings on the executable — releases are Authenticode-signed via SignPath Foundation's free OSS code signing, but SmartScreen reputation still builds up gradually after signing starts, not instantly. Verify authenticity via the published `SHA256SUMS.txt` and build provenance attestation (see the README's "Verifying a download" section) in the meantime.

## Security Advisories

Published advisories are available at:
<https://github.com/yusukensanta/polyrec/security/advisories>
