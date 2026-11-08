#Requires -Version 7.4
<#
.SYNOPSIS
Signs a draft release's SHA256SUMS.txt with the release key and publishes it.

.DESCRIPTION
Run by the maintainer, locally, in a terminal of their own (minisign asks
for the key's password) -- never in CI: the point of the release key is that
nothing on GitHub can sign with it. release.yml leaves every release as a
draft; this is what makes it public:

  1. checks the release is still a draft, and that its tag is a commit on
     main (the local tag -- the one pushed);
  2. downloads its files and checks SHA256SUMS.txt lists exactly them, with
     the right hashes;
  3. checks each file's build provenance: built by release.yml for that tag,
     from that commit, on a GitHub-hosted runner (gh attestation verify,
     which checks the Sigstore signature in full);
  4. signs SHA256SUMS.txt (trusted comment "polyrec <tag>", which PolyRec
     requires) and checks the signature against the public keys in keys/;
  5. uploads SHA256SUMS.txt.minisig and publishes the release.

Needs minisign (winget install jedisct1.minisign), gh signed in, and git.

.EXAMPLE
pwsh -File .github/scripts/Sign-Release.ps1 v0.5.161

.EXAMPLE
# With the backup key, should the release key be lost.
pwsh -File .github/scripts/Sign-Release.ps1 v0.5.161 -KeyFile E:\polyrec-release-backup.key
#>
param(
    [Parameter(Mandatory)]
    [string]$Tag,
    [string]$KeyFile = (Join-Path $env:USERPROFILE '.minisign\polyrec-release.key'),
    # Check and sign, but leave the release as it is.
    [switch]$NoPublish
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$Repo = 'yusukensanta/polyrec'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$PublicKeys = @('keys/polyrec-release.pub', 'keys/polyrec-release-backup.pub') |
    ForEach-Object { Join-Path $Root $_ }

function Step([string]$What) { Write-Host "-- $What" -ForegroundColor Cyan }

if ($Tag -cnotmatch '^v[0-9]+\.[0-9]+\.[0-9]+\z') {
    throw "Not a release tag: '$Tag' -- expected vX.Y.Z"
}
if (-not (Test-Path -LiteralPath $KeyFile)) { throw "No key at $KeyFile" }

if (-not $NoPublish) {
    Step "$Tag is a draft"
    $isDraft = gh release view $Tag -R $Repo --json isDraft --jq .isDraft
    if ($isDraft -ne 'true') { throw "$Tag isn't a draft release -- already published?" }
}

Step "$Tag is a commit on main"
git -C $Root fetch --quiet origin main
$commit = git -C $Root rev-parse --verify --quiet "refs/tags/$Tag^{commit}"
if (-not $commit) { throw "No local tag $Tag -- sign from the checkout it was pushed from" }
git -C $Root merge-base --is-ancestor $commit origin/main
Write-Host "   $commit"

$Dir = Join-Path ([IO.Path]::GetTempPath()) "polyrec-sign-$Tag-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $Dir | Out-Null
try {
    $files = @("polyrec-$Tag-windows-x64.zip", "polyrec-$Tag-windows-x64-setup.exe")
    $sums = Join-Path $Dir 'SHA256SUMS.txt'

    Step 'Downloading its files'
    gh release download $Tag -R $Repo -D $Dir -p SHA256SUMS.txt -p $files[0] -p $files[1]

    Step 'SHA256SUMS.txt lists exactly them'
    $listed = @{}
    foreach ($line in [IO.File]::ReadAllLines($sums)) {
        if ($line -eq '') { continue }
        if ($line -cnotmatch '^([0-9a-f]{64})  (\S+)\z') { throw "Unexpected line in SHA256SUMS.txt: '$line'" }
        $listed[$Matches[2]] = $Matches[1]
    }
    if ($listed.Count -ne $files.Count) { throw "SHA256SUMS.txt lists $($listed.Keys -join ', ')" }
    foreach ($file in $files) {
        $hash = (Get-FileHash -LiteralPath (Join-Path $Dir $file) -Algorithm SHA256).Hash.ToLower()
        if ($listed[$file] -cne $hash) { throw "$file is $hash, SHA256SUMS.txt says '$($listed[$file])'" }
        Write-Host "   $hash  $file"
    }

    Step 'Each was built by release.yml for this tag'
    foreach ($file in $files) {
        gh attestation verify (Join-Path $Dir $file) -R $Repo `
            --signer-workflow "$Repo/.github/workflows/release.yml" `
            --source-ref "refs/tags/$Tag" `
            --source-digest $commit `
            --deny-self-hosted-runners | Out-Null
        Write-Host "   $file"
    }

    Step 'Signing SHA256SUMS.txt (minisign asks for the key''s password)'
    $signature = "$sums.minisig"
    minisign -S -s $KeyFile -m $sums -t "polyrec $Tag" -x $signature

    Step 'The signature is a release key''s, for this tag'
    $signedFor = $null
    foreach ($key in $PublicKeys) {
        try {
            $signedFor = minisign -V -Q -p $key -m $sums -x $signature 2>$null
            Write-Host "   $(Split-Path $key -Leaf)"
            break
        } catch { }
    }
    if ($null -eq $signedFor) { throw "$KeyFile isn't one of the release keys in keys/" }
    if ($signedFor -cne "polyrec $Tag") { throw "Signed for '$signedFor', not 'polyrec $Tag'" }

    if ($NoPublish) {
        Step 'Checked and signed; -NoPublish, so nothing uploaded'
        return
    }

    Step 'Uploading the signature and publishing'
    gh release upload $Tag $signature -R $Repo
    gh release edit $Tag -R $Repo --draft=false --latest | Out-Null
    $published = gh release view $Tag -R $Repo --json isDraft,url,assets |
        ConvertFrom-Json
    if ($published.isDraft) { throw "$Tag is still a draft" }
    if ('SHA256SUMS.txt.minisig' -cnotin $published.assets.name) { throw "$Tag has no signature" }
    Step "Published: $($published.url)"
} finally {
    Remove-Item -LiteralPath $Dir -Recurse -Force -ErrorAction SilentlyContinue
}
