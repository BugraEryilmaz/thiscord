param(
    [string]$Distribution = 'kali-linux',
    [switch]$MigrateOnly
)

$ErrorActionPreference = 'Stop'
$backendArgs = @()
if ($MigrateOnly) { $backendArgs += '--migrate-only' }
& wsl.exe --distribution $Distribution --cd $PSScriptRoot --exec bash ./run.sh @backendArgs
exit $LASTEXITCODE
