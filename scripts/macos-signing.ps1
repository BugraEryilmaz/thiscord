#!/usr/bin/env pwsh
# Stable macOS code signing for release builds; see docs/releases.md.
#
# Import: decode MACOS_SIGNING_CERTIFICATE (base64 .p12) into a temporary
#   keychain on the user search list and print the identity's SHA-1.
# Verify: require the bundle's designated requirement to name this identifier
#   and certificate, so permission grants and Keychain items survive updates.
# Remove: delete the temporary keychain and restore the search list.
param(
    [Parameter(Mandatory)][ValidateSet('Import', 'Verify', 'Remove')][string]$Mode,
    [string]$App,
    [string]$Certificate,
    [string]$Identifier = 'tr.com.thiscord.desktop'
)
$ErrorActionPreference = 'Stop'
if (!$IsMacOS) { throw 'macOS signing runs on macOS only.' }

function Invoke-Checked([string]$File, [string[]]$Arguments) {
    $output = & $File @Arguments 2>&1
    if ($LASTEXITCODE) { throw "$File $($Arguments[0]) failed: $output" }
    $output
}
function Get-SearchList {
    @(Invoke-Checked security @('list-keychains', '-d', 'user') | ForEach-Object { $_.Trim().Trim('"') } | Where-Object { $_ })
}
$state = Join-Path ([IO.Path]::GetTempPath()) 'thiscord-signing-keychain.txt'
if ($env:RUNNER_TEMP) { $state = Join-Path $env:RUNNER_TEMP 'thiscord-signing-keychain.txt' }

if ($Mode -eq 'Remove') {
    if (!(Test-Path -LiteralPath $state)) { return }
    $keychain = (Get-Content -LiteralPath $state -Raw).Trim()
    $remaining = @(Get-SearchList | Where-Object { $_ -cne $keychain })
    Invoke-Checked security (@('list-keychains', '-d', 'user', '-s') + $remaining) | Out-Null
    if (Test-Path -LiteralPath $keychain) { Invoke-Checked security @('delete-keychain', $keychain) | Out-Null }
    Remove-Item -LiteralPath $state
    return
}

if ($Mode -eq 'Import') {
    if (!$env:MACOS_SIGNING_CERTIFICATE -or !$env:MACOS_SIGNING_CERTIFICATE_PASSWORD) {
        throw 'Set MACOS_SIGNING_CERTIFICATE and MACOS_SIGNING_CERTIFICATE_PASSWORD.'
    }
    if (Test-Path -LiteralPath $state) { & $PSCommandPath -Mode Remove }
    $random = -join ((48..57) + (97..122) | Get-Random -Count 24 | ForEach-Object { [char]$_ })
    $directory = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
    $keychain = Join-Path $directory "thiscord-signing-$random.keychain-db"
    $p12 = Join-Path $directory "thiscord-signing-$random.p12"
    $password = -join ((48..57) + (65..90) + (97..122) | Get-Random -Count 32 | ForEach-Object { [char]$_ })
    try {
        [IO.File]::WriteAllBytes($p12, [Convert]::FromBase64String($env:MACOS_SIGNING_CERTIFICATE))
        Invoke-Checked security @('create-keychain', '-p', $password, $keychain) | Out-Null
        Set-Content -LiteralPath $state -Value $keychain
        Invoke-Checked security @('set-keychain-settings', '-lut', '3600', $keychain) | Out-Null
        Invoke-Checked security @('unlock-keychain', '-p', $password, $keychain) | Out-Null
        Invoke-Checked security @('import', $p12, '-k', $keychain, '-f', 'pkcs12', '-P', $env:MACOS_SIGNING_CERTIFICATE_PASSWORD, '-T', '/usr/bin/codesign') | Out-Null
        Invoke-Checked security @('set-key-partition-list', '-S', 'apple-tool:,apple:,codesign:', '-s', '-k', $password, $keychain) | Out-Null
        # codesign looks identities up through the user search list.
        Invoke-Checked security (@('list-keychains', '-d', 'user', '-s') + (Get-SearchList) + $keychain) | Out-Null
    } finally {
        Remove-Item -LiteralPath $p12 -Force -ErrorAction SilentlyContinue
    }
    # Self-signed certificates are listed as untrusted; codesign still uses them.
    $identities = @(Invoke-Checked security @('find-identity', '-p', 'codesigning', $keychain) |
        Select-String -Pattern '^\s*\d+\)\s+([0-9A-F]{40})\s' | ForEach-Object { $_.Matches[0].Groups[1].Value } | Sort-Object -Unique)
    if ($identities.Count -ne 1) { throw "Expected one signing identity in the certificate, found $($identities.Count)." }
    $identities[0]
    return
}

# Verify
if (!$App -or !(Test-Path -LiteralPath $App)) { throw 'Pass -App with the signed .app bundle.' }
if ($Certificate -notmatch '^[0-9A-Fa-f]{40}$') { throw 'Pass -Certificate with the expected SHA-1.' }
Invoke-Checked codesign @('--verify', '--deep', '--strict', $App) | Out-Null
$details = (Invoke-Checked codesign @('-d', '--verbose=2', $App)) -join "`n"
if ($details -match 'Signature=adhoc') { throw 'The app is ad-hoc signed.' }
$requirement = (Invoke-Checked codesign @('-d', '-r-', $App)) -join "`n"
# A self-signed certificate is both root and leaf; codesign may name either.
$pattern = '(?m)^designated => identifier "' + [regex]::Escape($Identifier) +
    '" and certificate (root|leaf) = H"' + $Certificate.ToLowerInvariant() + '"$'
if ($requirement -notmatch $pattern) {
    throw "Unexpected designated requirement; macOS permissions would not carry over. Got: $requirement"
}
"Signed: $($Matches[0])"
