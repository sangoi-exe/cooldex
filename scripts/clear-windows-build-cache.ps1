# Merge-safety anchor: this helper has one destructive authority only: after
# explicit proof of quiescence, it can remove direct children of literal
# F:\.cache while preserving that root. Nested junctions are removed as leaf
# entries without traversing their targets. Test seams are opt-in and confined.
[CmdletBinding()]
param(
    [switch]$Delete,
    [string]$TestRoot,
    [string]$TestFixture
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$script:ProductionTarget = "F:\.cache"
$script:TestPrefix = "F:\.cache\cw\cleanup-tests"
$script:TestOptIn = "COOLDEX_WINDOWS_CACHE_CLEANUP_TEST_ONLY"
$script:MutexName = "Local\Cooldex.WindowsBuildCacheCleanup.v1"
$script:HasTestRoot = $PSBoundParameters.ContainsKey("TestRoot")
$script:HasTestFixture = $PSBoundParameters.ContainsKey("TestFixture")
$script:DeletedCount = 0
$script:Result = [ordered]@{
    schema = "cooldex.windows-cache-cleanup.v1"
    mode = if ($Delete) { "delete" } else { "preflight" }
    target = $script:ProductionTarget
    status = "failed"
    deleted_count = 0
    remaining_count = $null
    windows_scan_status = "not-run"
    wsl_scan_status = "not-run"
    windows_second_scan_status = "not-run"
    wsl_second_scan_status = "not-run"
    message = $null
    error = $null
}

function Stop-Cleanup([string]$Message) {
    throw [System.InvalidOperationException]::new($Message)
}

function Same-Path([string]$Left, [string]$Right) {
    return [string]::Equals($Left, $Right, [System.StringComparison]::OrdinalIgnoreCase)
}

function Is-Below([string]$Child, [string]$Root) {
    $childFull = [System.IO.Path]::GetFullPath($Child)
    $rootFull = [System.IO.Path]::GetFullPath($Root).TrimEnd("\\")
    return $childFull.StartsWith($rootFull + "\", [System.StringComparison]::OrdinalIgnoreCase)
}

function Is-Reparse([System.IO.FileSystemInfo]$Item) {
    return (([int]$Item.Attributes -band [int][System.IO.FileAttributes]::ReparsePoint) -ne 0)
}

function Is-Junction([System.IO.FileSystemInfo]$Item) {
    if (-not (Is-Reparse $Item)) { return $false }
    $linkType = $Item.PSObject.Properties["LinkType"]
    return $null -ne $linkType -and [string]$linkType.Value -eq "Junction"
}

function Read-Fixture([string]$Path) {
    try {
        $fixture = Get-Content -LiteralPath $Path -Raw -ErrorAction Stop | ConvertFrom-Json -AsHashtable -ErrorAction Stop
    } catch {
        Stop-Cleanup "test fixture is unreadable JSON: $($_.Exception.Message)"
    }
    if ($fixture -isnot [System.Collections.IDictionary]) {
        Stop-Cleanup "test fixture must be a JSON object"
    }
    return $fixture
}

function New-Context {
    if ($script:HasTestRoot -xor $script:HasTestFixture) {
        Stop-Cleanup "TestRoot and TestFixture must be supplied together"
    }
    if (-not $script:HasTestRoot) {
        return [pscustomobject]@{ target = $script:ProductionTarget; fixture = $null; test = $false }
    }
    if ([System.Environment]::GetEnvironmentVariable($script:TestOptIn) -ne "1") {
        Stop-Cleanup "$($script:TestOptIn)=1 is required for test overrides"
    }
    if ([string]::IsNullOrWhiteSpace($TestRoot) -or [string]::IsNullOrWhiteSpace($TestFixture)) {
        Stop-Cleanup "TestRoot and TestFixture must not be empty"
    }
    if (-not [System.IO.Path]::IsPathFullyQualified($TestRoot)) {
        Stop-Cleanup "TestRoot must be fully qualified"
    }
    $target = [System.IO.Path]::GetFullPath($TestRoot)
    if (-not (Same-Path $target $TestRoot)) {
        Stop-Cleanup "TestRoot must already be normalized"
    }
    if (-not (Is-Below $target $script:TestPrefix)) {
        Stop-Cleanup "TestRoot must be below $($script:TestPrefix)"
    }
    return [pscustomobject]@{ target = $target; fixture = Read-Fixture ([System.IO.Path]::GetFullPath($TestFixture)); test = $true }
}

function Assert-Root([string]$Target) {
    try {
        $drive = Get-Item -LiteralPath "F:\" -Force -ErrorAction Stop
        $item = Get-Item -LiteralPath $Target -Force -ErrorAction Stop
    } catch {
        Stop-Cleanup "target or F:\ is missing or inaccessible: $($_.Exception.Message)"
    }
    if (-not $drive.PSIsContainer -or -not $item.PSIsContainer) {
        Stop-Cleanup "F:\ and target must be directories"
    }
    $full = [System.IO.Path]::GetFullPath($item.FullName)
    if (-not (Same-Path $full $Target)) {
        Stop-Cleanup "target normalized to an unexpected path"
    }
    if (Is-Reparse $item) {
        Stop-Cleanup "target must not be a reparse point"
    }
    return $full
}

function Get-Tree([string]$Root) {
    $records = [System.Collections.Generic.List[string]]::new()
    $direct = [System.Collections.Generic.List[string]]::new()
    $pending = [System.Collections.Generic.Stack[string]]::new()
    $pending.Push($Root)
    while ($pending.Count -gt 0) {
        $directory = $pending.Pop()
        try {
            $children = @(Get-ChildItem -LiteralPath $directory -Force -ErrorAction Stop)
        } catch {
            Stop-Cleanup "tree enumeration failed at ${directory}: $($_.Exception.Message)"
        }
        foreach ($item in $children) {
            $full = [System.IO.Path]::GetFullPath($item.FullName)
            if (-not (Is-Below $full $Root)) {
                Stop-Cleanup "tree path escapes target: $full"
            }
            $isReparse = Is-Reparse $item
            if ($isReparse -and -not (Is-Junction $item)) {
                Stop-Cleanup "tree contains an unsupported reparse point: $full"
            }
            if (Same-Path $directory $Root) {
                $direct.Add($full)
            }
            $relative = $full.Substring($Root.Length).TrimStart("\\")
            $length = if ($item.PSIsContainer) { 0 } else { [int64]$item.Length }
            $stamp = if ($item.PSIsContainer) { 0 } else { $item.LastWriteTimeUtc.Ticks }
            $kind = if ($isReparse) { "junction" } else { "ordinary" }
            $records.Add("$relative|$kind|$($item.Attributes)|$length|$stamp")
            if ($item.PSIsContainer -and -not $isReparse) {
                $pending.Push($full)
            }
        }
    }
    $tree = [string[]]$records.ToArray()
    $children = [string[]]$direct.ToArray()
    [System.Array]::Sort($tree, [System.StringComparer]::Ordinal)
    [System.Array]::Sort($children, [System.StringComparer]::OrdinalIgnoreCase)
    $bytes = [System.Text.Encoding]::UTF8.GetBytes(($tree -join "`n"))
    $digest = [Convert]::ToHexString([System.Security.Cryptography.SHA256]::HashData($bytes))
    return [pscustomobject]@{ direct = $children; identity = $digest }
}

function Get-FixtureScan([hashtable]$Fixture, [string]$Name, [int]$Pass) {
    if (-not $Fixture.Contains($Name) -or $Fixture[$Name] -is [string] -or $Fixture[$Name] -is [System.Collections.IDictionary] -or $Fixture[$Name] -isnot [System.Collections.IEnumerable]) {
        Stop-Cleanup "fixture.$Name must be an array"
    }
    $scans = @($Fixture[$Name])
    if ($scans.Count -lt $Pass -or $scans[$Pass - 1] -isnot [System.Collections.IDictionary]) {
        Stop-Cleanup "fixture.$Name has no valid scan for pass $Pass"
    }
    return $scans[$Pass - 1]
}

function Scan-Windows([pscustomobject]$Context, [int]$Pass) {
    $status = if ($Pass -eq 1) { "windows_scan_status" } else { "windows_second_scan_status" }
    try {
        if ($null -eq $Context.fixture) {
            $processes = @(Get-CimInstance -ClassName Win32_Process -ErrorAction Stop)
        } else {
            $scan = Get-FixtureScan $Context.fixture "windows" $Pass
            if ($scan.Contains("error")) { Stop-Cleanup "fixture simulates CIM failure: $($scan["error"])" }
            if (-not $scan.Contains("processes") -or $scan["processes"] -is [string] -or $scan["processes"] -isnot [System.Collections.IEnumerable]) {
                Stop-Cleanup "fixture Windows processes must be an array"
            }
            $processes = @($scan["processes"])
        }
        foreach ($process in $processes) {
            if ($process -is [System.Collections.IDictionary]) {
                if (-not $process.Contains("ProcessId") -or -not $process.Contains("Name") -or -not $process.Contains("ExecutablePath") -or -not $process.Contains("CommandLine")) {
                    Stop-Cleanup "fixture Windows process is malformed"
                }
                $processId = $process["ProcessId"]
                $name = $process["Name"]
                $executablePath = $process["ExecutablePath"]
                $commandLine = $process["CommandLine"]
            } else {
                $processId = $process.ProcessId
                $name = $process.Name
                $executablePath = $process.ExecutablePath
                $commandLine = $process.CommandLine
            }
            try { $processId = [uint32]$processId } catch { Stop-Cleanup "Windows process PID is invalid" }
            if ([string]::IsNullOrWhiteSpace([string]$name)) { Stop-Cleanup "Windows process name is missing" }
            if (($null -ne $executablePath -and $executablePath -isnot [string]) -or ($null -ne $commandLine -and $commandLine -isnot [string])) {
                Stop-Cleanup "Windows process metadata is invalid"
            }
            if ($processId -eq [uint32]$PID) { continue }
            $lowerName = ([string]$name).ToLowerInvariant()
            $referencesCache = ($executablePath -is [string] -and $executablePath.IndexOf($script:ProductionTarget, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) -or ($commandLine -is [string] -and $commandLine.IndexOf($script:ProductionTarget, [System.StringComparison]::OrdinalIgnoreCase) -ge 0)
            if ($lowerName -in @("cargo.exe", "cargo-nextest.exe", "rustc.exe", "rustdoc.exe") -or $referencesCache) {
                $script:Result[$status] = "blocked"
                Stop-Cleanup "active Windows writer $name (PID $processId) blocks cleanup"
            }
        }
        $script:Result[$status] = "clean"
    } catch {
        if ($script:Result[$status] -ne "blocked") { $script:Result[$status] = "error" }
        throw
    }
}

function Get-WslText([pscustomobject]$Context, [int]$Pass) {
    if ($null -ne $Context.fixture) {
        $scan = Get-FixtureScan $Context.fixture "wsl" $Pass
        if ($scan.Contains("error")) { Stop-Cleanup "fixture simulates wsl.exe failure: $($scan["error"])" }
        if (-not $scan.Contains("exit_code") -or -not $scan.Contains("stdout") -or $scan["stdout"] -isnot [string] -or [int]$scan["exit_code"] -ne 0) {
            Stop-Cleanup "fixture WSL result is invalid"
        }
        return [string]$scan["stdout"]
    }
    $info = [System.Diagnostics.ProcessStartInfo]::new()
    $info.FileName = "wsl.exe"
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in @("--exec", "ps", "-eo", "pid=,ppid=,comm=,args=")) { $null = $info.ArgumentList.Add($argument) }
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $info
    try {
        if (-not $process.Start()) { Stop-Cleanup "wsl.exe did not start" }
        $out = $process.StandardOutput.ReadToEndAsync()
        $err = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(30000)) {
            try { $process.Kill($true) } catch {}
            Stop-Cleanup "wsl.exe ps timed out"
        }
        $stdout = $out.GetAwaiter().GetResult()
        $stderr = $err.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { Stop-Cleanup "wsl.exe ps failed: $stderr" }
        return $stdout
    } finally {
        $process.Dispose()
    }
}

function Scan-Wsl([pscustomobject]$Context, [int]$Pass) {
    $status = if ($Pass -eq 1) { "wsl_scan_status" } else { "wsl_second_scan_status" }
    try {
        $records = 0
        foreach ($line in ((Get-WslText $Context $Pass) -split '\r?\n')) {
            if ([string]::IsNullOrWhiteSpace($line)) { continue }
            if ($line -notmatch "^\s*(?<pid>\d+)\s+(?<ppid>\d+)\s+(?<comm>\S+)(?:\s+(?<args>.*))?$") {
                Stop-Cleanup "wsl.exe ps output is not parseable: $line"
            }
            $records++
            $comm = $Matches["comm"].ToLowerInvariant()
            if ($comm -in @("cargo", "cargo-nextest", "rustc", "rustdoc", "just") -or $line.IndexOf("/mnt/f/.cache", [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
                $script:Result[$status] = "blocked"
                Stop-Cleanup "active WSL writer blocks cleanup: $line"
            }
        }
        if ($records -eq 0) { Stop-Cleanup "wsl.exe ps output is empty" }
        $script:Result[$status] = "clean"
    } catch {
        if ($script:Result[$status] -ne "blocked") { $script:Result[$status] = "error" }
        throw
    }
}

function Enter-CleanupMutex {
    $mutex = [System.Threading.Mutex]::new($false, $script:MutexName)
    try {
        if (-not $mutex.WaitOne(0)) {
            $mutex.Dispose()
            Stop-Cleanup "another cache cleanup instance holds the mutex"
        }
    } catch [System.Threading.AbandonedMutexException] {
        $mutex.Dispose()
        Stop-Cleanup "cache cleanup mutex was abandoned"
    }
    return $mutex
}

function Invoke-TestMutation([pscustomobject]$Context, [string]$Root) {
    if (-not $Context.test -or -not $Context.fixture.Contains("add_before_second")) { return }
    $leaf = [string]$Context.fixture["add_before_second"]
    if ([string]::IsNullOrWhiteSpace($leaf) -or [System.IO.Path]::GetFileName($leaf) -ne $leaf) {
        Stop-Cleanup "fixture add_before_second must be one file name"
    }
    [System.IO.File]::WriteAllText((Join-Path $Root $leaf), "fixture mutation")
}

function Remove-Child([pscustomobject]$Context, [string]$Root, [string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    $full = [System.IO.Path]::GetFullPath($item.FullName)
    if (-not (Same-Path ([System.IO.Path]::GetDirectoryName($full)) $Root)) {
        Stop-Cleanup "captured child is no longer a direct supported child"
    }
    $isReparse = Is-Reparse $item
    if ($isReparse -and -not (Is-Junction $item)) {
        Stop-Cleanup "captured child is no longer a direct supported child"
    }
    if ($Context.test -and $Context.fixture.Contains("fail_on") -and ([string]$Context.fixture["fail_on"] -eq $item.Name)) {
        Stop-Cleanup "fixture deletion failure for $($item.Name)"
    }
    if ($isReparse) {
        Remove-Item -LiteralPath $full -Force -ErrorAction Stop
        return
    }
    Remove-Item -LiteralPath $full -Recurse -Force -ErrorAction Stop
}

$context = $null
$mutex = $null
$exitCode = 1
try {
    $context = New-Context
    $script:Result.target = $context.target
    if (-not $context.test -and -not (Same-Path $context.target $script:ProductionTarget)) {
        Stop-Cleanup "production target must remain literal $($script:ProductionTarget)"
    }
    if (-not $Delete) {
        $root = Assert-Root $context.target
        $tree = Get-Tree $root
        Scan-Windows $context 1
        Scan-Wsl $context 1
        $script:Result.status = "preflight-ok"
        $script:Result.remaining_count = $tree.direct.Count
        $script:Result.message = "preflight passed; no deletion was requested"
        $exitCode = 0
    } else {
        $mutex = Enter-CleanupMutex
        $root = Assert-Root $context.target
        $before = Get-Tree $root
        Scan-Windows $context 1
        Scan-Wsl $context 1
        Invoke-TestMutation $context $root
        $root = Assert-Root $context.target
        Scan-Windows $context 2
        Scan-Wsl $context 2
        $after = Get-Tree $root
        if ($before.identity -ne $after.identity -or $before.direct.Count -ne $after.direct.Count -or (@($before.direct) -join "`0") -ne (@($after.direct) -join "`0")) {
            Stop-Cleanup "tree changed between snapshots"
        }
        foreach ($child in $before.direct) {
            $root = Assert-Root $context.target
            Remove-Child $context $root $child
            $script:DeletedCount++
        }
        $root = Assert-Root $context.target
        $final = Get-Tree $root
        if ($final.direct.Count -ne 0) { Stop-Cleanup "target still has direct children after deletion" }
        $script:Result.status = "deleted"
        $script:Result.remaining_count = 0
        $script:Result.message = "deleted captured direct children; target root remains"
        $exitCode = 0
    }
} catch {
    $failure = $_.Exception.Message
    if ($null -ne $context) {
        try {
            $safeRoot = Assert-Root $context.target
            $script:Result.remaining_count = (Get-Tree $safeRoot).direct.Count
        } catch {}
    }
    $script:Result.status = "failed"
    $script:Result.message = "cleanup stopped without retry"
    $script:Result.error = $failure
} finally {
    $script:Result.deleted_count = $script:DeletedCount
    if ($null -ne $mutex) {
        try { $mutex.ReleaseMutex() } catch {}
        $mutex.Dispose()
    }
}
[Console]::Out.WriteLine(($script:Result | ConvertTo-Json -Depth 8 -Compress))
exit $exitCode
