#Requires -Version 7.4
<#
.SYNOPSIS
Signs a draft release's SHA256SUMS.txt with the release key and publishes it.

.DESCRIPTION
Run by the maintainer, locally, in a terminal of their own (minisign asks for
the key's password) -- never in CI: the point of the release key is that
nothing on GitHub can sign with it.

Run a copy kept OUTSIDE the repo, not this file. Whoever can push to GitHub
can change this one, and it runs while you type the key's password. Install
it next to your keys once, after reading it -- and again, after reading the
diff, whenever it changes (it says when the repo's copy differs from it):

  Copy-Item .github/scripts/Sign-Release.ps1 $env:USERPROFILE\.minisign\

Then, from the repo checkout the tag was pushed from:

  pwsh -NoProfile -File $env:USERPROFILE\.minisign\Sign-Release.ps1 v0.5.161

It trusts what's on this machine -- the local tag, and your public keys next
to it (polyrec-release.pub, polyrec-release-backup.pub) -- not GitHub, and:

  1. checks the release is still a draft, its tag a commit on main, and that
     keys/ at that commit (the keys PolyRec gets built with) are your keys;
  2. shows what changed since the last release signed here, flagging files
     that change the build, the release or the keys, and waits for you to
     type the tag: signing says you've seen it;
  3. downloads its files: SHA256SUMS.txt must list exactly them, and each
     must have build provenance from release.yml for that tag and commit
     (gh attestation verify, which checks the Sigstore signature in full);
  4. signs SHA256SUMS.txt (trusted comment "polyrec <tag>", which PolyRec
     requires) and checks the signature against your public keys;
  5. uploads the signature, checks the draft holds exactly these four files
     with the hashes checked, publishes it, and checks it again.

Needs minisign (winget install jedisct1.minisign), gh signed in, and git.

.EXAMPLE
pwsh -NoProfile -File $env:USERPROFILE\.minisign\Sign-Release.ps1 v0.5.161

.EXAMPLE
# With the backup key, should the release key be lost.
pwsh -NoProfile -File $env:USERPROFILE\.minisign\Sign-Release.ps1 v0.5.161 -KeyFile E:\polyrec-release-backup.key
#>
param(
    [Parameter(Mandatory)]
    [string]$Tag,
    [string]$KeyFile = (Join-Path $env:USERPROFILE '.minisign\polyrec-release.key'),
    # Your public keys, and the record of the last release signed here.
    [string]$KeyDir = (Join-Path $env:USERPROFILE '.minisign'),
    # Check and sign, but leave the release as it is.
    [switch]$NoPublish
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$Repo = 'yusukensanta/polyrec'
$KeyNames = @('polyrec-release.pub', 'polyrec-release-backup.pub')
$Record = Join-Path $KeyDir 'polyrec-last-signed.txt'
# Changes to these change what gets built, released or trusted.
$Sensitive = '^(keys/|\.github/|installer/|Cargo\.(toml|lock)$|rust-toolchain|(.*/)?build\.rs$|\.cargo/)'

function Step([string]$What) { Write-Host "-- $What" -ForegroundColor Cyan }

# A file's lines, without line endings or trailing blank lines.
function Lines([string[]]$Text) { (($Text -join "`n") -replace "`r", '').TrimEnd("`n") }

if ($Tag -cnotmatch '^v[0-9]+\.[0-9]+\.[0-9]+\z') {
    throw "Not a release tag: '$Tag' -- expected vX.Y.Z"
}
if (-not (Test-Path -LiteralPath $KeyFile)) { throw "No key at $KeyFile" }
foreach ($name in $KeyNames) {
    if (-not (Test-Path -LiteralPath (Join-Path $KeyDir $name))) { throw "No $name in $KeyDir" }
}
if (-not ([Environment]::GetCommandLineArgs() -match '^-(NoProfile|nop)$')) {
    Write-Warning 'Started without -NoProfile: your PowerShell profile ran first.'
}
foreach ($tool in 'git', 'gh', 'minisign') {
    Write-Host "   $tool = $((Get-Command $tool -CommandType Application)[0].Source)"
}

$Root = (Resolve-Path (git rev-parse --show-toplevel)).Path
if ((git -C $Root config --get remote.origin.url) -notmatch "[:/]$Repo(\.git)?$") {
    throw "$Root isn't a checkout of $Repo -- run this from one"
}
if ($PSCommandPath.StartsWith($Root + [IO.Path]::DirectorySeparatorChar, 'OrdinalIgnoreCase')) {
    throw "This is the repo's copy of Sign-Release.ps1, which anyone able to push to GitHub can change. Run the copy in $KeyDir (see the top of this file)."
}

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

Step "PolyRec at $Tag trusts your keys, and only them"
foreach ($name in $KeyNames) {
    try { $built = Lines (git -C $Root show "${commit}:keys/$name" 2>$null) }
    catch { throw "There's no keys/$name at $Tag" }
    $yours = Lines (Get-Content -LiteralPath (Join-Path $KeyDir $name))
    if ($built -cne $yours) { throw "keys/$name at $Tag isn't your $name -- someone changed the keys PolyRec trusts" }
    Write-Host "   keys/$name"
}
try { $repoCopy = Lines (git -C $Root show "${commit}:.github/scripts/Sign-Release.ps1" 2>$null) }
catch { $repoCopy = $null }
if ($repoCopy -cne (Lines (Get-Content -LiteralPath $PSCommandPath))) {
    Write-Warning "The repo's Sign-Release.ps1 at $Tag differs from this copy. Read the change (git diff) and re-install it if it's yours."
}

Step 'What changed since the last release signed here'
if (Test-Path -LiteralPath $Record) {
    $lastTag, $since = (Get-Content -LiteralPath $Record -TotalCount 1) -split ' '
    try { git -C $Root merge-base --is-ancestor $since $commit }
    catch { throw "$Tag doesn't follow $lastTag ($since), the last release signed here -- main was rewritten?" }
} else {
    $lastTag = git -C $Root describe --tags --abbrev=0 --match 'v[0-9]*' "$commit^"
    $since = git -C $Root rev-parse "$lastTag^{commit}"
    Write-Host "   (nothing signed here yet: since $lastTag)"
}
git -C $Root --no-pager log --format='   %h %ad %an  %s' --date=short "$since..$commit"
$changed = @(git -C $Root diff --name-only "$since..$commit")
$flagged = @($changed | Where-Object { $_ -match $Sensitive })
Write-Host "   $($changed.Count) files changed since $lastTag"
foreach ($file in $flagged) { Write-Host "   ! $file" -ForegroundColor Yellow }
if ($flagged) {
    Write-Host "   The ! files change what gets built, released or trusted: check them (git diff $lastTag $Tag -- <file>)." -ForegroundColor Yellow
}
$answer = Read-Host "Type $Tag to sign it, anything else to stop"
if ($answer -cne $Tag) { throw 'Not signed.' }

$Dir = Join-Path ([IO.Path]::GetTempPath()) "polyrec-sign-$Tag-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $Dir | Out-Null
try {
    $files = @("polyrec-$Tag-windows-x64.zip", "polyrec-$Tag-windows-x64-setup.exe")
    $sums = Join-Path $Dir 'SHA256SUMS.txt'
    $signature = "$sums.minisig"

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
    $checked = @{}
    foreach ($file in $files) {
        $hash = (Get-FileHash -LiteralPath (Join-Path $Dir $file) -Algorithm SHA256).Hash.ToLower()
        if ($listed[$file] -cne $hash) { throw "$file is $hash, SHA256SUMS.txt says '$($listed[$file])'" }
        $checked[$file] = $hash
        Write-Host "   $hash  $file"
    }

    Step 'Each was built by release.yml for this tag and commit'
    foreach ($file in $files) {
        gh attestation verify (Join-Path $Dir $file) -R $Repo `
            --signer-workflow "$Repo/.github/workflows/release.yml" `
            --source-ref "refs/tags/$Tag" `
            --source-digest $commit `
            --deny-self-hosted-runners | Out-Null
        Write-Host "   $file"
    }

    Step 'Signing SHA256SUMS.txt (minisign asks for the key''s password)'
    minisign -S -s $KeyFile -m $sums -t "polyrec $Tag" -x $signature

    Step 'The signature is one of your keys'', for this tag'
    $signedFor = $null
    foreach ($name in $KeyNames) {
        try {
            $signedFor = minisign -V -Q -p (Join-Path $KeyDir $name) -m $sums -x $signature 2>$null
            Write-Host "   $name"
            break
        } catch [System.Management.Automation.NativeCommandExitException] {
            # Not this key: try the next.
        }
    }
    if ($null -eq $signedFor) { throw "$KeyFile isn't the secret half of $($KeyNames -join ' or ')" }
    if ($signedFor -cne "polyrec $Tag") { throw "Signed for '$signedFor', not 'polyrec $Tag'" }

    if ($NoPublish) {
        Step 'Checked and signed; -NoPublish, so nothing uploaded'
        return
    }

    $checked['SHA256SUMS.txt'] = (Get-FileHash -LiteralPath $sums -Algorithm SHA256).Hash.ToLower()
    $checked['SHA256SUMS.txt.minisig'] = (Get-FileHash -LiteralPath $signature -Algorithm SHA256).Hash.ToLower()
    # Drafts can still change: what's published must be exactly what was
    # checked here, nothing swapped in or added since.
    function Assert-Checked {
        $assets = @((gh release view $Tag -R $Repo --json assets | ConvertFrom-Json).assets)
        $have = ($assets.name | Sort-Object -CaseSensitive) -join ', '
        $want = ($checked.Keys | Sort-Object -CaseSensitive) -join ', '
        if ($have -cne $want) { throw "$Tag holds $have -- expected $want" }
        foreach ($asset in $assets) {
            if ($asset.digest -cne "sha256:$($checked[$asset.name])") {
                throw "$($asset.name) on GitHub is '$($asset.digest)', not the sha256:$($checked[$asset.name]) checked here"
            }
        }
    }

    Step 'Uploading the signature'
    gh release upload $Tag $signature -R $Repo
    Step 'The draft holds exactly what was checked'
    Assert-Checked
    Step 'Publishing'
    gh release edit $Tag -R $Repo --draft=false --latest | Out-Null
    $published = gh release view $Tag -R $Repo --json isDraft,url | ConvertFrom-Json
    if ($published.isDraft) { throw "$Tag is still a draft" }
    Assert-Checked
    Set-Content -LiteralPath $Record -Value "$Tag $commit"
    Step "Published: $($published.url)"
} finally {
    Remove-Item -LiteralPath $Dir -Recurse -Force -ErrorAction SilentlyContinue
}
