# Requires PowerShell, Rust and (Publish only) GitHub CLI.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('Validate', 'Collect', 'Assemble', 'Publish')][string]$Mode,
    [Parameter(Mandatory)][string]$Tag,
    [string]$Target,
    [string]$Platform,
    [string]$InputDirectory = 'release-artifacts',
    [string]$OutputDirectory = 'release-output'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repository = 'BugraEryilmaz/thiscord'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

function Invoke-Checked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}

if ($Tag -cnotmatch '^client-v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
    throw 'Use a stable client-vMAJOR.MINOR.PATCH tag.'
}
$version = $Tag.Substring(8)
$config = Get-Content frontend/tauri.conf.json -Raw | ConvertFrom-Json
# Resolve every optional/platform dependency too: --no-deps misses corrupted
# registry versions/checksums in Cargo.lock and can approve an unbuildable tag.
$metadataText = & cargo metadata --all-features --format-version 1 --locked
if ($LASTEXITCODE) { throw 'Cargo metadata failed' }
$metadata = $metadataText | ConvertFrom-Json
$package = $metadata.packages | Where-Object name -EQ 'thiscord-frontend'
if ($config.version -cne $version -or $package.version -cne $version) {
    throw 'Tag, workspace package version and frontend/tauri.conf.json version must match.'
}
if ($config.plugins.updater.endpoints[0] -cne "https://github.com/$repository/releases/latest/download/latest.json") {
    throw 'Update endpoint and publishing repository disagree.'
}
if ($env:GITHUB_REPOSITORY -and $env:GITHUB_REPOSITORY -cne $repository) {
    throw 'Configure the updater endpoint and release repository before publishing a fork.'
}
if ($Mode -eq 'Validate') { Write-Output "Validated $Tag"; return }

$targets = @{
    'windows-x86_64' = @{ Triple = 'x86_64-pc-windows-msvc'; Update = '.exe'; UpdateDir = 'nsis'; Extra = $null; ExtraDir = $null }
    'linux-x86_64' = @{ Triple = 'x86_64-unknown-linux-gnu'; Update = '.AppImage'; UpdateDir = 'appimage'; Extra = '.deb'; ExtraDir = 'deb' }
    'darwin-aarch64' = @{ Triple = 'aarch64-apple-darwin'; Update = '.app.tar.gz'; UpdateDir = 'macos'; Extra = '.dmg'; ExtraDir = 'dmg' }
}
# This independently verifies the client's actual public key and signed version.
# Tauri's bundler only warns when its signing key and configured public key differ.
Invoke-Checked cargo @('build', '-p', 'thiscord-frontend', '--example', 'verify_update', '--features', 'release-tools', '--locked')
$suffix = if ([Environment]::OSVersion.Platform -eq 'Win32NT') { '.exe' } else { '' }
$verifier = Join-Path $metadata.target_directory "debug/examples/verify_update$suffix"
function Verify-Artifact([string]$File) {
    if ((Get-Item -LiteralPath $File).Length -gt 512MB) { throw 'Updater artifact exceeds the client download limit.' }
    Invoke-Checked $verifier @($File, "$File.sig", $version)
}

if ($Mode -eq 'Collect') {
    if (!$targets.ContainsKey($Platform) -or $targets[$Platform].Triple -cne $Target) {
        throw 'Unsupported platform/target pair.'
    }
    $spec = $targets[$Platform]
    $bundle = Join-Path $metadata.target_directory "$Target/release/bundle"
    $output = New-Item -ItemType Directory -Force -Path $InputDirectory
    foreach ($extension in @($spec.Update, $spec.Extra) | Where-Object { $_ }) {
        $directory = if ($extension -ceq $spec.Update) { $spec.UpdateDir } else { $spec.ExtraDir }
        $files = Get-ChildItem -LiteralPath (Join-Path $bundle $directory) -File
        $matches = @($files | Where-Object { $_.Name.EndsWith($extension, [StringComparison]::Ordinal) })
        if ($matches.Count -ne 1) { throw "Expected exactly one $extension bundle for $Platform" }
        $destination = Join-Path $output.FullName "Thiscord-$version-$Platform$extension"
        Copy-Item -LiteralPath $matches[0].FullName -Destination $destination
        if ($extension -ceq $spec.Update) {
            Copy-Item -LiteralPath "$($matches[0].FullName).sig" -Destination "$destination.sig"
            Verify-Artifact $destination
        }
    }
    @{ platform = $Platform; version = $version } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $output.FullName 'platform.json')
    return
}

# Assemble only from a complete set. Each updater is authenticated again after
# downloading CI artifacts, before creating a remotely visible release.
if (Test-Path -LiteralPath $OutputDirectory) {
    if (@(Get-ChildItem -LiteralPath $OutputDirectory -Force).Count -ne 0) { throw 'Output directory must be empty.' }
}
$output = New-Item -ItemType Directory -Force -Path $OutputDirectory
$records = @(Get-ChildItem -LiteralPath $InputDirectory -Filter platform.json -Recurse -File)
if ($records.Count -ne $targets.Count) { throw 'All configured platform artifacts are required.' }
$platforms = [ordered]@{}
foreach ($record in $records) {
    $data = Get-Content -LiteralPath $record.FullName -Raw | ConvertFrom-Json
    if (!$targets.ContainsKey($data.platform) -or $platforms.Contains($data.platform) -or $data.version -cne $version) {
        throw 'Unknown/duplicate platform or mismatched artifact version.'
    }
    $spec = $targets[$data.platform]
    $name = "Thiscord-$version-$($data.platform)$($spec.Update)"
    $artifact = Join-Path $record.DirectoryName $name
    Verify-Artifact $artifact
    foreach ($file in @($name, "$name.sig")) {
        Copy-Item -LiteralPath (Join-Path $record.DirectoryName $file) -Destination $output.FullName
    }
    if ($spec.Extra) {
        $extra = "Thiscord-$version-$($data.platform)$($spec.Extra)"
        Copy-Item -LiteralPath (Join-Path $record.DirectoryName $extra) -Destination $output.FullName
    }
    $platforms[$data.platform] = @{
        signature = (Get-Content -LiteralPath "$artifact.sig" -Raw).Trim()
        url = "https://github.com/$repository/releases/download/$Tag/$name"
    }
}
@{
    version = $version
    notes = "Thiscord $version. Install when you are ready to restart."
    pub_date = [DateTime]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ssZ')
    platforms = $platforms
} | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $output.FullName 'latest.json')
$hashes = Get-ChildItem -LiteralPath $output.FullName -File | Sort-Object Name | ForEach-Object {
    "$((Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant())  $($_.Name)"
}
$hashes | Set-Content -LiteralPath (Join-Path $output.FullName 'SHA256SUMS')
if ($Mode -eq 'Assemble') { Write-Output "Assembled complete $Tag release"; return }

# Inspect releases before writing anything. Network/auth failures fail closed.
$releaseText = & gh api "repos/$repository/releases?per_page=100" --paginate --slurp
if ($LASTEXITCODE) { throw 'Could not inspect existing releases.' }
$pages = $releaseText | ConvertFrom-Json
$releases = @($pages | ForEach-Object { $_ } | ForEach-Object { $_ })
foreach ($release in $releases) {
    if (!$release.draft -and !$release.prerelease -and $release.tag_name -cmatch '^client-v([0-9]+\.[0-9]+\.[0-9]+)$') {
        if ([version]$Matches[1] -ge [version]$version) { throw 'A same/newer client release is already published.' }
    }
}
$existing = @($releases | Where-Object tag_name -CEQ $Tag)
if ($existing.Count -gt 0 -and !$existing[0].draft) { throw 'Published releases are immutable.' }
$notes = Join-Path $output.FullName 'release-notes.md'
@"
Thiscord $version

- Windows x64: download the .exe installer.
- macOS: download the .dmg matching Apple Silicon (aarch64) or Intel (x86_64).
- Linux x64: use the AppImage for in-app updates, or install the .deb manually.
- The app checks on launch and every six hours. Choose Install and restart when ready; disconnect from voice first.

Updater archives are signed and verified by Thiscord. OS trust is separate: Windows installers are currently unsigned; macOS requires Developer ID/notarization secrets for a trusted download. See docs/releases.md in the tagged source for setup and platform requirements.
"@ | Set-Content -LiteralPath $notes
if ($existing.Count -eq 0) {
    Invoke-Checked gh @('release', 'create', $Tag, '--repo', $repository, '--verify-tag', '--draft', '--title', "Thiscord $version", '--notes-file', $notes)
}
$assets = @(Get-ChildItem -LiteralPath $output.FullName -File | Where-Object Name -NE 'release-notes.md' | Select-Object -ExpandProperty FullName)
Invoke-Checked gh (@('release', 'upload', $Tag, '--repo', $repository, '--clobber') + $assets)
Invoke-Checked gh @('release', 'edit', $Tag, '--repo', $repository, '--draft=false', '--latest=true')
Write-Output "Published $Tag with all platforms and signed update feed."
