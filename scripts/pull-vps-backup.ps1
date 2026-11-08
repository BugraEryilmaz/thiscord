<#
.SYNOPSIS
Copies the newest VPS database dump to this machine (off-server backup copy).

.DESCRIPTION
Finds the newest /var/backups/thiscord/thiscord-*.dump over SSH, streams it with
`sudo -n cat` (the server directory stays root-only), verifies its SHA-256 and the
PostgreSQL custom-format header, and keeps the newest -Keep copies locally. The
destination directory is restricted to the current user and SYSTEM. Attachments,
avatars and soundboard clips are stored in PostgreSQL, so the dump is complete.

Run once with -Register to create a daily Windows scheduled task. See docs/vps.md.
#>
[CmdletBinding()]
param(
    [string]$HostAlias = 'vps',
    [string]$Destination = (Join-Path $HOME '.thiscord\backups'),
    [ValidateRange(1, 365)][int]$Keep = 30,
    [switch]$Register,
    [string]$At = '06:00'
)
$ErrorActionPreference = 'Stop'
$RemoteDirectory = '/var/backups/thiscord'
$TaskName = 'Thiscord VPS backup copy'

if ($Register) {
    $pwsh = (Get-Process -Id $PID).Path
    # Run a stable copy so the task survives worktree/checkout removal.
    $script = Join-Path $HOME '.thiscord\tools\pull-vps-backup.ps1'
    New-Item -ItemType Directory -Force -Path (Split-Path $script) | Out-Null
    if ($PSCommandPath -ne $script) { Copy-Item -LiteralPath $PSCommandPath -Destination $script -Force }
    $arguments = "-NoProfile -NonInteractive -WindowStyle Hidden -File `"$script`" " +
        "-HostAlias `"$HostAlias`" -Destination `"$Destination`" -Keep $Keep"
    $action = New-ScheduledTaskAction -Execute $pwsh -Argument $arguments
    # Server dumps finish by about 03:30 UTC; 06:00 local stays after that in CET and CEST.
    $trigger = New-ScheduledTaskTrigger -Daily -At $At
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries -RunOnlyIfNetworkAvailable `
        -ExecutionTimeLimit (New-TimeSpan -Minutes 30) `
        -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 30) -MultipleInstances IgnoreNew
    # Runs as the signed-in user so it can use that user's SSH key; no stored password.
    $principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) `
        -LogonType Interactive -RunLevel Limited
    Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger -Settings $settings `
        -Principal $principal -Description 'Copies the newest Thiscord VPS database dump to this computer.' -Force |
        Out-Null
    Write-Output "Registered '$TaskName' daily at $At (catches up after missed runs)."
    return
}

function Invoke-Remote([string]$Command) {
    $output = & ssh -o BatchMode=yes -o ConnectTimeout=20 $HostAlias $Command
    if ($LASTEXITCODE) { throw "ssh $HostAlias failed with exit code $LASTEXITCODE." }
    return $output
}

function Write-Log([string]$Message) {
    $line = '{0:u} {1}' -f (Get-Date).ToUniversalTime(), $Message
    Add-Content -LiteralPath (Join-Path $Destination 'pull.log') -Value $line
    Write-Output $line
}

if (!(Test-Path -LiteralPath $Destination)) {
    New-Item -ItemType Directory -Path $Destination | Out-Null
}
# Database dumps contain private messages and credential hashes: owner and SYSTEM only.
$user = [Security.Principal.WindowsIdentity]::GetCurrent().Name
& icacls $Destination /inheritance:r /grant:r "${user}:(OI)(CI)F" '*S-1-5-18:(OI)(CI)F' | Out-Null
if ($LASTEXITCODE) { throw 'Could not restrict the backup directory permissions.' }

try {
    $latest = Invoke-Remote "sudo -n sh -c 'ls -1t $RemoteDirectory/thiscord-*.dump | head -n 1'" |
        Select-Object -First 1
    $name = Split-Path -Leaf "$latest"
    if ($name -notmatch '^thiscord-\d{8}T\d{6}Z\.dump$') { throw "Unexpected remote backup name '$name'." }
    $remotePath = "$RemoteDirectory/$name"
    $expected = ((Invoke-Remote "sudo -n sha256sum '$remotePath'") -split '\s+')[0].ToUpperInvariant()
    if ($expected -notmatch '^[0-9A-F]{64}$') { throw 'Could not read the remote checksum.' }

    $target = Join-Path $Destination $name
    if ((Test-Path -LiteralPath $target) -and (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash -eq $expected) {
        Write-Log "Up to date: $name"
    } else {
        $partial = "$target.partial"
        # Copy raw bytes from ssh's stdout; PowerShell pipelines would re-encode them.
        $start = [Diagnostics.ProcessStartInfo]::new('ssh')
        foreach ($argument in @('-o', 'BatchMode=yes', '-o', 'ConnectTimeout=20', $HostAlias, "sudo -n cat '$remotePath'")) {
            $start.ArgumentList.Add($argument)
        }
        $start.RedirectStandardOutput = $true
        $start.UseShellExecute = $false
        $process = [Diagnostics.Process]::Start($start)
        $file = [IO.File]::Create($partial)
        try { $process.StandardOutput.BaseStream.CopyTo($file) } finally { $file.Dispose() }
        $process.WaitForExit()
        if ($process.ExitCode) { Remove-Item -LiteralPath $partial -Force; throw "Download failed with exit code $($process.ExitCode)." }

        $actual = (Get-FileHash -LiteralPath $partial -Algorithm SHA256).Hash
        $bytes = [byte[]]::new(5)
        $stream = [IO.File]::OpenRead($partial)
        try { $read = $stream.Read($bytes, 0, 5) } finally { $stream.Dispose() }
        $header = [Text.Encoding]::ASCII.GetString($bytes, 0, $read)
        if ($actual -ne $expected -or $header -ne 'PGDMP') {
            Remove-Item -LiteralPath $partial -Force
            throw "Verification failed for $name."
        }
        Move-Item -LiteralPath $partial -Destination $target -Force
        Write-Log ('Copied {0} ({1:N0} bytes, sha256 {2})' -f $name, (Get-Item -LiteralPath $target).Length, $actual)
    }

    Get-ChildItem -LiteralPath $Destination -Filter 'thiscord-*.dump' |
        Sort-Object Name -Descending | Select-Object -Skip $Keep |
        ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force; Write-Log "Pruned $($_.Name)" }
} catch {
    Write-Log "FAILED: $($_.Exception.Message)"
    exit 1
}
