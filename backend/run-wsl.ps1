param(
    [string]$Distribution = 'kali-linux',
    [switch]$MigrateOnly,
    [string]$BootstrapOwner
)

$ErrorActionPreference = 'Stop'
$backendArgs = @()
if ($MigrateOnly) { $backendArgs += '--migrate-only' }
if ($BootstrapOwner) {
    if ($MigrateOnly) { throw 'Use either -MigrateOnly or -BootstrapOwner.' }
    $backendArgs += @('--bootstrap-owner', $BootstrapOwner)
}
& wsl.exe --distribution $Distribution --cd $PSScriptRoot --exec bash ./run.sh @backendArgs
exit $LASTEXITCODE
