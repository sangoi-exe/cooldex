# Merge-safety anchor: this native Windows executor consumes only the planner-owned
# frozen manifest, materializes its indexed candidate below F:\.cache, and never
# introduces a second planner, artifact-pin owner, or mutable C-drive state.
param(
    [Parameter(Mandatory = $true)]
    [string]$Manifest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$script:CacheRoot = $null
$script:Runtime = $null
$script:TestOnlyFakeCargoOptIn = "CARGO_VALIDATE_WINDOWS_TEST_ONLY_FAKE_CARGO"
$script:TestOnlyPreflightFixtureOptIn = "CARGO_VALIDATE_WINDOWS_TEST_ONLY_PREFLIGHT_FIXTURES"
$script:TestOnlyFakeCreateIgnoredFile = "CARGO_VALIDATE_WINDOWS_TEST_FAKE_CREATE_IGNORED"
$script:TestOnlyBootstrapFixtureOptIn = "CARGO_VALIDATE_WINDOWS_TEST_ONLY_BOOTSTRAP_FIXTURES"
$script:NativeMutexName = "Local\Cooldex.WindowsBuildCacheCleanup.v1"
$script:NativeMutex = $null
$script:NativeMutexHeld = $false
$script:NativeGit = $null
$script:DirectToolchain = $null
$script:Bootstrap = $null
$script:GitEnvironment = $null
$script:Materialization = $null
$script:SourceSnapshotBefore = $null
$script:SourceRepository = $null
$script:SourceExcludesFile = $null
$script:SourceFinalized = $false
$script:RunPaths = $null
$script:Preflight = $null
$script:CommandResults = [System.Collections.Generic.List[object]]::new()
$script:LivePreflightChecks = [System.Collections.Generic.List[object]]::new()
$script:ExitCode = 1
$script:Status = "preflight-failed"
$script:Failure = $null

function Fail-Manifest {
    param([string]$Message)

    throw [System.InvalidOperationException]::new($Message)
}

function Get-MapValue {
    param(
        [System.Collections.IDictionary]$Map,
        [string]$Name
    )

    if (-not $Map.Contains($Name)) {
        Fail-Manifest "manifest field '$Name' is required"
    }
    return [pscustomobject]@{ value = $Map[$Name] }
}

function Get-RequiredString {
    param(
        [object]$Value,
        [string]$Name
    )

    if ($Value -isnot [string] -or [string]::IsNullOrWhiteSpace($Value)) {
        Fail-Manifest "$Name must be a non-empty string"
    }
    return [string]$Value
}

function Get-RequiredSha256 {
    param(
        [object]$Value,
        [string]$Name,
        [switch]$Lowercase
    )

    $text = Get-RequiredString $Value $Name
    $pattern = if ($Lowercase) { "^[0-9a-f]{64}$" } else { "^[0-9a-fA-F]{64}$" }
    if ($text -notmatch $pattern) {
        $caseDescription = if ($Lowercase) { "lowercase " } else { "" }
        Fail-Manifest "$Name must be a ${caseDescription}64-character SHA-256 digest"
    }
    return $text
}

function Get-RequiredMap {
    param(
        [object]$Value,
        [string]$Name
    )

    if ($Value -isnot [System.Collections.IDictionary]) {
        Fail-Manifest "$Name must be a JSON object"
    }
    return $Value
}

function Get-RequiredArray {
    param(
        [object]$Value,
        [string]$Name
    )

    if (
        $null -eq $Value -or
        $Value -is [string] -or
        $Value -is [System.Collections.IDictionary] -or
        $Value -isnot [System.Collections.IEnumerable]
    ) {
        Fail-Manifest "$Name must be a JSON array"
    }
    return [pscustomobject]@{ items = @($Value) }
}

function Assert-ExactKeys {
    param(
        [System.Collections.IDictionary]$Map,
        [string[]]$Expected,
        [string]$Context
    )

    $missing = [System.Collections.Generic.List[string]]::new()
    foreach ($name in $Expected) {
        if (-not $Map.Contains($name)) {
            $missing.Add($name)
        }
    }
    $unknown = [System.Collections.Generic.List[string]]::new()
    foreach ($name in $Map.Keys) {
        if ($Expected -notcontains [string]$name) {
            $unknown.Add([string]$name)
        }
    }
    if ($missing.Count -ne 0 -or $unknown.Count -ne 0) {
        $detail = [System.Collections.Generic.List[string]]::new()
        if ($missing.Count -ne 0) {
            $detail.Add("missing keys: $($missing -join ', ')")
        }
        if ($unknown.Count -ne 0) {
            $detail.Add("unknown keys: $($unknown -join ', ')")
        }
        Fail-Manifest "$Context has an unsupported object shape ($($detail -join '; '))"
    }
}

function Assert-RequiredAndAllowedKeys {
    param(
        [System.Collections.IDictionary]$Map,
        [string[]]$Required,
        [string[]]$Allowed,
        [string]$Context
    )

    $missing = [System.Collections.Generic.List[string]]::new()
    foreach ($name in $Required) {
        if (-not $Map.Contains($name)) {
            $missing.Add($name)
        }
    }
    $unknown = [System.Collections.Generic.List[string]]::new()
    foreach ($name in $Map.Keys) {
        if ($Allowed -notcontains [string]$name) {
            $unknown.Add([string]$name)
        }
    }
    if ($missing.Count -ne 0 -or $unknown.Count -ne 0) {
        $detail = [System.Collections.Generic.List[string]]::new()
        if ($missing.Count -ne 0) {
            $detail.Add("missing keys: $($missing -join ', ')")
        }
        if ($unknown.Count -ne 0) {
            $detail.Add("unknown keys: $($unknown -join ', ')")
        }
        Fail-Manifest "$Context has an unsupported object shape ($($detail -join '; '))"
    }
}

function Get-RequiredPositiveInt {
    param(
        [object]$Value,
        [string]$Name,
        [switch]$AllowZero
    )

    if (
        $Value -is [bool] -or
        ($Value -isnot [byte] -and
        $Value -isnot [int16] -and
        $Value -isnot [int] -and
        $Value -isnot [int64] -and
        $Value -isnot [uint16] -and
        $Value -isnot [uint32] -and
        $Value -isnot [uint64])
    ) {
        Fail-Manifest "$Name must be an integer"
    }
    $number = [int64]$Value
    if ($AllowZero) {
        if ($number -lt 0) {
            Fail-Manifest "$Name must be a non-negative integer"
        }
    } elseif ($number -le 0) {
        Fail-Manifest "$Name must be a positive integer"
    }
    return $number
}

function Assert-StringArray {
    param(
        [object]$Value,
        [string]$Name
    )

    $items = (Get-RequiredArray $Value $Name).items
    foreach ($item in $items) {
        $null = Get-RequiredString $item "$Name entry"
    }
    return $items
}

function Assert-StringMap {
    param(
        [System.Collections.IDictionary]$Map,
        [string]$Name
    )

    foreach ($key in $Map.Keys) {
        if ($key -isnot [string] -or $key -notmatch "^[A-Za-z_][A-Za-z0-9_]*$") {
            Fail-Manifest "$Name contains an unsafe environment key"
        }
        if ($Map[$key] -isnot [string]) {
            Fail-Manifest "$Name values must be strings"
        }
    }
}

function Assert-RunPath {
    param([string]$Path)

    if ([string]::IsNullOrWhiteSpace($script:CacheRoot)) {
        Fail-Manifest "run path validation requires a validated Windows runtime"
    }
    $fullPath = [System.IO.Path]::GetFullPath($Path)
    $prefix = "$($script:CacheRoot)\"
    if (
        -not $fullPath.Equals($script:CacheRoot, [System.StringComparison]::OrdinalIgnoreCase) -and
        -not $fullPath.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)
    ) {
        Fail-Manifest "run state must resolve below literal $($script:CacheRoot): $fullPath"
    }
    return $fullPath
}

function Assert-FixedCacheAncestorIsNotReparsePoint {
    param([string]$Path)

    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }
    $attributes = (Get-Item -LiteralPath $Path -Force).Attributes
    if (($attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        Fail-Manifest "fixed F-drive cache ancestor is a reparse point: $Path"
    }
}

function Get-SafeHttpsPin {
    param(
        [object]$Value,
        [string]$Name
    )

    $text = Get-RequiredString $Value $Name
    if ($text.Length -gt 2048 -or $text.IndexOfAny([char[]]@(0, 10, 13)) -ge 0) {
        Fail-Manifest "$Name must be a safe HTTPS pin string"
    }
    $uri = $null
    if (-not [System.Uri]::TryCreate($text, [System.UriKind]::Absolute, [ref]$uri)) {
        Fail-Manifest "$Name must be a safe HTTPS pin string"
    }
    if (
        $uri.Scheme -cne "https" -or
        [string]::IsNullOrWhiteSpace($uri.Host) -or
        $uri.HostNameType -ne [System.UriHostNameType]::Dns -or
        $uri.Host -notmatch "\." -or
        -not [string]::IsNullOrEmpty($uri.UserInfo) -or
        -not [string]::IsNullOrEmpty($uri.Query) -or
        -not [string]::IsNullOrEmpty($uri.Fragment)
    ) {
        Fail-Manifest "$Name must be a safe HTTPS pin string"
    }
    return $text
}

function Get-SafePosixSourceRoot {
    param(
        [object]$Value,
        [string]$Name
    )

    $path = Get-RequiredString $Value $Name
    if ($path.Length -gt 1024 -or -not $path.StartsWith("/", [System.StringComparison]::Ordinal)) {
        Fail-Manifest "$Name must be an absolute POSIX source root"
    }
    if ($path.IndexOfAny([char[]]@(0, 10, 13, 92)) -ge 0) {
        Fail-Manifest "$Name contains an unsafe path character"
    }
    $segments = $path.Substring(1).Split("/", [System.StringSplitOptions]::None)
    if ($segments.Count -eq 0 -or ($segments.Count -eq 1 -and [string]::IsNullOrEmpty($segments[0]))) {
        Fail-Manifest "$Name must name a repository below the POSIX root"
    }
    foreach ($segment in $segments) {
        if (
            [string]::IsNullOrEmpty($segment) -or
            $segment -in @(".", "..") -or
            $segment -notmatch "^[A-Za-z0-9._-]+$"
        ) {
            Fail-Manifest "$Name must use safe POSIX path components"
        }
    }
    return $path
}

function Get-SafeWslDistroName {
    param(
        [object]$Value,
        [string]$Name
    )

    $name = Get-RequiredString $Value $Name
    if ($name.Length -gt 64 -or $name -notmatch "^[A-Za-z0-9][A-Za-z0-9._-]*$") {
        Fail-Manifest "$Name must be a safe non-empty WSL distribution name"
    }
    return $name
}

function Get-SafeWorkflowNamespace {
    param(
        [object]$Value,
        [string]$Name
    )

    $namespace = Get-RequiredString $Value $Name
    if (
        $namespace.Length -gt 64 -or
        $namespace -in @(".", "..") -or
        $namespace -notmatch "^[A-Za-z0-9][A-Za-z0-9_-]*(?:\.[A-Za-z0-9][A-Za-z0-9_-]*)*\z" -or
        $namespace.ToLowerInvariant() -in @(
            "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9",
            "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9"
        )
    ) {
        Fail-Manifest "$Name must be a safe single namespace"
    }
    return $namespace
}

function Get-ReuseRunRoot {
    param(
        [object]$Value,
        [string]$CacheRoot,
        [string]$Name
    )

    if ($null -eq $Value) {
        return $null
    }
    $path = Get-RequiredString $Value $Name
    $fullPath = [System.IO.Path]::GetFullPath($path)
    $prefix = "$CacheRoot\"
    if (
        -not [System.IO.Path]::IsPathFullyQualified($fullPath) -or
        $fullPath.Equals($CacheRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
        -not $fullPath.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)
    ) {
        Fail-Manifest "$Name must be an absolute working-set root below literal $CacheRoot"
    }
    return $fullPath
}

function Get-WindowsRuntime {
    param([System.Collections.IDictionary]$ManifestData)

    $runtime = Get-RequiredMap (Get-MapValue $ManifestData "windows_runtime").value "windows_runtime"
    $expected = @(
        "cache_root",
        "workflow_namespace",
        "reuse_run_root",
        "minimum_free_disk_gib",
        "minimum_available_memory_gib",
        "target",
        "rust_toolchain",
        "nextest_version",
        "nextest_url",
        "nextest_sha256",
        "v8_version",
        "v8_archive_url",
        "v8_archive_sha256",
        "v8_binding_url",
        "v8_binding_sha256",
        "resource_contract",
        "source_materialization"
    )
    Assert-ExactKeys $runtime $expected "windows_runtime"

    $cacheRoot = Get-RequiredString $runtime["cache_root"] "windows_runtime.cache_root"
    if ($cacheRoot -cne "F:\.cache") {
        Fail-Manifest "windows_runtime.cache_root must be literal F:\.cache"
    }
    $namespace = Get-SafeWorkflowNamespace $runtime["workflow_namespace"] "windows_runtime.workflow_namespace"
    $reuseRunRoot = Get-ReuseRunRoot $runtime["reuse_run_root"] $cacheRoot "windows_runtime.reuse_run_root"

    $minimumFreeDisk = Get-RequiredPositiveInt $runtime["minimum_free_disk_gib"] "windows_runtime.minimum_free_disk_gib"
    $minimumDiskFloor = if ($null -eq $reuseRunRoot) { 120 } else { 1 }
    if ($minimumFreeDisk -lt $minimumDiskFloor) {
        $route = if ($null -eq $reuseRunRoot) { "cold" } else { "reused" }
        Fail-Manifest "windows_runtime.minimum_free_disk_gib must be at least $minimumDiskFloor for the $route route"
    }
    $minimumMemory = Get-RequiredPositiveInt $runtime["minimum_available_memory_gib"] "windows_runtime.minimum_available_memory_gib"
    if ($minimumMemory -lt 30) {
        Fail-Manifest "windows_runtime.minimum_available_memory_gib must be at least 30"
    }

    $target = Get-RequiredString $runtime["target"] "windows_runtime.target"
    if ($target -cne "x86_64-pc-windows-msvc") {
        Fail-Manifest "windows_runtime.target must be x86_64-pc-windows-msvc"
    }
    $toolchain = Get-RequiredString $runtime["rust_toolchain"] "windows_runtime.rust_toolchain"
    if ($toolchain -notmatch "^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?-x86_64-pc-windows-msvc$") {
        Fail-Manifest "windows_runtime.rust_toolchain has an unsupported Windows toolchain shape"
    }
    foreach ($versionField in @("nextest_version", "v8_version")) {
        if ((Get-RequiredString $runtime[$versionField] "windows_runtime.$versionField") -notmatch "^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$") {
            Fail-Manifest "windows_runtime.$versionField must be a semantic version"
        }
    }
    foreach ($urlField in @("nextest_url", "v8_archive_url", "v8_binding_url")) {
        $null = Get-SafeHttpsPin $runtime[$urlField] "windows_runtime.$urlField"
    }
    foreach ($digestField in @("nextest_sha256", "v8_archive_sha256", "v8_binding_sha256")) {
        $null = Get-RequiredSha256 $runtime[$digestField] "windows_runtime.$digestField" -Lowercase
    }

    $resource = Get-RequiredMap $runtime["resource_contract"] "windows_runtime.resource_contract"
    Assert-ExactKeys $resource @("resource_profile", "cargo_build_jobs", "nextest_test_threads") "windows_runtime.resource_contract"
    $resourceProfile = Get-RequiredString $resource["resource_profile"] "windows_runtime.resource_contract.resource_profile"
    if ($resourceProfile -cne "windows_nextest") {
        Fail-Manifest "windows_runtime.resource_contract.resource_profile must be windows_nextest"
    }
    $resourceContract = [ordered]@{
        resource_profile = $resourceProfile
        cargo_build_jobs = Get-RequiredPositiveInt $resource["cargo_build_jobs"] "windows_runtime.resource_contract.cargo_build_jobs"
        nextest_test_threads = Get-RequiredPositiveInt $resource["nextest_test_threads"] "windows_runtime.resource_contract.nextest_test_threads"
    }

    $source = Get-RequiredMap $runtime["source_materialization"] "windows_runtime.source_materialization"
    Assert-ExactKeys $source @("posix_repo_root", "wsl_distro_name") "windows_runtime.source_materialization"
    $posixRoot = Get-SafePosixSourceRoot $source["posix_repo_root"] "windows_runtime.source_materialization.posix_repo_root"
    $distroName = Get-SafeWslDistroName $source["wsl_distro_name"] "windows_runtime.source_materialization.wsl_distro_name"
    $segments = $posixRoot.Substring(1).Split("/", [System.StringSplitOptions]::None)
    $sourceUnc = "\\wsl.localhost\$distroName\$($segments -join '\')"

    return [pscustomobject]@{
        cache_root = $cacheRoot
        workflow_namespace = $namespace
        reuse_run_root = $reuseRunRoot
        minimum_free_disk_gib = $minimumFreeDisk
        minimum_available_memory_gib = $minimumMemory
        target = $target
        rust_toolchain = $toolchain
        nextest_version = [string]$runtime["nextest_version"]
        nextest_url = [string]$runtime["nextest_url"]
        nextest_sha256 = [string]$runtime["nextest_sha256"]
        v8_version = [string]$runtime["v8_version"]
        v8_archive_url = [string]$runtime["v8_archive_url"]
        v8_archive_sha256 = [string]$runtime["v8_archive_sha256"]
        v8_binding_url = [string]$runtime["v8_binding_url"]
        v8_binding_sha256 = [string]$runtime["v8_binding_sha256"]
        resource_contract = [pscustomobject]$resourceContract
        source_materialization = [pscustomobject]@{
            posix_repo_root = $posixRoot
            wsl_distro_name = $distroName
            source_unc = $sourceUnc
            source_git_unc = (Join-Path $sourceUnc ".git")
        }
    }
}

function New-RunPaths {
    param(
        [string]$Namespace,
        [object]$ReuseRunRoot
    )

    $Namespace = Get-SafeWorkflowNamespace $Namespace "windows_runtime.workflow_namespace"
    $runId = "{0}-{1}" -f [DateTime]::UtcNow.ToString("yyyyMMddHHmmssfff"), [Guid]::NewGuid().ToString("N")
    $tempLeaf = "p$([Guid]::NewGuid().ToString('N'))"
    $executorRoot = Join-Path $script:CacheRoot $Namespace
    $runContainer = Join-Path $executorRoot "r"
    $executionRoot = Join-Path $runContainer $runId
    $isReuse = $null -ne $ReuseRunRoot
    $workingSetRoot = if ($isReuse) {
        Assert-RunPath $ReuseRunRoot
    } else {
        Assert-RunPath $executionRoot
    }
    $paths = [ordered]@{
        cache_root = $script:CacheRoot
        executor_root = $executorRoot
        run_root = $workingSetRoot
        evidence_dir = (Join-Path $executionRoot "e")
        temp_dir = (Join-Path $script:CacheRoot $tempLeaf)
        target_dir = (Join-Path $workingSetRoot "target")
        cargo_home = (Join-Path $workingSetRoot "cargo")
        rustup_home = (Join-Path $workingSetRoot "rustup")
        candidate_root = (Join-Path $workingSetRoot "candidate")
        helper_state = (Join-Path $workingSetRoot "helper")
        tool_staging = (Join-Path $workingSetRoot "tools")
        v8_cache = (Join-Path $workingSetRoot "v8")
    }

    foreach ($ancestor in @("F:\", $script:CacheRoot, $executorRoot, $runContainer)) {
        Assert-FixedCacheAncestorIsNotReparsePoint $ancestor
    }
    if ($isReuse) {
        if (-not (Test-Path -LiteralPath $workingSetRoot)) {
            Fail-Manifest "selected reuse working-set root does not exist: $workingSetRoot"
        }
        $workingSetItem = Get-Item -LiteralPath $workingSetRoot -Force
        if (
            $workingSetItem -isnot [System.IO.DirectoryInfo] -or
            ($workingSetItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            Fail-Manifest "selected reuse working-set root must be a non-reparse directory: $workingSetRoot"
        }
    }
    foreach ($path in @($executionRoot, $paths.evidence_dir, $paths.temp_dir)) {
        $resolved = Assert-RunPath ([string]$path)
        if (Test-Path -LiteralPath $resolved) {
            Fail-Manifest "fresh execution path already exists: $resolved"
        }
        $null = [System.IO.Directory]::CreateDirectory($resolved)
    }
    if (-not $isReuse) {
        foreach ($name in @("target_dir", "cargo_home", "rustup_home", "helper_state", "tool_staging", "v8_cache")) {
            $resolved = Assert-RunPath ([string]$paths[$name])
            $null = [System.IO.Directory]::CreateDirectory($resolved)
        }
    }
    foreach ($name in @("executor_root", "run_root", "evidence_dir", "temp_dir", "target_dir", "cargo_home", "rustup_home", "candidate_root", "helper_state", "tool_staging", "v8_cache")) {
        $paths[$name] = Assert-RunPath ([string]$paths[$name])
    }
    return [pscustomobject]$paths
}

function Assert-ReuseReceiptPath {
    param(
        [object]$Value,
        [string]$Expected,
        [string]$Name
    )

    $recorded = Assert-RunPath (Get-RequiredString $Value $Name)
    $expectedPath = Assert-RunPath $Expected
    if (-not $recorded.Equals($expectedPath, [System.StringComparison]::OrdinalIgnoreCase)) {
        Fail-Manifest "$Name does not match the selected reuse working set"
    }
}

function Get-ReuseReceipt {
    param([pscustomobject]$Paths)

    $receiptPath = Assert-RunPath (Join-Path $Paths.run_root "e\result.json")
    if (-not [System.IO.File]::Exists($receiptPath)) {
        Fail-Manifest "selected reuse working set is missing e\\result.json: $receiptPath"
    }
    $receiptItem = Get-Item -LiteralPath $receiptPath -Force
    if (
        $receiptItem -isnot [System.IO.FileInfo] -or
        ($receiptItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        Fail-Manifest "selected reuse receipt must be a non-reparse regular file: $receiptPath"
    }
    try {
        $receipt = ConvertFrom-Json -InputObject ([System.IO.File]::ReadAllText($receiptPath, [System.Text.Encoding]::UTF8)) -AsHashtable -Depth 64 -NoEnumerate
    } catch {
        Fail-Manifest "selected reuse receipt is not valid JSON: $receiptPath"
    }
    $receipt = Get-RequiredMap $receipt "selected reuse receipt"
    Assert-ExactKeys $receipt @(
        "schema", "status", "exit_code", "cache_root", "paths", "resource_contract", "bootstrap",
        "candidate_materialization", "manifest_path", "plan_id", "validation_tooling_digest",
        "candidate_identity", "command_results", "error"
    ) "selected reuse receipt"
    if ((Get-RequiredPositiveInt $receipt["schema"] "selected reuse receipt.schema") -ne 2) {
        Fail-Manifest "selected reuse receipt.schema must equal 2"
    }
    $recordedPaths = Get-RequiredMap $receipt["paths"] "selected reuse receipt.paths"
    Assert-ExactKeys $recordedPaths @(
        "cache_root", "executor_root", "run_root", "evidence_dir", "temp_dir", "target_dir", "cargo_home",
        "rustup_home", "candidate_root", "helper_state", "tool_staging", "v8_cache"
    ) "selected reuse receipt.paths"
    foreach ($entry in @(
            @("cache_root", $script:CacheRoot),
            @("executor_root", $Paths.executor_root),
            @("run_root", $Paths.run_root),
            @("target_dir", $Paths.target_dir),
            @("cargo_home", $Paths.cargo_home),
            @("rustup_home", $Paths.rustup_home),
            @("candidate_root", $Paths.candidate_root),
            @("helper_state", $Paths.helper_state),
            @("tool_staging", $Paths.tool_staging),
            @("v8_cache", $Paths.v8_cache)
        )) {
        Assert-ReuseReceiptPath $recordedPaths[$entry[0]] $entry[1] "selected reuse receipt.paths.$($entry[0])"
    }
    $materialization = Get-RequiredMap $receipt["candidate_materialization"] "selected reuse receipt.candidate_materialization"
    if (
        (Get-RequiredString $materialization["status"] "selected reuse receipt.candidate_materialization.status") -cne "success" -or
        $materialization["materialized"] -isnot [bool] -or
        -not $materialization["materialized"]
    ) {
        Fail-Manifest "selected reuse receipt does not record a materialized candidate"
    }
    Assert-ReuseReceiptPath $materialization["candidate_root"] $Paths.candidate_root "selected reuse receipt.candidate_materialization.candidate_root"
    $bootstrap = Get-RequiredMap $receipt["bootstrap"] "selected reuse receipt.bootstrap"
    if ((Get-RequiredString $bootstrap["status"] "selected reuse receipt.bootstrap.status") -cne "success") {
        Fail-Manifest "selected reuse receipt does not record a successful bootstrap"
    }
    return [pscustomobject]@{
        path = $receiptPath
        value = $receipt
    }
}

function Get-TestOnlyPreflightFixture {
    # This is a process-only deterministic harness input. It is not manifest data,
    # cannot be selected by a production plan, and is rejected unless this exact
    # test-only opt-in is present in the Windows process environment.
    $fields = @(
        "CARGO_VALIDATE_WINDOWS_TEST_DISK_FREE_BYTES",
        "CARGO_VALIDATE_WINDOWS_TEST_AVAILABLE_MEMORY_BYTES",
        "CARGO_VALIDATE_WINDOWS_TEST_NATIVE_PROCESS_STATE",
        "CARGO_VALIDATE_WINDOWS_TEST_WSL_PROCESS_STATE",
        "CARGO_VALIDATE_WINDOWS_TEST_MUTEX_STATE"
    )
    $provided = @($fields | Where-Object { $null -ne [System.Environment]::GetEnvironmentVariable($_) })
    $optIn = [System.Environment]::GetEnvironmentVariable($script:TestOnlyPreflightFixtureOptIn)
    if ([string]::IsNullOrEmpty($optIn)) {
        if ($provided.Count -ne 0) {
            Fail-Manifest "test-only preflight fixture data requires the process opt-in"
        }
        return $null
    }
    if ($optIn -cne "1") {
        Fail-Manifest "test-only preflight fixture opt-in must equal 1"
    }
    foreach ($field in $fields) {
        if ($provided -notcontains $field) {
            Fail-Manifest "test-only preflight fixture is missing $field"
        }
    }

    $numbers = [ordered]@{}
    foreach ($field in @(
            "CARGO_VALIDATE_WINDOWS_TEST_DISK_FREE_BYTES",
            "CARGO_VALIDATE_WINDOWS_TEST_AVAILABLE_MEMORY_BYTES"
        )) {
        $value = [System.Environment]::GetEnvironmentVariable($field)
        if ($value -notmatch "^(0|[1-9][0-9]{0,19})$") {
            Fail-Manifest "$field must be an unsigned decimal integer"
        }
        try {
            $numbers[$field] = [uint64]$value
        } catch {
            Fail-Manifest "$field is outside the supported unsigned integer range"
        }
    }
    $nativeState = [System.Environment]::GetEnvironmentVariable("CARGO_VALIDATE_WINDOWS_TEST_NATIVE_PROCESS_STATE")
    $wslState = [System.Environment]::GetEnvironmentVariable("CARGO_VALIDATE_WINDOWS_TEST_WSL_PROCESS_STATE")
    $mutexState = [System.Environment]::GetEnvironmentVariable("CARGO_VALIDATE_WINDOWS_TEST_MUTEX_STATE")
    if ($nativeState -notin @("clear", "cargo", "cargo-nextest", "rustc", "rustdoc", "query-failure", "malformed")) {
        Fail-Manifest "test-only native process state is unsupported"
    }
    if ($wslState -notin @("clear", "cargo", "cargo-nextest", "rustc", "rustdoc", "query-failure", "malformed")) {
        Fail-Manifest "test-only WSL process state is unsupported"
    }
    if ($mutexState -notin @("clear", "busy", "abandoned")) {
        Fail-Manifest "test-only mutex state is unsupported"
    }
    return [pscustomobject]@{
        disk_free_bytes = $numbers["CARGO_VALIDATE_WINDOWS_TEST_DISK_FREE_BYTES"]
        available_memory_bytes = $numbers["CARGO_VALIDATE_WINDOWS_TEST_AVAILABLE_MEMORY_BYTES"]
        native_process_state = $nativeState
        wsl_process_state = $wslState
        mutex_state = $mutexState
    }
}

function Get-TestOnlyBootstrapFixture {
    # This process-only fixture supplies immutable WSL-side artifact inputs to
    # exercise the same F-drive copy, digest, and extraction path as production.
    # It is never manifest data and requires the inert fake-cargo opt-in, so it
    # cannot make a production command use test inputs.
    $fields = @(
        "CARGO_VALIDATE_WINDOWS_TEST_BOOTSTRAP_NEXTEST_ZIP",
        "CARGO_VALIDATE_WINDOWS_TEST_BOOTSTRAP_V8_ARCHIVE",
        "CARGO_VALIDATE_WINDOWS_TEST_BOOTSTRAP_V8_BINDING",
        "CARGO_VALIDATE_WINDOWS_TEST_BOOTSTRAP_FINAL_HOST"
    )
    $provided = @($fields | Where-Object { $null -ne [System.Environment]::GetEnvironmentVariable($_) })
    $optIn = [System.Environment]::GetEnvironmentVariable($script:TestOnlyBootstrapFixtureOptIn)
    if ([string]::IsNullOrEmpty($optIn)) {
        if ($provided.Count -ne 0) {
            Fail-Manifest "test-only bootstrap fixture data requires the process opt-in"
        }
        return $null
    }
    if ($optIn -cne "1") {
        Fail-Manifest "test-only bootstrap fixture opt-in must equal 1"
    }
    if ([System.Environment]::GetEnvironmentVariable($script:TestOnlyFakeCargoOptIn) -ne "1") {
        Fail-Manifest "test-only bootstrap fixtures require the test-only fake-cargo process opt-in"
    }
    foreach ($field in $fields) {
        if ($provided -notcontains $field) {
            Fail-Manifest "test-only bootstrap fixture is missing $field"
        }
    }

    $sources = [ordered]@{}
    foreach ($field in $fields[0..2]) {
        $sourcePath = Get-RequiredString ([System.Environment]::GetEnvironmentVariable($field)) $field
        if (-not [System.IO.Path]::IsPathFullyQualified($sourcePath)) {
            Fail-Manifest "$field must be an absolute read-only WSL fixture path"
        }
        $fullPath = [System.IO.Path]::GetFullPath($sourcePath)
        if (-not $fullPath.StartsWith("\\wsl.localhost\", [System.StringComparison]::OrdinalIgnoreCase)) {
            Fail-Manifest "$field must resolve through the WSL UNC read-only fixture path"
        }
        if (-not [System.IO.File]::Exists($fullPath)) {
            Fail-Manifest "$field does not name an existing fixture file"
        }
        $item = Get-Item -LiteralPath $fullPath -Force
        if (
            $item -isnot [System.IO.FileInfo] -or
            ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
            $item.Length -le 0
        ) {
            Fail-Manifest "$field must name a non-empty non-reparse fixture file"
        }
        $sources[$field] = $fullPath
    }

    $finalHost = Get-RequiredString ([System.Environment]::GetEnvironmentVariable($fields[3])) $fields[3]
    if ($finalHost -notmatch "^[A-Za-z0-9](?:[A-Za-z0-9.-]{0,251}[A-Za-z0-9])?$") {
        Fail-Manifest "$($fields[3]) must be a safe DNS host"
    }
    $fixtureUri = $null
    if (-not [System.Uri]::TryCreate("https://$finalHost/bootstrap-fixture", [System.UriKind]::Absolute, [ref]$fixtureUri)) {
        Fail-Manifest "$($fields[3]) must be a safe DNS host"
    }
    if ($fixtureUri.Scheme -cne "https" -or $fixtureUri.HostNameType -ne [System.UriHostNameType]::Dns) {
        Fail-Manifest "$($fields[3]) must be a safe DNS host"
    }
    return [pscustomobject]@{
        nextest_zip = $sources["CARGO_VALIDATE_WINDOWS_TEST_BOOTSTRAP_NEXTEST_ZIP"]
        v8_archive = $sources["CARGO_VALIDATE_WINDOWS_TEST_BOOTSTRAP_V8_ARCHIVE"]
        v8_binding = $sources["CARGO_VALIDATE_WINDOWS_TEST_BOOTSTRAP_V8_BINDING"]
        final_host = $fixtureUri.Host.ToLowerInvariant()
    }
}

function Enter-NativeExecutionMutex {
    param([pscustomobject]$TestFixture)

    if ($null -ne $TestFixture) {
        if ($TestFixture.mutex_state -eq "busy") {
            Fail-Manifest "native Windows execution mutex is busy"
        }
        if ($TestFixture.mutex_state -eq "abandoned") {
            Fail-Manifest "native Windows execution mutex was abandoned"
        }
    }
    $mutex = [System.Threading.Mutex]::new($false, $script:NativeMutexName)
    try {
        if (-not $mutex.WaitOne(0)) {
            $mutex.Dispose()
            Fail-Manifest "native Windows execution mutex is busy"
        }
    } catch [System.Threading.AbandonedMutexException] {
        $mutex.Dispose()
        Fail-Manifest "native Windows execution mutex was abandoned"
    }
    $script:NativeMutex = $mutex
    $script:NativeMutexHeld = $true
    return [ordered]@{
        name = $script:NativeMutexName
        status = "held"
        source = if ($null -eq $TestFixture) { "native" } else { "test-only-fixture" }
    }
}

function Exit-NativeExecutionMutex {
    if ($null -eq $script:NativeMutex) {
        return
    }
    try {
        if ($script:NativeMutexHeld) {
            $script:NativeMutex.ReleaseMutex()
        }
    } catch {
    } finally {
        $script:NativeMutex.Dispose()
        $script:NativeMutex = $null
        $script:NativeMutexHeld = $false
    }
}

function Get-WindowsDriveFreeBytes {
    param([pscustomobject]$TestFixture)

    if ($null -ne $TestFixture) {
        return [uint64]$TestFixture.disk_free_bytes
    }
    try {
        $drive = [System.IO.DriveInfo]::new("F:\")
        if (-not $drive.IsReady) {
            Fail-Manifest "F: is not ready for native Windows execution"
        }
        return [uint64]$drive.AvailableFreeSpace
    } catch {
        if ($_.Exception.Message -like "F: is not ready*") {
            throw
        }
        Fail-Manifest "native Windows F: free-byte query failed: $($_.Exception.Message)"
    }
}

function Get-WindowsAvailablePhysicalMemoryBytes {
    param([pscustomobject]$TestFixture)

    if ($null -ne $TestFixture) {
        return [uint64]$TestFixture.available_memory_bytes
    }
    try {
        $records = @(Get-CimInstance -ClassName Win32_OperatingSystem -ErrorAction Stop)
        if ($records.Count -ne 1) {
            Fail-Manifest "native Windows physical-memory query returned an unsupported record count"
        }
        $freeKib = $records[0].FreePhysicalMemory
        if ($null -eq $freeKib -or [string]$freeKib -notmatch "^[0-9]+$") {
            Fail-Manifest "native Windows physical-memory query returned an invalid available-memory value"
        }
        [uint64]$bytes = [uint64]$freeKib * [uint64]1024
        return $bytes
    } catch {
        if ($_.Exception.Message -like "native Windows physical-memory query*") {
            throw
        }
        Fail-Manifest "native Windows physical-memory query failed: $($_.Exception.Message)"
    }
}

function Get-NativeProcessRecords {
    param([pscustomobject]$TestFixture)

    if ($null -ne $TestFixture) {
        switch ($TestFixture.native_process_state) {
            "query-failure" { Fail-Manifest "test-only native Windows process query failed" }
            "malformed" { Fail-Manifest "test-only native Windows process query returned malformed data" }
            "clear" {
                return @([pscustomobject]@{
                        process_id = [uint32]0
                        name = "System Idle Process"
                        executable_path = $null
                        command_line = $null
                    })
            }
            default {
                return @([pscustomobject]@{
                        process_id = [uint32]9
                        name = "$($TestFixture.native_process_state).exe"
                        executable_path = "C:\\fixture\\$($TestFixture.native_process_state).exe"
                        command_line = "$($TestFixture.native_process_state) fixture"
                    })
            }
        }
    }

    try {
        $processes = @(Get-CimInstance -ClassName Win32_Process -ErrorAction Stop)
    } catch {
        Fail-Manifest "native Windows process query failed: $($_.Exception.Message)"
    }
    if ($processes.Count -eq 0) {
        Fail-Manifest "native Windows process query returned no records"
    }
    $records = [System.Collections.Generic.List[object]]::new()
    foreach ($process in $processes) {
        try {
            $processId = [uint32]$process.ProcessId
        } catch {
            Fail-Manifest "native Windows process query returned an invalid process id"
        }
        $name = $process.Name
        $executablePath = $process.ExecutablePath
        $commandLine = $process.CommandLine
        if ([string]::IsNullOrWhiteSpace([string]$name)) {
            Fail-Manifest "native Windows process query returned a process without a name"
        }
        if (
            ($null -ne $executablePath -and $executablePath -isnot [string]) -or
            ($null -ne $commandLine -and $commandLine -isnot [string])
        ) {
            Fail-Manifest "native Windows process query returned invalid process metadata"
        }
        $records.Add([pscustomobject]@{
                process_id = $processId
                name = [string]$name
                executable_path = $executablePath
                command_line = $commandLine
            })
    }
    return $records.ToArray()
}

function Test-CargoWriterName {
    param([string]$Name)

    $leaf = [System.IO.Path]::GetFileName($Name).ToLowerInvariant()
    return $leaf -in @("cargo", "cargo.exe", "cargo-nextest", "cargo-nextest.exe", "rustc", "rustc.exe", "rustdoc", "rustdoc.exe")
}

function Get-NativeWriterSummary {
    param([pscustomobject]$TestFixture)

    $records = @(Get-NativeProcessRecords $TestFixture)
    $writers = [System.Collections.Generic.List[object]]::new()
    foreach ($record in $records) {
        if (Test-CargoWriterName ([string]$record.name)) {
            $writers.Add([ordered]@{ process_id = $record.process_id; name = $record.name })
        }
    }
    if ($writers.Count -ne 0) {
        $writer = $writers[0]
        Fail-Manifest "visible native Windows cargo writer $($writer.name) (PID $($writer.process_id)) blocks execution"
    }
    return [ordered]@{
        source = if ($null -eq $TestFixture) { "native" } else { "test-only-fixture" }
        record_count = $records.Count
        writers = @()
    }
}

function Get-WslProcessText {
    param(
        [pscustomobject]$Runtime,
        [pscustomobject]$Paths,
        [pscustomobject]$TestFixture
    )

    if ($null -ne $TestFixture) {
        switch ($TestFixture.wsl_process_state) {
            "query-failure" { Fail-Manifest "test-only wsl.exe process query failed" }
            "malformed" { return "not-ps-output" }
            "clear" { return "1 0 init /init" }
            default { return "9 1 $($TestFixture.wsl_process_state) $($TestFixture.wsl_process_state) fixture" }
        }
    }
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = "wsl.exe"
    $startInfo.WorkingDirectory = $Paths.helper_state
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.CreateNoWindow = $true
    foreach ($argument in @(
            "--distribution",
            [string]$Runtime.source_materialization.wsl_distro_name,
            "--exec",
            "ps",
            "-eo",
            "pid=,ppid=,comm=,args="
        )) {
        $null = $startInfo.ArgumentList.Add($argument)
    }
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) {
            Fail-Manifest "wsl.exe process query did not start"
        }
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(30000)) {
            try { $process.Kill($true) } catch {}
            Fail-Manifest "wsl.exe process query timed out"
        }
        $stdout = $stdoutTask.GetAwaiter().GetResult()
        $stderr = $stderrTask.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) {
            Fail-Manifest "wsl.exe process query failed: $stderr"
        }
        return $stdout
    } finally {
        $process.Dispose()
    }
}

function Get-WslWriterSummary {
    param(
        [pscustomobject]$Runtime,
        [pscustomobject]$Paths,
        [pscustomobject]$TestFixture
    )

    $records = [System.Collections.Generic.List[object]]::new()
    $wslText = Get-WslProcessText $Runtime $Paths $TestFixture
    foreach ($line in ($wslText -split "`r?`n")) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        if ($line -notmatch "^\s*(?<pid>\d+)\s+(?<ppid>\d+)\s+(?<comm>\S+)(?:\s+(?<args>.*))?$") {
            Fail-Manifest "wsl.exe process query output is not parseable: $line"
        }
        $records.Add([pscustomobject]@{
                process_id = [uint32]$Matches["pid"]
                parent_process_id = [uint32]$Matches["ppid"]
                command = [string]$Matches["comm"]
                arguments = [string]$Matches["args"]
            })
    }
    if ($records.Count -eq 0) {
        Fail-Manifest "wsl.exe process query returned no records"
    }
    foreach ($record in $records) {
        if (Test-CargoWriterName ([string]$record.command)) {
            Fail-Manifest "visible WSL cargo writer $($record.command) (PID $($record.process_id)) blocks execution"
        }
    }
    return [ordered]@{
        source = if ($null -eq $TestFixture) { "native" } else { "test-only-fixture" }
        wsl_distro_name = [string]$Runtime.source_materialization.wsl_distro_name
        record_count = $records.Count
        writers = @()
    }
}

function Invoke-ExecutionPreflight {
    param(
        [pscustomobject]$Runtime,
        [pscustomobject]$Paths,
        [pscustomobject]$TestFixture,
        [string]$Stage,
        [bool]$Yolo
    )

    $check = [ordered]@{
        stage = $Stage
        source = if ($null -eq $TestFixture) { "native" } else { "test-only-fixture" }
        status = "started"
        free_disk_bytes = $null
        required_free_disk_bytes = $null
        available_memory_bytes = $null
        required_available_memory_bytes = $null
        yolo = $Yolo
        bypassed_free_disk_floor = $false
        bypassed_available_memory_floor = $false
        native_processes = $null
        wsl_processes = $null
        error = $null
    }
    $script:LivePreflightChecks.Add($check)
    try {
        [uint64]$gib = 1073741824
        [uint64]$requiredDisk = [uint64]$Runtime.minimum_free_disk_gib * $gib
        [uint64]$requiredMemory = [uint64]$Runtime.minimum_available_memory_gib * $gib
        [uint64]$freeDisk = Get-WindowsDriveFreeBytes $TestFixture
        [uint64]$availableMemory = Get-WindowsAvailablePhysicalMemoryBytes $TestFixture
        $check.free_disk_bytes = $freeDisk
        $check.required_free_disk_bytes = $requiredDisk
        $check.available_memory_bytes = $availableMemory
        $check.required_available_memory_bytes = $requiredMemory
        if ($freeDisk -lt $requiredDisk) {
            if ($Yolo) {
                $check.bypassed_free_disk_floor = $true
            } else {
                Fail-Manifest "F: free bytes $freeDisk are below the required $requiredDisk"
            }
        }
        if ($availableMemory -lt $requiredMemory) {
            if ($Yolo) {
                $check.bypassed_available_memory_floor = $true
            } else {
                Fail-Manifest "available Windows physical memory $availableMemory is below the required $requiredMemory"
            }
        }
        $check.native_processes = Get-NativeWriterSummary $TestFixture
        $check.wsl_processes = Get-WslWriterSummary $Runtime $Paths $TestFixture
        $check.status = "passed"
        return [pscustomobject]$check
    } catch {
        $check.status = "failed"
        $check.error = $_.Exception.Message
        throw
    }
}

function Set-RunEnvironment {
    param([pscustomobject]$Paths)

    $env:TEMP = $Paths.temp_dir
    $env:TMP = $Paths.temp_dir
    $env:CARGO_TARGET_DIR = $Paths.target_dir
    $env:CARGO_HOME = $Paths.cargo_home
    $env:RUSTUP_HOME = $Paths.rustup_home
    $env:HOME = $Paths.helper_state
    $env:USERPROFILE = $Paths.helper_state
    $env:APPDATA = $Paths.helper_state
    $env:LOCALAPPDATA = $Paths.helper_state
    foreach ($name in @("RUSTY_V8_ARCHIVE", "RUSTY_V8_MIRROR", "RUSTY_V8_SRC_BINDING_PATH", "V8_FROM_SOURCE")) {
        Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
    }
}

function Write-JsonEvidence {
    param(
        [string]$Path,
        [object]$Value
    )

    $content = ($Value | ConvertTo-Json -Depth 64) + [Environment]::NewLine
    [System.IO.File]::WriteAllText(
        (Assert-RunPath $Path),
        $content,
        [System.Text.UTF8Encoding]::new($false)
    )
}

function Write-TextEvidence {
    param(
        [string]$Path,
        [string]$Value
    )

    [System.IO.File]::WriteAllText(
        (Assert-RunPath $Path),
        $Value,
        [System.Text.UTF8Encoding]::new($false)
    )
}

function Write-BytesEvidence {
    param(
        [string]$Path,
        [byte[]]$Bytes
    )

    [System.IO.File]::WriteAllBytes((Assert-RunPath $Path), $Bytes)
}

function Get-ByteDigest {
    param([byte[]]$Bytes)

    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return ([System.BitConverter]::ToString($sha.ComputeHash($Bytes))).Replace("-", "").ToLowerInvariant()
    } finally {
        $sha.Dispose()
    }
}

function Test-BytesEqual {
    param(
        [byte[]]$Left,
        [byte[]]$Right
    )

    if ($Left.Length -ne $Right.Length) {
        return $false
    }
    for ($index = 0; $index -lt $Left.Length; $index++) {
        if ($Left[$index] -ne $Right[$index]) {
            return $false
        }
    }
    return $true
}

function New-ByteRecord {
    param(
        [string]$Path,
        [byte[]]$Bytes
    )

    return [pscustomobject]@{
        path = $Path
        byte_length = $Bytes.Length
        sha256 = Get-ByteDigest $Bytes
        bytes = $Bytes
    }
}

function Get-TextFromBytes {
    param([byte[]]$Bytes)

    return ([System.Text.UTF8Encoding]::new($false).GetString($Bytes)).TrimEnd([char[]]@(13, 10))
}

function Assert-GitObjectId {
    param(
        [object]$Value,
        [string]$Name
    )

    $text = Get-RequiredString $Value $Name
    if ($text -notmatch "^[0-9a-fA-F]{40}([0-9a-fA-F]{24})?$") {
        Fail-Manifest "$Name must be a 40- or 64-character Git object id"
    }
    return $text
}

function Get-CandidateIdentity {
    param([System.Collections.IDictionary]$ManifestData)

    $candidate = Get-RequiredMap (Get-MapValue $ManifestData "candidate_identity").value "candidate_identity"
    Assert-ExactKeys $candidate @("head", "merge_head", "index_tree") "candidate_identity"
    $head = $candidate["head"]
    $indexTree = $candidate["index_tree"]
    $mergeHead = $candidate["merge_head"]

    if ($null -eq $head -or $null -eq $indexTree) {
        if ($null -ne $head -or $null -ne $indexTree -or $null -ne $mergeHead) {
            Fail-Manifest "candidate_identity must use all-null values or bound head/index_tree values"
        }
        return [pscustomobject]@{
            head = $null
            merge_head = $null
            index_tree = $null
            is_bound = $false
        }
    }

    return [pscustomobject]@{
        head = Assert-GitObjectId $head "candidate_identity.head"
        merge_head = if ($null -eq $mergeHead) { $null } else { Assert-GitObjectId $mergeHead "candidate_identity.merge_head" }
        index_tree = Assert-GitObjectId $indexTree "candidate_identity.index_tree"
        is_bound = $true
    }
}

function Assert-ManifestRoot {
    param([System.Collections.IDictionary]$ManifestData)

    $expected = @(
        "action", "stage", "mode", "changed_files", "selected_packages", "selected_surfaces", "flags", "warnings",
        "commands", "manual", "receipt_dir", "telemetry_level", "candidate_identity", "windows_runtime",
        "plan_id", "validation_tooling_digest"
    )
    Assert-ExactKeys $ManifestData $expected "manifest"
    $null = Get-RequiredString $ManifestData["action"] "manifest.action"
    $null = Get-RequiredString $ManifestData["stage"] "manifest.stage"
    $mode = Get-RequiredString $ManifestData["mode"] "manifest.mode"
    if ($mode -notin @("quick", "standard", "strict", "full")) {
        Fail-Manifest "manifest.mode is unsupported"
    }
    foreach ($field in @("changed_files", "selected_packages", "selected_surfaces", "flags", "warnings")) {
        $null = Assert-StringArray $ManifestData[$field] "manifest.$field"
    }
    $manual = (Get-RequiredArray $ManifestData["manual"] "manifest.manual").items
    foreach ($item in $manual) {
        $entry = Get-RequiredMap $item "manifest.manual entry"
        Assert-ExactKeys $entry @("message", "reason", "kind") "manifest.manual entry"
        $null = Get-RequiredString $entry["message"] "manifest.manual.message"
        $null = Get-RequiredString $entry["reason"] "manifest.manual.reason"
        $null = Get-RequiredString $entry["kind"] "manifest.manual.kind"
    }
    if ($null -ne $ManifestData["receipt_dir"]) {
        $null = Get-RequiredString $ManifestData["receipt_dir"] "manifest.receipt_dir"
    }
    $telemetry = Get-RequiredString $ManifestData["telemetry_level"] "manifest.telemetry_level"
    if ($telemetry -notin @("off", "summary", "full", "debug")) {
        Fail-Manifest "manifest.telemetry_level is unsupported"
    }
    $null = Get-RequiredSha256 $ManifestData["plan_id"] "plan_id"
    $null = Get-RequiredSha256 $ManifestData["validation_tooling_digest"] "validation_tooling_digest"
}

function Assert-ManifestCommandShape {
    param([System.Collections.IDictionary]$Command)

    $required = @(
        "argv", "reason", "kind", "env", "platform", "executor", "classification", "resource_profile", "artifact_policy", "command_id"
    )
    $allowed = @(
        "argv", "reason", "kind", "env", "platform", "executor", "classification", "resource_profile", "artifact_policy", "command_id",
        "codex_v8_target", "fingerprint", "job_contract_digest", "fallback_expected_growth_gib", "effective_expected_growth_gib", "expected_growth_source"
    )
    Assert-RequiredAndAllowedKeys $Command $required $allowed "command"
    $null = Assert-StringArray $Command["argv"] "command.argv"
    $null = Get-RequiredString $Command["reason"] "command.reason"
    $null = Get-RequiredString $Command["kind"] "command.kind"
    $environment = Get-RequiredMap $Command["env"] "command.env"
    Assert-StringMap $environment "command.env"
    if ($null -ne $Command["platform"]) {
        $null = Get-RequiredString $Command["platform"] "command.platform"
    }
    if ($null -ne $Command["executor"]) {
        $null = Get-RequiredString $Command["executor"] "command.executor"
    }
    $null = Get-RequiredString $Command["classification"] "command.classification"
    if ($null -ne $Command["resource_profile"]) {
        $null = Get-RequiredString $Command["resource_profile"] "command.resource_profile"
    }
    $null = Get-RequiredString $Command["artifact_policy"] "command.artifact_policy"
    $null = Get-RequiredSha256 $Command["command_id"] "command.command_id"
    foreach ($field in @("fingerprint", "job_contract_digest")) {
        if ($Command.Contains($field)) {
            $null = Get-RequiredSha256 $Command[$field] "command.$field"
        }
    }
    foreach ($field in @("codex_v8_target", "expected_growth_source")) {
        if ($Command.Contains($field)) {
            $null = Get-RequiredString $Command[$field] "command.$field"
        }
    }
    foreach ($field in @("fallback_expected_growth_gib", "effective_expected_growth_gib")) {
        if ($Command.Contains($field)) {
            $null = Get-RequiredPositiveInt $Command[$field] "command.$field" -AllowZero
        }
    }
}

function Assert-ExclusionRecord {
    param(
        [System.Collections.IDictionary]$Command,
        [string]$Classification
    )

    $argv = (Get-RequiredArray $Command["argv"] "excluded command.argv").items
    $environment = Get-RequiredMap $Command["env"] "excluded command.env"
    if (
        $argv.Count -ne 0 -or
        $environment.Count -ne 0 -or
        $null -ne $Command["platform"] -or
        $null -ne $Command["executor"] -or
        $null -ne $Command["resource_profile"] -or
        (Get-RequiredString $Command["artifact_policy"] "excluded command.artifact_policy") -ne "none"
    ) {
        Fail-Manifest "$Classification entries must use the accepted null/empty exclusion shape"
    }
}

function Get-TestFixture {
    param([System.Collections.IDictionary]$Environment)

    $keys = @($Environment.Keys | ForEach-Object { [string]$_ })
    if ($keys.Count -eq 0) {
        return $null
    }
    if (
        $keys.Count -ne 2 -or
        $keys -notcontains "CARGO_VALIDATE_WINDOWS_TEST_FIXTURE" -or
        $keys -notcontains "CARGO_VALIDATE_WINDOWS_TEST_EXIT_CODE"
    ) {
        Fail-Manifest "Windows command env must be empty outside the exact inert fake-cargo fixture"
    }
    if ((Get-RequiredString $Environment["CARGO_VALIDATE_WINDOWS_TEST_FIXTURE"] "fixture name") -ne "fake-cargo-v1") {
        Fail-Manifest "unknown Windows test fixture"
    }
    $exitCodeText = Get-RequiredString $Environment["CARGO_VALIDATE_WINDOWS_TEST_EXIT_CODE"] "fixture exit code"
    if ($exitCodeText -notmatch "^(0|[1-9][0-9]{0,2})$" -or [int]$exitCodeText -gt 255) {
        Fail-Manifest "fixture exit code must be an integer from 0 through 255"
    }
    if ([System.Environment]::GetEnvironmentVariable($script:TestOnlyFakeCargoOptIn) -ne "1") {
        Fail-Manifest "the inert fake-cargo fixture requires the test-only process opt-in"
    }
    return [pscustomobject]@{ exit_code = [int]$exitCodeText }
}

function Test-ManifestCommand {
    param(
        [System.Collections.IDictionary]$Command,
        [int]$Index,
        [pscustomobject]$Runtime
    )

    Assert-ManifestCommandShape $Command
    $classification = Get-RequiredString $Command["classification"] "command.classification"
    if ($classification -notin @("command", "build-like", "platform-neutral-test", "wsl-unix-test", "windows-only-excluded", "macos-not-applicable")) {
        Fail-Manifest "command.classification is unsupported"
    }
    $reason = Get-RequiredString $Command["reason"] "command.reason"
    $commandId = Get-RequiredSha256 $Command["command_id"] "command.command_id"
    $kind = Get-RequiredString $Command["kind"] "command.kind"

    if ($classification -in @("windows-only-excluded", "macos-not-applicable")) {
        Assert-ExclusionRecord $Command $classification
        return [pscustomobject]@{
            index = $Index
            command_id = $commandId
            kind = $kind
            reason = $reason
            disposition = "excluded"
            classification = $classification
        }
    }

    $platform = Get-RequiredString $Command["platform"] "command.platform"
    if ($platform -eq "wsl") {
        $executor = Get-RequiredString $Command["executor"] "command.executor"
        if ($executor -notin @("cargo-guard", "command")) {
            Fail-Manifest "WSL records must use cargo-guard or command executors"
        }
        return [pscustomobject]@{
            index = $Index
            command_id = $commandId
            kind = $kind
            reason = $reason
            disposition = "wsl-not-executed"
            classification = $classification
        }
    }
    if ($platform -ne "windows") {
        Fail-Manifest "command.platform must be wsl or windows"
    }

    $artifactPolicy = Get-RequiredString $Command["artifact_policy"] "command.artifact_policy"
    if (
        (Get-RequiredString $Command["executor"] "command.executor") -ne "powershell" -or
        $classification -ne "platform-neutral-test" -or
        (Get-RequiredString $Command["resource_profile"] "command.resource_profile") -ne $Runtime.resource_contract.resource_profile -or
        $artifactPolicy -notin @("none", "ephemeral-codex-exe")
    ) {
        Fail-Manifest "Windows commands must be PowerShell platform-neutral windows_nextest test records"
    }

    $fingerprint = Get-RequiredSha256 (Get-MapValue $Command "fingerprint").value "command.fingerprint"
    $jobContractDigest = Get-RequiredSha256 (Get-MapValue $Command "job_contract_digest").value "command.job_contract_digest"
    $environment = Get-RequiredMap $Command["env"] "command.env"
    $argv = @(Assert-StringArray $Command["argv"] "command.argv")
    if (
        $argv.Count -lt 4 -or
        $argv[0] -cne "cargo" -or
        $argv[1] -cne "nextest" -or
        $argv[2] -cne "run" -or
        $argv[3] -cne "--workspace"
    ) {
        Fail-Manifest "Windows commands must use the cargo nextest run --workspace test route"
    }
    $fixture = Get-TestFixture $environment

    return [pscustomobject]@{
        index = $Index
        command_id = $commandId
        kind = $kind
        reason = $reason
        disposition = "approved"
        argv = $argv
        fingerprint = $fingerprint
        job_contract_digest = $jobContractDigest
        artifact_policy = $artifactPolicy
        fixture = $fixture
    }
}

function Get-FileSha256 {
    param([string]$Path)

    $stream = [System.IO.File]::OpenRead($Path)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return ([System.BitConverter]::ToString($sha.ComputeHash($stream))).Replace("-", "").ToLowerInvariant()
    } finally {
        $sha.Dispose()
        $stream.Dispose()
    }
}

function Assert-AbsoluteCDriveRegularFile {
    param(
        [string]$Path,
        [string]$Name
    )

    $fullPath = [System.IO.Path]::GetFullPath($Path)
    if (
        -not [System.IO.Path]::IsPathFullyQualified($fullPath) -or
        -not $fullPath.StartsWith("C:\", [System.StringComparison]::OrdinalIgnoreCase) -or
        -not [System.IO.File]::Exists($fullPath)
    ) {
        Fail-Manifest "$Name must be an existing absolute C-drive file: $fullPath"
    }
    $item = Get-Item -LiteralPath $fullPath -Force
    if (
        $item -isnot [System.IO.FileInfo] -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        Fail-Manifest "$Name must be a non-reparse C-drive regular file: $fullPath"
    }
    return [pscustomobject]@{
        path = $fullPath
        byte_length = $item.Length
        sha256 = Get-FileSha256 $fullPath
    }
}

function Capture-DirectRustToolchain {
    param([pscustomobject]$Runtime)

    $userProfile = [System.Environment]::GetEnvironmentVariable("USERPROFILE", "Process")
    if ([string]::IsNullOrWhiteSpace($userProfile)) {
        $userProfile = [System.Environment]::GetFolderPath([System.Environment+SpecialFolder]::UserProfile)
    }
    if ([string]::IsNullOrWhiteSpace($userProfile)) {
        Fail-Manifest "the installed Windows user profile is unavailable before run environment isolation"
    }
    $userProfile = [System.IO.Path]::GetFullPath($userProfile)
    if (
        -not [System.IO.Path]::IsPathFullyQualified($userProfile) -or
        -not $userProfile.StartsWith("C:\", [System.StringComparison]::OrdinalIgnoreCase) -or
        -not [System.IO.Directory]::Exists($userProfile)
    ) {
        Fail-Manifest "the installed Windows user profile must resolve under C: before run environment isolation"
    }

    $rustupSource = [System.Environment]::GetEnvironmentVariable("RUSTUP_HOME", "Process")
    if ([string]::IsNullOrWhiteSpace($rustupSource)) {
        $rustupSource = Join-Path $userProfile ".rustup"
    }
    $rustupSource = [System.IO.Path]::GetFullPath($rustupSource)
    if (
        -not [System.IO.Path]::IsPathFullyQualified($rustupSource) -or
        -not $rustupSource.StartsWith("C:\", [System.StringComparison]::OrdinalIgnoreCase) -or
        -not [System.IO.Directory]::Exists($rustupSource)
    ) {
        Fail-Manifest "the installed Rustup source must resolve under C: before run environment isolation"
    }

    $toolchainBin = [System.IO.Path]::GetFullPath(
        (Join-Path $rustupSource "toolchains\$($Runtime.rust_toolchain)\bin")
    )
    if (-not [System.IO.Directory]::Exists($toolchainBin)) {
        Fail-Manifest "the manifest Rust toolchain bin is unavailable: $toolchainBin"
    }
    return [pscustomobject]@{
        source_user_profile = $userProfile
        source_rustup_home = $rustupSource
        toolchain_bin = $toolchainBin
        cargo = Assert-AbsoluteCDriveRegularFile (Join-Path $toolchainBin "cargo.exe") "direct toolchain cargo.exe"
        rustc = Assert-AbsoluteCDriveRegularFile (Join-Path $toolchainBin "rustc.exe") "direct toolchain rustc.exe"
        rustdoc = Assert-AbsoluteCDriveRegularFile (Join-Path $toolchainBin "rustdoc.exe") "direct toolchain rustdoc.exe"
    }
}

function Prepend-PathEntries {
    param([string[]]$Entries)

    $prefix = @($Entries | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }) -join ";"
    if ([string]::IsNullOrEmpty($prefix)) {
        return
    }
    $existing = [System.Environment]::GetEnvironmentVariable("PATH", "Process")
    $env:PATH = if ([string]::IsNullOrEmpty($existing)) { $prefix } else { "$prefix;$existing" }
}

function Get-SanitizedHttpsUri {
    param([System.Uri]$Uri)

    $builder = [System.UriBuilder]::new($Uri)
    $builder.Query = ""
    $builder.Fragment = ""
    return $builder.Uri.AbsoluteUri
}

function Assert-PinnedArtifactUris {
    param(
        [System.Uri]$RequestedUri,
        [System.Uri]$FinalUri,
        [string]$Name
    )

    if (
        $RequestedUri.Scheme -cne "https" -or
        $RequestedUri.Host -cne "github.com"
    ) {
        Fail-Manifest "$Name requested URI must use HTTPS github.com"
    }
    if (
        $FinalUri.Scheme -cne "https" -or
        $FinalUri.Host.ToLowerInvariant() -notin @("github.com", "release-assets.githubusercontent.com")
    ) {
        Fail-Manifest "$Name final URI must use HTTPS github.com or release-assets.githubusercontent.com"
    }
}

function Copy-InputStreamToPartial {
    param(
        [System.IO.Stream]$InputStream,
        [string]$PartialPath,
        [string]$Name
    )

    $resolvedPartial = Assert-RunPath $PartialPath
    if ([System.IO.File]::Exists($resolvedPartial)) {
        Fail-Manifest "$Name partial destination already exists: $resolvedPartial"
    }
    $output = [System.IO.File]::Open(
        $resolvedPartial,
        [System.IO.FileMode]::CreateNew,
        [System.IO.FileAccess]::Write,
        [System.IO.FileShare]::None
    )
    try {
        $InputStream.CopyTo($output)
    } finally {
        $output.Dispose()
    }
    $item = Get-Item -LiteralPath $resolvedPartial -Force
    if (
        $item -isnot [System.IO.FileInfo] -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -le 0
    ) {
        Fail-Manifest "$Name partial download must be a non-empty non-reparse regular file"
    }
    return [int64]$item.Length
}

function Acquire-PinnedArtifact {
    param(
        [string]$RequestedUrl,
        [string]$ExpectedSha256,
        [string]$FinalPath,
        [pscustomobject]$Fixture,
        [string]$FixtureProperty,
        [string]$Name
    )

    $requestedUri = $null
    if (-not [System.Uri]::TryCreate($RequestedUrl, [System.UriKind]::Absolute, [ref]$requestedUri)) {
        Fail-Manifest "$Name requested URI is invalid"
    }
    $resolvedFinal = Assert-RunPath $FinalPath
    $partialPath = Assert-RunPath "$resolvedFinal.partial"
    if ([System.IO.File]::Exists($resolvedFinal)) {
        Fail-Manifest "$Name final destination already exists: $resolvedFinal"
    }

    $finalUri = $null
    $statusCode = $null
    $source = "native-https"
    if ($null -ne $Fixture) {
        $sourcePath = [string]$Fixture.$FixtureProperty
        $finalUri = [System.Uri]::new("https://$($Fixture.final_host)/bootstrap-fixture/$FixtureProperty")
        Assert-PinnedArtifactUris $requestedUri $finalUri $Name
        $input = [System.IO.File]::OpenRead($sourcePath)
        try {
            $null = Copy-InputStreamToPartial $input $partialPath $Name
        } finally {
            $input.Dispose()
        }
        $source = "test-only-fixture"
    } else {
        $handler = [System.Net.Http.HttpClientHandler]::new()
        $handler.AllowAutoRedirect = $true
        $handler.MaxAutomaticRedirections = 5
        $client = [System.Net.Http.HttpClient]::new($handler)
        $client.Timeout = [TimeSpan]::FromSeconds(120)
        $response = $null
        try {
            $response = $client.GetAsync(
                $requestedUri,
                [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead
            ).GetAwaiter().GetResult()
            if (-not $response.IsSuccessStatusCode) {
                Fail-Manifest "$Name download returned HTTP $([int]$response.StatusCode)"
            }
            $finalUri = $response.RequestMessage.RequestUri
            if ($null -eq $finalUri) {
                Fail-Manifest "$Name download did not report a final URI"
            }
            Assert-PinnedArtifactUris $requestedUri $finalUri $Name
            $input = $response.Content.ReadAsStream()
            try {
                $null = Copy-InputStreamToPartial $input $partialPath $Name
            } finally {
                $input.Dispose()
            }
            $statusCode = [int]$response.StatusCode
        } finally {
            if ($null -ne $response) {
                $response.Dispose()
            }
            $client.Dispose()
            $handler.Dispose()
        }
    }

    $digest = Get-FileSha256 $partialPath
    if ($digest -cne $ExpectedSha256) {
        Fail-Manifest "$Name partial download SHA-256 does not match the frozen manifest"
    }
    [System.IO.File]::Move($partialPath, $resolvedFinal)
    $item = Get-Item -LiteralPath $resolvedFinal -Force
    if (
        $item -isnot [System.IO.FileInfo] -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -le 0
    ) {
        Fail-Manifest "$Name final artifact must be a non-empty non-reparse regular file"
    }
    return [pscustomobject]@{
        source = $source
        requested_uri = $requestedUri.AbsoluteUri
        final_uri = Get-SanitizedHttpsUri $finalUri
        status_code = $statusCode
        path = $resolvedFinal
        byte_length = $item.Length
        sha256 = Get-FileSha256 $resolvedFinal
    }
}

function Get-SafeZipEntry {
    param(
        [System.IO.Compression.ZipArchiveEntry]$Entry,
        [System.Collections.Generic.HashSet[string]]$SeenNames
    )

    $fullName = [string]$Entry.FullName
    if (
        [string]::IsNullOrEmpty($fullName) -or
        $fullName.Length -gt 1024 -or
        $fullName.Contains("\") -or
        $fullName.StartsWith("/", [System.StringComparison]::Ordinal) -or
        $fullName -match "^[A-Za-z]:"
    ) {
        Fail-Manifest "Nextest ZIP contains an unsafe entry name: $fullName"
    }
    $isDirectory = $fullName.EndsWith("/", [System.StringComparison]::Ordinal)
    $pathText = if ($isDirectory) { $fullName.Substring(0, $fullName.Length - 1) } else { $fullName }
    if ([string]::IsNullOrEmpty($pathText)) {
        Fail-Manifest "Nextest ZIP contains an empty entry name"
    }
    $segments = $pathText.Split("/", [System.StringSplitOptions]::None)
    foreach ($segment in $segments) {
        if (
            [string]::IsNullOrEmpty($segment) -or
            $segment -in @(".", "..") -or
            $segment.EndsWith(".", [System.StringComparison]::Ordinal) -or
            $segment.EndsWith(" ", [System.StringComparison]::Ordinal) -or
            $segment.IndexOfAny([char[]]@(':', '*', '?', '"', '<', '>', '|')) -ge 0
        ) {
            Fail-Manifest "Nextest ZIP contains an unsafe entry name: $fullName"
        }
    }
    $canonicalName = $segments -join "\"
    if (-not $SeenNames.Add($canonicalName)) {
        Fail-Manifest "Nextest ZIP contains duplicate Windows-equivalent entry names: $fullName"
    }
    [uint32]$attributes = [System.BitConverter]::ToUInt32(
        [System.BitConverter]::GetBytes([int]$Entry.ExternalAttributes),
        0
    )
    if (($attributes -band [uint32]0x00000400) -ne 0) {
        Fail-Manifest "Nextest ZIP contains a reparse-point entry: $fullName"
    }
    [uint32]$unixType = ($attributes -shr 16) -band [uint32]0x0000F000
    if ($unixType -eq [uint32]0x0000A000) {
        Fail-Manifest "Nextest ZIP contains a symbolic-link entry: $fullName"
    }
    return [pscustomobject]@{
        entry = $Entry
        full_name = $fullName
        segments = $segments
        is_directory = $isDirectory
    }
}

function Extract-NextestExecutable {
    param(
        [pscustomobject]$ZipArtifact,
        [pscustomobject]$Paths
    )

    $destinationDirectory = Assert-RunPath (Join-Path $Paths.tool_staging "nextest")
    $null = [System.IO.Directory]::CreateDirectory($destinationDirectory)
    $destination = Assert-RunPath (Join-Path $destinationDirectory "cargo-nextest.exe")
    $partialPath = Assert-RunPath "$destination.partial"
    if ([System.IO.File]::Exists($destination) -or [System.IO.File]::Exists($partialPath)) {
        Fail-Manifest "Nextest executable destination already exists"
    }

    $zipStream = [System.IO.File]::OpenRead($ZipArtifact.path)
    $archive = $null
    try {
        $archive = [System.IO.Compression.ZipArchive]::new(
            $zipStream,
            [System.IO.Compression.ZipArchiveMode]::Read,
            $false
        )
        $seenNames = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
        $matches = [System.Collections.Generic.List[object]]::new()
        foreach ($entry in $archive.Entries) {
            $safeEntry = Get-SafeZipEntry $entry $seenNames
            if (
                -not $safeEntry.is_directory -and
                $safeEntry.segments[$safeEntry.segments.Length - 1].Equals("cargo-nextest.exe", [System.StringComparison]::OrdinalIgnoreCase)
            ) {
                $matches.Add($safeEntry)
            }
        }
        if ($matches.Count -ne 1) {
            Fail-Manifest "Nextest ZIP must contain exactly one cargo-nextest.exe entry"
        }
        $entry = $matches[0].entry
        if ($entry.Length -le 0) {
            Fail-Manifest "Nextest ZIP cargo-nextest.exe entry must be non-empty"
        }
        $input = $entry.Open()
        try {
            $null = Copy-InputStreamToPartial $input $partialPath "Nextest executable"
        } finally {
            $input.Dispose()
        }
    } finally {
        if ($null -ne $archive) {
            $archive.Dispose()
        }
        $zipStream.Dispose()
    }

    [System.IO.File]::Move($partialPath, $destination)
    $item = Get-Item -LiteralPath $destination -Force
    if (
        $item -isnot [System.IO.FileInfo] -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -le 0
    ) {
        Fail-Manifest "extracted cargo-nextest.exe must be a non-empty non-reparse regular file"
    }
    return [pscustomobject]@{
        zip_path = $ZipArtifact.path
        zip_sha256 = $ZipArtifact.sha256
        executable_path = $destination
        executable_directory = $destinationDirectory
        entry_name = $matches[0].full_name
        byte_length = $item.Length
        sha256 = Get-FileSha256 $destination
    }
}

function Import-MsvcEnvironmentFile {
    param(
        [string]$EnvironmentPath,
        [pscustomobject]$Runtime,
        [pscustomobject]$DirectToolchain
    )

    $targetName = "CARGO_TARGET_$($Runtime.target.ToUpperInvariant().Replace('-', '_'))_LINKER"
    $expectedNames = @(
        "INCLUDE", "LIB", "LIBPATH", "PATH", "UCRTVersion", "UniversalCRTSdkDir", "VCINSTALLDIR",
        "VCToolsInstallDir", "WindowsLibPath", "WindowsSdkBinPath", "WindowsSdkDir", "WindowsSDKLibVersion",
        "WindowsSDKVersion", $targetName
    )
    if (-not [System.IO.File]::Exists($EnvironmentPath)) {
        Fail-Manifest "MSVC setup did not create GITHUB_ENV"
    }
    $records = [ordered]@{}
    $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($line in [System.IO.File]::ReadAllLines($EnvironmentPath, [System.Text.UTF8Encoding]::new($false))) {
        $separator = $line.IndexOf("=", [System.StringComparison]::Ordinal)
        if ($separator -le 0) {
            Fail-Manifest "MSVC GITHUB_ENV contains a malformed record"
        }
        $name = $line.Substring(0, $separator)
        $value = $line.Substring($separator + 1)
        if (
            $expectedNames -cnotcontains $name -or
            -not $seen.Add($name) -or
            [string]::IsNullOrEmpty($value) -or
            $value.IndexOf([char]0) -ge 0
        ) {
            Fail-Manifest "MSVC GITHUB_ENV contains an unsupported, duplicate, or empty record: $name"
        }
        $records[$name] = $value
    }
    foreach ($name in $expectedNames) {
        if (-not $seen.Contains($name)) {
            Fail-Manifest "MSVC GITHUB_ENV is missing required record: $name"
        }
    }
    foreach ($name in $expectedNames) {
        if ($name -cne "PATH") {
            Set-Item -LiteralPath "Env:$name" -Value ([string]$records[$name])
        }
    }
    $env:PATH = [string]$records["PATH"]
    return [pscustomobject]@{
        path = $EnvironmentPath
        sha256 = Get-FileSha256 $EnvironmentPath
        record_names = $expectedNames
        linker_variable = $targetName
        linker_path = [string]$records[$targetName]
    }
}

function Invoke-CanonicalMsvcSetup {
    param(
        [pscustomobject]$Runtime,
        [pscustomobject]$Paths,
        [pscustomobject]$Materialization,
        [pscustomobject]$DirectToolchain
    )

    if ($null -eq $Materialization -or -not $Materialization.materialized) {
        Fail-Manifest "pinned bootstrap requires a materialized candidate for MSVC setup"
    }
    $setupPath = Assert-RunPath (Join-Path $Paths.candidate_root ".github\actions\setup-msvc-env\setup-msvc-env.ps1")
    if (-not [System.IO.File]::Exists($setupPath)) {
        Fail-Manifest "candidate MSVC setup script is missing: $setupPath"
    }
    $setupItem = Get-Item -LiteralPath $setupPath -Force
    if (
        $setupItem -isnot [System.IO.FileInfo] -or
        ($setupItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        Fail-Manifest "candidate MSVC setup script must be a non-reparse regular file"
    }
    $environmentPath = Assert-RunPath (Join-Path $Paths.helper_state "msvc.github-env")
    Write-TextEvidence $environmentPath ""
    $runnerTemp = Assert-RunPath (Join-Path $Paths.temp_dir "msvc-runner-temp")
    $null = [System.IO.Directory]::CreateDirectory($runnerTemp)
    $powershellPath = [System.IO.Path]::GetFullPath((Join-Path $PSHOME "pwsh.exe"))
    if (-not [System.IO.File]::Exists($powershellPath)) {
        Fail-Manifest "the current PowerShell executable is unavailable for MSVC setup"
    }
    Prepend-PathEntries @($DirectToolchain.toolchain_bin)
    $setupEnvironment = [ordered]@{
        PATH = $env:PATH
        GITHUB_ENV = $environmentPath
        RUNNER_TEMP = $runnerTemp
        TEMP = $Paths.temp_dir
        TMP = $Paths.temp_dir
        HOME = $Paths.helper_state
        USERPROFILE = $Paths.helper_state
        APPDATA = $Paths.helper_state
        LOCALAPPDATA = $Paths.helper_state
    }
    $setupResult = Invoke-DirectProcess $powershellPath @(
        "-NoLogo", "-NoProfile", "-NonInteractive", "-File", $setupPath, "-Target", $Runtime.target
    ) $Paths.candidate_root $setupEnvironment "canonical MSVC setup"
    $stdoutPath = Assert-RunPath (Join-Path $Paths.evidence_dir "msvc-setup.stdout.bin")
    $stderrPath = Assert-RunPath (Join-Path $Paths.evidence_dir "msvc-setup.stderr.txt")
    Write-BytesEvidence $stdoutPath $setupResult.stdout_bytes
    Write-TextEvidence $stderrPath $setupResult.stderr
    if ($setupResult.exit_code -ne 0) {
        Fail-Manifest "canonical MSVC setup failed with exit code $($setupResult.exit_code): $($setupResult.stderr.Trim())"
    }
    $environment = Import-MsvcEnvironmentFile $environmentPath $Runtime $DirectToolchain
    return [pscustomobject]@{
        status = "success"
        script_path = $setupPath
        script_sha256 = Get-FileSha256 $setupPath
        stdout_path = $stdoutPath
        stderr_path = $stderrPath
        environment = $environment
    }
}

function Initialize-NativeWindowsBootstrap {
    param(
        [pscustomobject]$Runtime,
        [pscustomobject]$Paths,
        [pscustomobject]$Materialization,
        [pscustomobject]$DirectToolchain,
        [pscustomobject]$Fixture,
        [pscustomobject]$ReuseReceipt
    )

    if ($null -ne $ReuseReceipt) {
        if ($null -ne $Fixture) {
            Fail-Manifest "test-only bootstrap fixtures cannot replace a selected reuse working set"
        }
        return Restore-ReusedNativeWindowsBootstrap $Runtime $Paths $DirectToolchain $ReuseReceipt
    }

    $msvcSetup = Invoke-CanonicalMsvcSetup $Runtime $Paths $Materialization $DirectToolchain
    $bootstrapDirectory = Assert-RunPath (Join-Path $Paths.tool_staging "bootstrap")
    $null = [System.IO.Directory]::CreateDirectory($bootstrapDirectory)
    $nextestZip = Acquire-PinnedArtifact $Runtime.nextest_url $Runtime.nextest_sha256 (Join-Path $bootstrapDirectory "cargo-nextest.zip") $Fixture "nextest_zip" "Nextest ZIP"
    $nextest = Extract-NextestExecutable $nextestZip $Paths
    $v8Archive = Acquire-PinnedArtifact $Runtime.v8_archive_url $Runtime.v8_archive_sha256 (Join-Path $Paths.v8_cache "rusty-v8.lib.gz") $Fixture "v8_archive" "V8 archive"
    $v8Binding = Acquire-PinnedArtifact $Runtime.v8_binding_url $Runtime.v8_binding_sha256 (Join-Path $Paths.v8_cache "src-binding.rs") $Fixture "v8_binding" "V8 binding"
    $env:RUSTY_V8_ARCHIVE = $v8Archive.path
    $env:RUSTY_V8_SRC_BINDING_PATH = $v8Binding.path
    Prepend-PathEntries @($DirectToolchain.toolchain_bin, $nextest.executable_directory)
    return [pscustomobject]@{
        status = "success"
        source = if ($null -eq $Fixture) { "native-https" } else { "test-only-fixture" }
        direct_toolchain = $DirectToolchain
        msvc_setup = $msvcSetup
        nextest = [pscustomobject]@{
            zip = $nextestZip
            executable = $nextest
        }
        v8 = [pscustomobject]@{
            archive = $v8Archive
            binding = $v8Binding
        }
        path_prefix = @($DirectToolchain.toolchain_bin, $nextest.executable_directory)
    }
}

function Assert-ReusedPinnedArtifact {
    param(
        [System.Collections.IDictionary]$Record,
        [string]$ExpectedPath,
        [string]$ExpectedSha256,
        [string]$Name,
        [string]$PathField = "path"
    )

    Assert-ReuseReceiptPath $Record[$PathField] $ExpectedPath "$Name.$PathField"
    $recordedSha256 = Get-RequiredSha256 $Record["sha256"] "$Name.sha256" -Lowercase
    if (-not [string]::IsNullOrEmpty($ExpectedSha256) -and $recordedSha256 -cne $ExpectedSha256) {
        Fail-Manifest "$Name receipt SHA-256 does not match the frozen manifest"
    }
    $path = Assert-RunPath $ExpectedPath
    if (-not [System.IO.File]::Exists($path)) {
        Fail-Manifest "$Name is missing from the selected reuse working set: $path"
    }
    $item = Get-Item -LiteralPath $path -Force
    if (
        $item -isnot [System.IO.FileInfo] -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -le 0
    ) {
        Fail-Manifest "$Name must be a non-empty non-reparse regular file"
    }
    if ((Get-FileSha256 $path) -cne $recordedSha256) {
        Fail-Manifest "$Name no longer matches its selected reuse receipt"
    }
    return [pscustomobject]$Record
}

function Assert-ReusedDirectToolchain {
    param(
        [System.Collections.IDictionary]$Recorded,
        [pscustomobject]$Current
    )

    $recordedBin = [System.IO.Path]::GetFullPath(
        (Get-RequiredString $Recorded["toolchain_bin"] "selected reuse bootstrap.direct_toolchain.toolchain_bin")
    )
    if (-not $recordedBin.Equals($Current.toolchain_bin, [System.StringComparison]::OrdinalIgnoreCase)) {
        Fail-Manifest "selected reuse direct Rust toolchain differs from the installed toolchain"
    }
    foreach ($name in @("cargo", "rustc", "rustdoc")) {
        $record = Get-RequiredMap $Recorded[$name] "selected reuse bootstrap.direct_toolchain.$name"
        $recordedPath = [System.IO.Path]::GetFullPath(
            (Get-RequiredString $record["path"] "selected reuse bootstrap.direct_toolchain.$name.path")
        )
        if (-not $recordedPath.Equals($Current.$name.path, [System.StringComparison]::OrdinalIgnoreCase)) {
            Fail-Manifest "selected reuse $name.exe path differs from the installed toolchain"
        }
        if ((Get-RequiredSha256 $record["sha256"] "selected reuse bootstrap.direct_toolchain.$name.sha256" -Lowercase) -cne $Current.$name.sha256) {
            Fail-Manifest "selected reuse $name.exe differs from the installed toolchain"
        }
    }
}

function Restore-ReusedNativeWindowsBootstrap {
    param(
        [pscustomobject]$Runtime,
        [pscustomobject]$Paths,
        [pscustomobject]$DirectToolchain,
        [pscustomobject]$ReuseReceipt
    )

    $bootstrap = Get-RequiredMap $ReuseReceipt.value["bootstrap"] "selected reuse receipt.bootstrap"
    $recordedDirectToolchain = Get-RequiredMap $bootstrap["direct_toolchain"] "selected reuse receipt.bootstrap.direct_toolchain"
    Assert-ReusedDirectToolchain $recordedDirectToolchain $DirectToolchain

    $recordedMsvc = Get-RequiredMap $bootstrap["msvc_setup"] "selected reuse receipt.bootstrap.msvc_setup"
    if ((Get-RequiredString $recordedMsvc["status"] "selected reuse receipt.bootstrap.msvc_setup.status") -cne "success") {
        Fail-Manifest "selected reuse receipt does not record a successful MSVC setup"
    }
    $setupPath = Assert-RunPath (Join-Path $Paths.candidate_root ".github\actions\setup-msvc-env\setup-msvc-env.ps1")
    Assert-ReuseReceiptPath $recordedMsvc["script_path"] $setupPath "selected reuse receipt.bootstrap.msvc_setup.script_path"
    if (-not [System.IO.File]::Exists($setupPath)) {
        Fail-Manifest "selected reuse candidate MSVC setup script is missing: $setupPath"
    }
    $setupItem = Get-Item -LiteralPath $setupPath -Force
    if (
        $setupItem -isnot [System.IO.FileInfo] -or
        ($setupItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        Fail-Manifest "selected reuse candidate MSVC setup script must be a non-reparse regular file"
    }
    if ((Get-FileSha256 $setupPath) -cne (Get-RequiredSha256 $recordedMsvc["script_sha256"] "selected reuse receipt.bootstrap.msvc_setup.script_sha256" -Lowercase)) {
        Fail-Manifest "selected reuse candidate MSVC setup script changed"
    }
    $recordedEnvironment = Get-RequiredMap $recordedMsvc["environment"] "selected reuse receipt.bootstrap.msvc_setup.environment"
    $environmentPath = Assert-RunPath (Join-Path $Paths.helper_state "msvc.github-env")
    Assert-ReuseReceiptPath $recordedEnvironment["path"] $environmentPath "selected reuse receipt.bootstrap.msvc_setup.environment.path"
    $environment = Import-MsvcEnvironmentFile $environmentPath $Runtime $DirectToolchain
    if ($environment.sha256 -cne (Get-RequiredSha256 $recordedEnvironment["sha256"] "selected reuse receipt.bootstrap.msvc_setup.environment.sha256" -Lowercase)) {
        Fail-Manifest "selected reuse MSVC environment record changed"
    }
    $msvcSetup = [pscustomobject]@{
        status = "success"
        script_path = $setupPath
        script_sha256 = Get-FileSha256 $setupPath
        stdout_path = Assert-RunPath (Get-RequiredString $recordedMsvc["stdout_path"] "selected reuse receipt.bootstrap.msvc_setup.stdout_path")
        stderr_path = Assert-RunPath (Get-RequiredString $recordedMsvc["stderr_path"] "selected reuse receipt.bootstrap.msvc_setup.stderr_path")
        environment = $environment
    }

    $recordedNextest = Get-RequiredMap $bootstrap["nextest"] "selected reuse receipt.bootstrap.nextest"
    $nextestZip = Assert-ReusedPinnedArtifact (Get-RequiredMap $recordedNextest["zip"] "selected reuse receipt.bootstrap.nextest.zip") (Join-Path $Paths.tool_staging "bootstrap\cargo-nextest.zip") $Runtime.nextest_sha256 "selected reuse Nextest ZIP"
    $nextestExecutable = Assert-ReusedPinnedArtifact (Get-RequiredMap $recordedNextest["executable"] "selected reuse receipt.bootstrap.nextest.executable") (Join-Path $Paths.tool_staging "nextest\cargo-nextest.exe") $null "selected reuse Nextest executable" -PathField "executable_path"
    $nextestDirectory = Assert-RunPath (Join-Path $Paths.tool_staging "nextest")
    Assert-ReuseReceiptPath $nextestExecutable.executable_directory $nextestDirectory "selected reuse receipt.bootstrap.nextest.executable.executable_directory"

    $recordedV8 = Get-RequiredMap $bootstrap["v8"] "selected reuse receipt.bootstrap.v8"
    $v8Archive = Assert-ReusedPinnedArtifact (Get-RequiredMap $recordedV8["archive"] "selected reuse receipt.bootstrap.v8.archive") (Join-Path $Paths.v8_cache "rusty-v8.lib.gz") $Runtime.v8_archive_sha256 "selected reuse V8 archive"
    $v8Binding = Assert-ReusedPinnedArtifact (Get-RequiredMap $recordedV8["binding"] "selected reuse receipt.bootstrap.v8.binding") (Join-Path $Paths.v8_cache "src-binding.rs") $Runtime.v8_binding_sha256 "selected reuse V8 binding"
    $env:RUSTY_V8_ARCHIVE = $v8Archive.path
    $env:RUSTY_V8_SRC_BINDING_PATH = $v8Binding.path
    Prepend-PathEntries @($DirectToolchain.toolchain_bin, $nextestDirectory)
    return [pscustomobject]@{
        status = "success"
        source = "reused"
        direct_toolchain = $DirectToolchain
        msvc_setup = $msvcSetup
        nextest = [pscustomobject]@{
            zip = $nextestZip
            executable = $nextestExecutable
        }
        v8 = [pscustomobject]@{
            archive = $v8Archive
            binding = $v8Binding
        }
        path_prefix = @($DirectToolchain.toolchain_bin, $nextestDirectory)
    }
}

function Resolve-NativeGit {
    $command = Get-Command -Name "git.exe" -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($null -eq $command -or [string]::IsNullOrWhiteSpace([string]$command.Source)) {
        Fail-Manifest "native git.exe is required for Windows candidate materialization"
    }
    $path = [System.IO.Path]::GetFullPath([string]$command.Source)
    if (
        -not [System.IO.Path]::IsPathFullyQualified($path) -or
        -not $path.StartsWith("C:\", [System.StringComparison]::OrdinalIgnoreCase)
    ) {
        Fail-Manifest "native git.exe must resolve to an absolute C-drive executable: $path"
    }
    if (-not [System.IO.File]::Exists($path)) {
        Fail-Manifest "native git.exe resolved to a missing file: $path"
    }
    $item = Get-Item -LiteralPath $path -Force
    if (
        $item -isnot [System.IO.FileInfo] -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        Fail-Manifest "native git.exe must be a non-reparse C-drive file: $path"
    }
    return [pscustomobject]@{
        path = $path
        sha256 = Get-FileSha256 $path
    }
}

function Resolve-NativeGitUsrBin {
    param([pscustomobject]$NativeGit)

    $gitCommandDirectory = [System.IO.Path]::GetDirectoryName($NativeGit.path)
    if (
        [string]::IsNullOrWhiteSpace($gitCommandDirectory) -or
        -not [System.IO.Path]::GetFileName($gitCommandDirectory).Equals("cmd", [System.StringComparison]::OrdinalIgnoreCase)
    ) {
        Fail-Manifest "native Git installation does not expose its cmd directory"
    }
    $gitRoot = [System.IO.Directory]::GetParent($gitCommandDirectory)
    if ($null -eq $gitRoot) {
        Fail-Manifest "native Git installation root is unavailable"
    }
    $usrBin = [System.IO.Path]::GetFullPath((Join-Path $gitRoot.FullName "usr\bin"))
    if (
        -not $usrBin.StartsWith("C:\", [System.StringComparison]::OrdinalIgnoreCase) -or
        -not [System.IO.Directory]::Exists($usrBin)
    ) {
        Fail-Manifest "native Git usr\\bin is unavailable: $usrBin"
    }
    $usrBinItem = Get-Item -LiteralPath $usrBin -Force
    if (
        $usrBinItem -isnot [System.IO.DirectoryInfo] -or
        ($usrBinItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
    ) {
        Fail-Manifest "native Git usr\\bin must be a non-reparse directory: $usrBin"
    }
    $truePath = Assert-AbsoluteCDriveRegularFile (Join-Path $usrBin "true.exe") "native Git usr\\bin true.exe"
    return [pscustomobject]@{
        directory = $usrBin
        true = $truePath
    }
}

function New-GitEnvironment {
    param([pscustomobject]$Paths)

    $globalConfig = Assert-RunPath (Join-Path $Paths.helper_state "gitconfig")
    if (-not [System.IO.File]::Exists($globalConfig)) {
        [System.IO.File]::WriteAllText($globalConfig, "", [System.Text.UTF8Encoding]::new($false))
    }
    return [ordered]@{
        TEMP = $Paths.temp_dir
        TMP = $Paths.temp_dir
        HOME = $Paths.helper_state
        USERPROFILE = $Paths.helper_state
        APPDATA = $Paths.helper_state
        LOCALAPPDATA = $Paths.helper_state
        GIT_CONFIG_NOSYSTEM = "1"
        GIT_CONFIG_GLOBAL = $globalConfig
        GIT_ATTR_NOSYSTEM = "1"
        GIT_OPTIONAL_LOCKS = "0"
        GIT_TERMINAL_PROMPT = "0"
    }
}

function Invoke-DirectProcess {
    param(
        [string]$FileName,
        [string[]]$ArgumentVector,
        [string]$WorkingDirectory,
        [System.Collections.IDictionary]$Environment,
        [string]$Context
    )

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FileName
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.CreateNoWindow = $true
    foreach ($argument in $ArgumentVector) {
        $null = $startInfo.ArgumentList.Add($argument)
    }
    foreach ($key in $Environment.Keys) {
        $startInfo.Environment[[string]$key] = [string]$Environment[$key]
    }

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $output = [System.IO.MemoryStream]::new()
    try {
        if (-not $process.Start()) {
            Fail-Manifest "$Context did not start"
        }
        $stderrTask = $process.StandardError.ReadToEndAsync()
        $process.StandardOutput.BaseStream.CopyTo($output)
        $process.WaitForExit()
        $stderr = $stderrTask.GetAwaiter().GetResult()
        return [pscustomobject]@{
            exit_code = $process.ExitCode
            stdout_bytes = $output.ToArray()
            stderr = $stderr
            file_name = $FileName
            arguments = $ArgumentVector
        }
    } finally {
        $output.Dispose()
        $process.Dispose()
    }
}

function Invoke-DirectProcessToFile {
    param(
        [string]$FileName,
        [string[]]$ArgumentVector,
        [string]$WorkingDirectory,
        [System.Collections.IDictionary]$Environment,
        [string]$StdoutPath,
        [string]$Context
    )

    $resolvedOutput = Assert-RunPath $StdoutPath
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FileName
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.CreateNoWindow = $true
    foreach ($argument in $ArgumentVector) {
        $null = $startInfo.ArgumentList.Add($argument)
    }
    foreach ($key in $Environment.Keys) {
        $startInfo.Environment[[string]$key] = [string]$Environment[$key]
    }

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $stream = [System.IO.File]::Open(
        $resolvedOutput,
        [System.IO.FileMode]::Create,
        [System.IO.FileAccess]::Write,
        [System.IO.FileShare]::None
    )
    try {
        if (-not $process.Start()) {
            Fail-Manifest "$Context did not start"
        }
        $stderrTask = $process.StandardError.ReadToEndAsync()
        $process.StandardOutput.BaseStream.CopyTo($stream)
        $process.WaitForExit()
        $stderr = $stderrTask.GetAwaiter().GetResult()
        return [pscustomobject]@{
            exit_code = $process.ExitCode
            stdout_path = $resolvedOutput
            stdout_byte_length = $stream.Length
            stderr = $stderr
            file_name = $FileName
            arguments = $ArgumentVector
        }
    } finally {
        $stream.Dispose()
        $process.Dispose()
    }
}

function Invoke-Git {
    param(
        [string]$GitPath,
        [System.Collections.IDictionary]$GitEnvironment,
        [string[]]$ArgumentVector,
        [pscustomobject]$Paths,
        [string]$EvidenceStem,
        [int[]]$AllowedExitCodes = @(0)
    )

    $result = Invoke-DirectProcess $GitPath $ArgumentVector $Paths.helper_state $GitEnvironment "native git $EvidenceStem"
    $stdoutPath = Assert-RunPath (Join-Path $Paths.evidence_dir "$EvidenceStem.stdout.bin")
    $stderrPath = Assert-RunPath (Join-Path $Paths.evidence_dir "$EvidenceStem.stderr.txt")
    Write-BytesEvidence $stdoutPath $result.stdout_bytes
    Write-TextEvidence $stderrPath $result.stderr
    if ($AllowedExitCodes -notcontains $result.exit_code) {
        $detail = $result.stderr.Trim()
        Fail-Manifest "native git $EvidenceStem failed with exit code $($result.exit_code): $detail"
    }
    $result | Add-Member -NotePropertyName stdout_path -NotePropertyValue $stdoutPath
    $result | Add-Member -NotePropertyName stderr_path -NotePropertyValue $stderrPath
    return $result
}

function Invoke-GitToFile {
    param(
        [string]$GitPath,
        [System.Collections.IDictionary]$GitEnvironment,
        [string[]]$ArgumentVector,
        [pscustomobject]$Paths,
        [string]$EvidenceStem,
        [int[]]$AllowedExitCodes = @(0)
    )

    $stdoutPath = Assert-RunPath (Join-Path $Paths.evidence_dir "$EvidenceStem.stdout.bin")
    $result = Invoke-DirectProcessToFile $GitPath $ArgumentVector $Paths.helper_state $GitEnvironment $stdoutPath "native git $EvidenceStem"
    $stderrPath = Assert-RunPath (Join-Path $Paths.evidence_dir "$EvidenceStem.stderr.txt")
    Write-TextEvidence $stderrPath $result.stderr
    if ($AllowedExitCodes -notcontains $result.exit_code) {
        $detail = $result.stderr.Trim()
        Fail-Manifest "native git $EvidenceStem failed with exit code $($result.exit_code): $detail"
    }
    $result | Add-Member -NotePropertyName stderr_path -NotePropertyValue $stderrPath
    return $result
}

function Get-SourceExcludesFile {
    param(
        [pscustomobject]$Runtime,
        [pscustomobject]$Paths
    )

    # The query intentionally inherits the Windows process environment so WSL
    # observes the source repository's canonical Git configuration. Native Git
    # remains isolated through New-GitEnvironment.
    $result = Invoke-DirectProcess "wsl.exe" @(
        "--distribution",
        [string]$Runtime.source_materialization.wsl_distro_name,
        "--exec",
        "git",
        "-C",
        [string]$Runtime.source_materialization.posix_repo_root,
        "config",
        "--path",
        "--get",
        "core.excludesFile"
    ) $Paths.helper_state @{} "WSL source core.excludesFile query"
    if ($result.exit_code -eq 1) {
        if ($result.stdout_bytes.Length -ne 0 -or -not [string]::IsNullOrWhiteSpace($result.stderr)) {
            Fail-Manifest "WSL source core.excludesFile query returned output on its allowed missing-value exit"
        }
        return $null
    }
    if ($result.exit_code -ne 0) {
        $detail = $result.stderr.Trim()
        Fail-Manifest "WSL source core.excludesFile query failed with exit code $($result.exit_code): $detail"
    }

    $posixExcludesFile = Get-SafePosixSourceRoot (Get-TextFromBytes $result.stdout_bytes) "source core.excludesFile"
    $segments = $posixExcludesFile.Substring(1).Split("/", [System.StringSplitOptions]::None)
    return "\\wsl.localhost\$($Runtime.source_materialization.wsl_distro_name)\$($segments -join '\')"
}

function Get-GitSnapshotArguments {
    param(
        [string]$Repository,
        [string[]]$Command,
        [string]$ExcludesFile = $null
    )

    $arguments = @(
        "--no-optional-locks",
        "-c", "core.refreshIndex=false",
        "-c", "core.fsmonitor=false",
        "-c", "core.filemode=false",
        "-c", "safe.directory=$Repository"
    )
    if ($null -ne $ExcludesFile) {
        $arguments += @("-c", "core.excludesFile=$ExcludesFile")
    }
    return $arguments + @("-C", $Repository) + $Command
}

function Get-IndexSymlinkEntries {
    param([byte[]]$IndexBytes)

    $text = [System.Text.UTF8Encoding]::new($false).GetString($IndexBytes)
    $symlinkEntries = [System.Collections.Generic.List[object]]::new()
    $gitlinkPaths = [System.Collections.Generic.List[string]]::new()
    foreach ($record in $text.Split([char]0)) {
        if ([string]::IsNullOrEmpty($record)) {
            continue
        }
        $tab = $record.IndexOf("`t", [System.StringComparison]::Ordinal)
        if ($tab -le 0) {
            Fail-Manifest "native git index entry has an unsupported shape"
        }
        $header = $record.Substring(0, $tab)
        $fields = @($header -split " " | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        if ($fields.Count -ne 3) {
            Fail-Manifest "native git index entry has an unsupported shape"
        }
        $path = $record.Substring($tab + 1)
        if ($fields[0] -eq "160000") {
            $gitlinkPaths.Add($path)
        } elseif ($fields[0] -eq "120000") {
            if ($fields[2] -ne "0") {
                Fail-Manifest "native git symlink index entry is not stage zero"
            }
            $symlinkEntries.Add([pscustomobject]@{
                    path = $path
                    object_id = Assert-GitObjectId $fields[1] "native git symlink index object"
                })
        }
    }
    if ($gitlinkPaths.Count -ne 0) {
        Fail-Manifest "candidate materialization rejects gitlinks or submodules: $($gitlinkPaths -join ', ')"
    }
    return $symlinkEntries.ToArray()
}

function Capture-GitSnapshot {
    param(
        [string]$GitPath,
        [System.Collections.IDictionary]$GitEnvironment,
        [string]$Repository,
        [pscustomobject]$Paths,
        [string]$Prefix,
        [string]$ExcludesFile = $null
    )

    $headResult = Invoke-Git $GitPath $GitEnvironment (Get-GitSnapshotArguments -Repository $Repository -Command @("rev-parse", "HEAD") -ExcludesFile $ExcludesFile) $Paths "$Prefix-head"
    $head = Assert-GitObjectId (Get-TextFromBytes $headResult.stdout_bytes) "$Prefix HEAD"

    $symbolicHeadResult = Invoke-Git $GitPath $GitEnvironment (Get-GitSnapshotArguments -Repository $Repository -Command @("symbolic-ref", "-q", "HEAD") -ExcludesFile $ExcludesFile) $Paths "$Prefix-symbolic-head" @(0, 1)
    $symbolicHead = if ($symbolicHeadResult.exit_code -eq 0) {
        Get-RequiredString (Get-TextFromBytes $symbolicHeadResult.stdout_bytes) "$Prefix symbolic HEAD"
    } else {
        if ($symbolicHeadResult.stdout_bytes.Length -ne 0) {
            Fail-Manifest "$Prefix symbolic HEAD returned output on its allowed detached exit"
        }
        $null
    }

    $mergeResult = Invoke-Git $GitPath $GitEnvironment (Get-GitSnapshotArguments -Repository $Repository -Command @("rev-parse", "-q", "--verify", "MERGE_HEAD") -ExcludesFile $ExcludesFile) $Paths "$Prefix-merge-head" @(0, 1)
    $mergeHead = if ($mergeResult.exit_code -eq 0) {
        Assert-GitObjectId (Get-TextFromBytes $mergeResult.stdout_bytes) "$Prefix MERGE_HEAD"
    } else {
        if ($mergeResult.stdout_bytes.Length -ne 0) {
            Fail-Manifest "$Prefix MERGE_HEAD returned output on its allowed missing-object exit"
        }
        $null
    }

    $treeResult = Invoke-Git $GitPath $GitEnvironment (Get-GitSnapshotArguments -Repository $Repository -Command @("write-tree") -ExcludesFile $ExcludesFile) $Paths "$Prefix-write-tree"
    $indexTree = Assert-GitObjectId (Get-TextFromBytes $treeResult.stdout_bytes) "$Prefix index tree"
    $unmergedResult = Invoke-Git $GitPath $GitEnvironment (Get-GitSnapshotArguments -Repository $Repository -Command @("ls-files", "-u", "-z") -ExcludesFile $ExcludesFile) $Paths "$Prefix-unmerged"
    if ($unmergedResult.stdout_bytes.Length -ne 0) {
        Fail-Manifest "$Prefix has unmerged index entries"
    }
    $indexResult = Invoke-Git $GitPath $GitEnvironment (Get-GitSnapshotArguments -Repository $Repository -Command @("ls-files", "-s", "-z") -ExcludesFile $ExcludesFile) $Paths "$Prefix-index"
    $symlinkEntries = @(Get-IndexSymlinkEntries $indexResult.stdout_bytes)
    $refsResult = Invoke-Git $GitPath $GitEnvironment (Get-GitSnapshotArguments -Repository $Repository -Command @("show-ref", "--head") -ExcludesFile $ExcludesFile) $Paths "$Prefix-refs"
    $statusResult = Invoke-Git $GitPath $GitEnvironment (Get-GitSnapshotArguments -Repository $Repository -Command @("status", "--porcelain=v2", "-z") -ExcludesFile $ExcludesFile) $Paths "$Prefix-status"
    $untrackedResult = Invoke-Git $GitPath $GitEnvironment (Get-GitSnapshotArguments -Repository $Repository -Command @("ls-files", "--others", "--exclude-standard", "-z") -ExcludesFile $ExcludesFile) $Paths "$Prefix-untracked"
    $ignoredUntrackedResult = Invoke-Git $GitPath $GitEnvironment (Get-GitSnapshotArguments -Repository $Repository -Command @("ls-files", "--others", "--ignored", "--exclude-standard", "-z") -ExcludesFile $ExcludesFile) $Paths "$Prefix-ignored-untracked"
    $worktreeIndexResult = Invoke-Git $GitPath $GitEnvironment (Get-GitSnapshotArguments -Repository $Repository -Command @("diff", "--quiet") -ExcludesFile $ExcludesFile) $Paths "$Prefix-worktree-index" @(0, 1)

    return [pscustomobject]@{
        repository = $Repository
        head = $head
        symbolic_head = $symbolicHead
        merge_head = $mergeHead
        index_tree = $indexTree
        refs = New-ByteRecord $refsResult.stdout_path $refsResult.stdout_bytes
        index_entries = New-ByteRecord $indexResult.stdout_path $indexResult.stdout_bytes
        status = New-ByteRecord $statusResult.stdout_path $statusResult.stdout_bytes
        unmerged = New-ByteRecord $unmergedResult.stdout_path $unmergedResult.stdout_bytes
        untracked = New-ByteRecord $untrackedResult.stdout_path $untrackedResult.stdout_bytes
        ignored_untracked = New-ByteRecord $ignoredUntrackedResult.stdout_path $ignoredUntrackedResult.stdout_bytes
        worktree_equals_index = ($worktreeIndexResult.exit_code -eq 0)
        symlink_entries = $symlinkEntries
        symlink_placeholders = $null
    }
}

function Get-SnapshotSummary {
    param([pscustomobject]$Snapshot)

    return [ordered]@{
        repository = $Snapshot.repository
        head = $Snapshot.head
        symbolic_head = $Snapshot.symbolic_head
        merge_head = $Snapshot.merge_head
        index_tree = $Snapshot.index_tree
        refs = [ordered]@{ byte_length = $Snapshot.refs.byte_length; sha256 = $Snapshot.refs.sha256; path = $Snapshot.refs.path }
        index_entries = [ordered]@{ byte_length = $Snapshot.index_entries.byte_length; sha256 = $Snapshot.index_entries.sha256; path = $Snapshot.index_entries.path }
        status = [ordered]@{ byte_length = $Snapshot.status.byte_length; sha256 = $Snapshot.status.sha256; path = $Snapshot.status.path }
        unmerged = [ordered]@{ byte_length = $Snapshot.unmerged.byte_length; sha256 = $Snapshot.unmerged.sha256; path = $Snapshot.unmerged.path }
        untracked = [ordered]@{ byte_length = $Snapshot.untracked.byte_length; sha256 = $Snapshot.untracked.sha256; path = $Snapshot.untracked.path }
        ignored_untracked = [ordered]@{ byte_length = $Snapshot.ignored_untracked.byte_length; sha256 = $Snapshot.ignored_untracked.sha256; path = $Snapshot.ignored_untracked.path }
        worktree_equals_index = $Snapshot.worktree_equals_index
        symlink_entries = @($Snapshot.symlink_entries | ForEach-Object {
                [ordered]@{ path = $_.path; object_id = $_.object_id }
            })
        symlink_placeholders = $Snapshot.symlink_placeholders
    }
}

function Assert-SourceMatchesCandidateIdentity {
    param(
        [pscustomobject]$Snapshot,
        [pscustomobject]$Candidate
    )

    if ($Snapshot.head -cne $Candidate.head) {
        Fail-Manifest "source HEAD does not match manifest candidate_identity.head"
    }
    if ($Snapshot.index_tree -cne $Candidate.index_tree) {
        Fail-Manifest "source index tree does not match manifest candidate_identity.index_tree"
    }
    if ($Snapshot.merge_head -cne $Candidate.merge_head) {
        Fail-Manifest "source MERGE_HEAD does not match manifest candidate_identity.merge_head"
    }
}

function Assert-SnapshotUnchanged {
    param(
        [pscustomobject]$Before,
        [pscustomobject]$After,
        [string]$Description
    )

    foreach ($field in @("head", "symbolic_head", "merge_head", "index_tree", "worktree_equals_index")) {
        if ($Before.$field -cne $After.$field) {
            Fail-Manifest "$Description changed $field"
        }
    }
    foreach ($field in @("refs", "index_entries", "status", "unmerged", "untracked", "ignored_untracked")) {
        $beforeRecord = $Before.$field
        $afterRecord = $After.$field
        if (
            $beforeRecord.byte_length -ne $afterRecord.byte_length -or
            $beforeRecord.sha256 -cne $afterRecord.sha256 -or
            -not (Test-BytesEqual $beforeRecord.bytes $afterRecord.bytes)
        ) {
            Fail-Manifest "$Description changed $field bytes"
        }
    }
}

function Assert-SafeRelativeGitPath {
    param([string]$RelativePath)

    if (
        [string]::IsNullOrWhiteSpace($RelativePath) -or
        $RelativePath.StartsWith("/", [System.StringComparison]::Ordinal) -or
        $RelativePath.IndexOfAny([char[]]@(0, 92)) -ge 0
    ) {
        Fail-Manifest "candidate symlink path is unsafe"
    }
    foreach ($segment in $RelativePath.Split("/", [System.StringSplitOptions]::None)) {
        if (
            [string]::IsNullOrEmpty($segment) -or
            $segment -in @(".", "..") -or
            $segment -notmatch "^[A-Za-z0-9._-]+$"
        ) {
            Fail-Manifest "candidate symlink path is unsafe"
        }
    }
}

function Assert-CandidateSymlinkPlaceholders {
    param(
        [string]$GitPath,
        [System.Collections.IDictionary]$GitEnvironment,
        [pscustomobject]$Paths,
        [pscustomobject]$Snapshot,
        [pscustomobject]$SourceSymlinkState,
        [string]$Prefix
    )

    $sourceEntries = @($SourceSymlinkState.entries)
    $candidateEntries = @($Snapshot.symlink_entries)
    if ($candidateEntries.Count -ne $sourceEntries.Count) {
        Fail-Manifest "candidate did not preserve the source symlink index entry count"
    }
    $dispositions = [System.Collections.Generic.List[object]]::new()
    for ($index = 0; $index -lt $sourceEntries.Count; $index++) {
        $sourceEntry = $sourceEntries[$index]
        $relativePath = [string]$sourceEntry.path
        Assert-SafeRelativeGitPath $relativePath
        $matches = @($candidateEntries | Where-Object { $_.path -ceq $relativePath })
        if ($matches.Count -ne 1 -or $matches[0].object_id -cne $sourceEntry.object_id) {
            Fail-Manifest "candidate did not preserve mode-120000 index entry $relativePath"
        }
        $candidatePath = Assert-RunPath (Join-Path $Paths.candidate_root ($relativePath.Replace("/", "\")))
        if (-not (Test-Path -LiteralPath $candidatePath)) {
            Fail-Manifest "candidate symlink placeholder is missing: $relativePath"
        }
        $item = Get-Item -LiteralPath $candidatePath -Force
        if (
            $item -isnot [System.IO.FileInfo] -or
            ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            Fail-Manifest "candidate symlink placeholder must be a non-reparse regular file: $relativePath"
        }
        $placeholderBytes = [System.IO.File]::ReadAllBytes($candidatePath)
        $blobResult = Invoke-Git $GitPath $GitEnvironment (Get-GitSnapshotArguments $Paths.candidate_root @("cat-file", "blob", [string]$sourceEntry.object_id)) $Paths "$Prefix-symlink-$index-blob"
        if (-not (Test-BytesEqual $placeholderBytes $blobResult.stdout_bytes)) {
            Fail-Manifest "candidate symlink placeholder bytes do not match its indexed blob: $relativePath"
        }
        $dispositions.Add([pscustomobject]@{
                path = $relativePath
                mode = "120000"
                object_id = [string]$sourceEntry.object_id
                disposition = "git-symlink-placeholder"
                worktree_path = $candidatePath
                indexed_blob_sha256 = Get-ByteDigest $blobResult.stdout_bytes
                placeholder_sha256 = Get-ByteDigest $placeholderBytes
                byte_length = $placeholderBytes.Length
            })
    }
    return $dispositions.ToArray()
}

function Assert-CandidateIntegrity {
    param(
        [string]$GitPath,
        [System.Collections.IDictionary]$GitEnvironment,
        [pscustomobject]$Paths,
        [pscustomobject]$Candidate,
        [pscustomobject]$SourceSymlinkState,
        [string]$Prefix,
        [switch]$AllowIgnoredUntracked
    )

    $snapshot = Capture-GitSnapshot $GitPath $GitEnvironment $Paths.candidate_root $Paths $Prefix
    if ($snapshot.head -cne $Candidate.head) {
        Fail-Manifest "candidate HEAD does not match manifest candidate_identity.head"
    }
    if ($snapshot.index_tree -cne $Candidate.index_tree) {
        Fail-Manifest "candidate index tree does not match manifest candidate_identity.index_tree"
    }
    if ($snapshot.untracked.byte_length -ne 0) {
        Fail-Manifest "candidate has untracked paths"
    }
    if (-not $AllowIgnoredUntracked -and $snapshot.ignored_untracked.byte_length -ne 0) {
        Fail-Manifest "candidate has ignored untracked paths"
    }
    if (-not $snapshot.worktree_equals_index) {
        Fail-Manifest "candidate worktree differs from its index"
    }
    $snapshot.symlink_placeholders = @(
        Assert-CandidateSymlinkPlaceholders $GitPath $GitEnvironment $Paths $snapshot $SourceSymlinkState $Prefix
    )
    return $snapshot
}

function Assert-ReusedCandidateReady {
    param(
        [string]$GitPath,
        [System.Collections.IDictionary]$GitEnvironment,
        [pscustomobject]$Paths,
        [pscustomobject]$Candidate,
        [string]$Prefix
    )

    $snapshot = Capture-GitSnapshot $GitPath $GitEnvironment $Paths.candidate_root $Paths $Prefix
    if ($snapshot.head -cne $Candidate.head) {
        Fail-Manifest "reused candidate HEAD does not match manifest candidate_identity.head"
    }
    if ($snapshot.untracked.byte_length -ne 0) {
        Fail-Manifest "reused candidate has untracked paths"
    }
    if (-not $snapshot.worktree_equals_index) {
        Fail-Manifest "reused candidate worktree differs from its index"
    }
    return $snapshot
}

function Assert-SourceReadyForMaterialization {
    param([pscustomobject]$Snapshot)

    if ($Snapshot.untracked.byte_length -ne 0) {
        Fail-Manifest "source repository has ordinary untracked paths"
    }
    if (-not $Snapshot.worktree_equals_index) {
        Fail-Manifest "source repository worktree differs from its index"
    }
}

function Finalize-SourceIntegrity {
    if ($script:SourceFinalized -or $null -eq $script:SourceSnapshotBefore) {
        return
    }
    if (
        $null -eq $script:NativeGit -or
        $null -eq $script:GitEnvironment -or
        [string]::IsNullOrWhiteSpace($script:SourceRepository) -or
        $null -eq $script:RunPaths
    ) {
        Fail-Manifest "source final integrity cannot run without native Git materialization state"
    }
    $sourceFinal = Capture-GitSnapshot $script:NativeGit.path $script:GitEnvironment $script:SourceRepository $script:RunPaths "source-after-command-loop" $script:SourceExcludesFile
    Assert-SourceMatchesCandidateIdentity $sourceFinal $script:Preflight.candidate_identity
    Assert-SnapshotUnchanged $script:SourceSnapshotBefore $sourceFinal "source repository after complete command loop"
    $summary = Get-SnapshotSummary $sourceFinal
    if ($null -ne $script:Materialization) {
        $script:Materialization.source_after_command_loop = $summary
        $script:Preflight.candidate_materialization = $script:Materialization
    } elseif ($null -ne $script:Preflight -and $null -ne $script:Preflight.candidate_materialization) {
        $script:Preflight.candidate_materialization.source_after_command_loop = $summary
    }
    $script:SourceFinalized = $true
}

function Materialize-Candidate {
    param(
        [pscustomobject]$Runtime,
        [pscustomobject]$Candidate,
        [pscustomobject]$Paths,
        [pscustomobject]$NativeGit,
        [pscustomobject]$ReuseReceipt
    )

    $gitPath = $NativeGit.path
    $sourceRoot = $Runtime.source_materialization.source_unc
    $sourceGit = $Runtime.source_materialization.source_git_unc
    $sourceExcludesFile = Get-SourceExcludesFile $Runtime $Paths
    $gitEnvironment = New-GitEnvironment $Paths
    $script:NativeGit = $NativeGit
    $script:GitEnvironment = $gitEnvironment
    $script:SourceRepository = $sourceRoot
    $script:SourceExcludesFile = $sourceExcludesFile
    $sourceBefore = Capture-GitSnapshot $gitPath $gitEnvironment $sourceRoot $Paths "source-before" $sourceExcludesFile
    $script:SourceSnapshotBefore = $sourceBefore
    Assert-SourceMatchesCandidateIdentity $sourceBefore $Candidate
    Assert-SourceReadyForMaterialization $sourceBefore

    if ($null -ne $Runtime.reuse_run_root) {
        if ($null -eq $ReuseReceipt) {
            Fail-Manifest "selected reuse working set is missing its validated receipt"
        }
        if (-not (Test-Path -LiteralPath $Paths.candidate_root)) {
            Fail-Manifest "selected reuse working set is missing its candidate: $($Paths.candidate_root)"
        }
        $candidateItem = Get-Item -LiteralPath $Paths.candidate_root -Force
        if (
            $candidateItem -isnot [System.IO.DirectoryInfo] -or
            ($candidateItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            Fail-Manifest "selected reuse candidate must be a non-reparse directory: $($Paths.candidate_root)"
        }
        $candidateBeforeSync = Assert-ReusedCandidateReady $gitPath $gitEnvironment $Paths $Candidate "reused-candidate-before-sync"
        $patchResult = Invoke-GitToFile $gitPath $gitEnvironment (Get-GitSnapshotArguments $sourceRoot @(
                    "diff", "--binary", "--full-index", "--no-renames", "--no-ext-diff",
                    $candidateBeforeSync.index_tree, $sourceBefore.index_tree, "--"
                )) $Paths "reuse-source-index.patch"
        $patchPath = $patchResult.stdout_path
        if ($patchResult.stdout_byte_length -ne 0) {
            $null = Invoke-Git $gitPath $gitEnvironment @("-C", $Paths.candidate_root, "apply", "--check", "--index", "--binary", $patchPath) $Paths "reuse-candidate-apply-check"
            $null = Invoke-Git $gitPath $gitEnvironment @("-C", $Paths.candidate_root, "apply", "--index", "--binary", $patchPath) $Paths "reuse-candidate-apply"
        }

        $sourceSymlinkState = [pscustomobject]@{ entries = @($sourceBefore.symlink_entries) }
        $candidateBeforeCommand = Assert-CandidateIntegrity $gitPath $gitEnvironment $Paths $Candidate $sourceSymlinkState "reused-candidate-before-command" -AllowIgnoredUntracked
        $sourceAfter = Capture-GitSnapshot $gitPath $gitEnvironment $sourceRoot $Paths "source-after" $sourceExcludesFile
        Assert-SourceMatchesCandidateIdentity $sourceAfter $Candidate
        Assert-SnapshotUnchanged $sourceBefore $sourceAfter "source repository"

        return [pscustomobject]@{
            status = "success"
            materialized = $true
            reused = $true
            reuse_receipt_path = $ReuseReceipt.path
            candidate_root = $Paths.candidate_root
            source_unc = $sourceRoot
            source_git_unc = $sourceGit
            source_merge_head = $Candidate.merge_head
            native_git = $NativeGit
            patch_path = $patchPath
            source_before = Get-SnapshotSummary $sourceBefore
            source_after = Get-SnapshotSummary $sourceAfter
            source_symlink_state = $sourceSymlinkState
            candidate_before_sync = Get-SnapshotSummary $candidateBeforeSync
            candidate_before_command = Get-SnapshotSummary $candidateBeforeCommand
            candidate_after_command = $null
            source_after_command_loop = $null
        }
    }

    if (Test-Path -LiteralPath $Paths.candidate_root) {
        Fail-Manifest "candidate root already exists before materialization"
    }
    $initResult = Invoke-Git $gitPath $gitEnvironment @("init", "--quiet", $Paths.candidate_root) $Paths "candidate-init"
    foreach ($setting in @(
            @("core.autocrlf", "false"),
            @("core.longpaths", "true"),
            @("core.symlinks", "false"),
            @("core.filemode", "false")
        )) {
        $settingName = $setting[0]
        $settingValue = $setting[1]
        $null = Invoke-Git $gitPath $gitEnvironment @("-C", $Paths.candidate_root, "config", $settingName, $settingValue) $Paths "candidate-config-$($settingName.Replace('.', '-'))"
    }

    $null = Invoke-Git $gitPath $gitEnvironment @(
        "-C", $Paths.candidate_root,
        "-c", "protocol.file.allow=always",
        "fetch", "--no-tags", "--no-recurse-submodules", $sourceGit, "HEAD"
    ) $Paths "candidate-fetch"
    $fetchHeadResult = Invoke-Git $gitPath $gitEnvironment @("-C", $Paths.candidate_root, "rev-parse", "FETCH_HEAD") $Paths "candidate-fetch-head"
    $fetchHead = Assert-GitObjectId (Get-TextFromBytes $fetchHeadResult.stdout_bytes) "candidate FETCH_HEAD"
    if ($fetchHead -cne $Candidate.head) {
        Fail-Manifest "candidate fetch did not resolve the manifest source HEAD"
    }
    $null = Invoke-Git $gitPath $gitEnvironment @("-C", $Paths.candidate_root, "checkout", "--detach", "--force", $Candidate.head) $Paths "candidate-checkout"

    $patchResult = Invoke-GitToFile $gitPath $gitEnvironment (Get-GitSnapshotArguments $sourceRoot @("diff", "--cached", "--binary", "--full-index", "--no-renames", "--no-ext-diff", "HEAD", "--")) $Paths "source-index.patch"
    $patchPath = $patchResult.stdout_path
    if ($patchResult.stdout_byte_length -ne 0) {
        $null = Invoke-Git $gitPath $gitEnvironment @("-C", $Paths.candidate_root, "apply", "--check", "--index", "--binary", $patchPath) $Paths "candidate-apply-check"
        $null = Invoke-Git $gitPath $gitEnvironment @("-C", $Paths.candidate_root, "apply", "--index", "--binary", $patchPath) $Paths "candidate-apply"
    }

    $sourceSymlinkState = [pscustomobject]@{ entries = @($sourceBefore.symlink_entries) }
    $candidateBeforeCommand = Assert-CandidateIntegrity $gitPath $gitEnvironment $Paths $Candidate $sourceSymlinkState "candidate-before-command"
    $sourceAfter = Capture-GitSnapshot $gitPath $gitEnvironment $sourceRoot $Paths "source-after" $sourceExcludesFile
    Assert-SourceMatchesCandidateIdentity $sourceAfter $Candidate
    Assert-SnapshotUnchanged $sourceBefore $sourceAfter "source repository"

    return [pscustomobject]@{
        status = "success"
        materialized = $true
        reused = $false
        candidate_root = $Paths.candidate_root
        source_unc = $sourceRoot
        source_git_unc = $sourceGit
        source_merge_head = $Candidate.merge_head
        native_git = $NativeGit
        patch_path = $patchPath
        source_before = Get-SnapshotSummary $sourceBefore
        source_after = Get-SnapshotSummary $sourceAfter
        source_symlink_state = $sourceSymlinkState
        candidate_before_command = Get-SnapshotSummary $candidateBeforeCommand
        candidate_after_command = $null
        source_after_command_loop = $null
    }
}

function New-FakeCargoTool {
    param(
        [pscustomobject]$Paths,
        [int]$Index
    )

    $toolDirectory = Assert-RunPath (Join-Path $Paths.tool_staging "command-$Index")
    $null = [System.IO.Directory]::CreateDirectory($toolDirectory)
    $toolPath = Join-Path $toolDirectory "cargo.exe"
    $sourcePath = Join-Path $toolDirectory "cargo.cs"
    $toolSource = @'
using System;
using System.IO;
using System.Text;

public static class CargoValidateWindowsFakeCargo
{
    public static int Main(string[] args)
    {
        File.WriteAllText(
            Environment.GetEnvironmentVariable("CARGO_VALIDATE_WINDOWS_FAKE_ARGV_LOG"),
            string.Join("\n", args) + "\n",
            new UTF8Encoding(false));
        File.WriteAllLines(
            Environment.GetEnvironmentVariable("CARGO_VALIDATE_WINDOWS_FAKE_ENV_LOG"),
            new[]
            {
                "CARGO_BUILD_JOBS=" + Environment.GetEnvironmentVariable("CARGO_BUILD_JOBS"),
                "NEXTEST_TEST_THREADS=" + Environment.GetEnvironmentVariable("NEXTEST_TEST_THREADS"),
                "RUST_MIN_STACK=" + Environment.GetEnvironmentVariable("RUST_MIN_STACK"),
                "FORCE_COLOR=" + Environment.GetEnvironmentVariable("FORCE_COLOR"),
                "CARGO_TARGET_DIR=" + Environment.GetEnvironmentVariable("CARGO_TARGET_DIR"),
                "CARGO_HOME=" + Environment.GetEnvironmentVariable("CARGO_HOME"),
                "RUSTUP_HOME=" + Environment.GetEnvironmentVariable("RUSTUP_HOME"),
                "TEMP=" + Environment.GetEnvironmentVariable("TEMP"),
                "TMP=" + Environment.GetEnvironmentVariable("TMP"),
                "PYTHONPYCACHEPREFIX=" + Environment.GetEnvironmentVariable("PYTHONPYCACHEPREFIX"),
                "PATH=" + Environment.GetEnvironmentVariable("PATH"),
                "RUSTY_V8_ARCHIVE=" + Environment.GetEnvironmentVariable("RUSTY_V8_ARCHIVE"),
                "RUSTY_V8_MIRROR=" + Environment.GetEnvironmentVariable("RUSTY_V8_MIRROR"),
                "RUSTY_V8_SRC_BINDING_PATH=" + Environment.GetEnvironmentVariable("RUSTY_V8_SRC_BINDING_PATH"),
                "V8_FROM_SOURCE=" + Environment.GetEnvironmentVariable("V8_FROM_SOURCE"),
            },
            new UTF8Encoding(false));
        if (Environment.GetEnvironmentVariable("CARGO_VALIDATE_WINDOWS_FAKE_CREATE_IGNORED") == "1")
        {
            File.WriteAllText(
                Path.Combine(Environment.CurrentDirectory, ".fixture-ignored"),
                "ignored fixture\n",
                new UTF8Encoding(false));
        }
        int exitCode;
        return Int32.TryParse(
            Environment.GetEnvironmentVariable("CARGO_VALIDATE_WINDOWS_FAKE_EXIT_CODE"),
            out exitCode)
            ? exitCode
            : 1;
    }
}
'@
    [System.IO.File]::WriteAllText(
        (Assert-RunPath $sourcePath),
        $toolSource,
        [System.Text.UTF8Encoding]::new($false)
    )
    $compilerPath = "C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe"
    if (-not [System.IO.File]::Exists($compilerPath)) {
        Fail-Manifest "the fixed inert fixture compiler is unavailable: $compilerPath"
    }
    $compileInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $compileInfo.FileName = $compilerPath
    $compileInfo.UseShellExecute = $false
    $compileInfo.RedirectStandardOutput = $true
    $compileInfo.RedirectStandardError = $true
    $compileInfo.CreateNoWindow = $true
    foreach ($argument in @("/nologo", "/target:exe", "/out:$toolPath", $sourcePath)) {
        $null = $compileInfo.ArgumentList.Add($argument)
    }
    $compileInfo.Environment["TEMP"] = $Paths.temp_dir
    $compileInfo.Environment["TMP"] = $Paths.temp_dir
    $compileInfo.Environment["USERPROFILE"] = $Paths.helper_state
    $compiler = [System.Diagnostics.Process]::new()
    $compiler.StartInfo = $compileInfo
    if (-not $compiler.Start()) {
        Fail-Manifest "inert fake-cargo fixture compiler did not start"
    }
    $compilerStdoutTask = $compiler.StandardOutput.ReadToEndAsync()
    $compilerStderrTask = $compiler.StandardError.ReadToEndAsync()
    $compiler.WaitForExit()
    $compilerStdout = $compilerStdoutTask.GetAwaiter().GetResult()
    $compilerStderr = $compilerStderrTask.GetAwaiter().GetResult()
    $compilerExitCode = $compiler.ExitCode
    $compiler.Dispose()
    Write-TextEvidence (Join-Path $Paths.evidence_dir "fake-cargo-$Index-compile.stdout.txt") $compilerStdout
    Write-TextEvidence (Join-Path $Paths.evidence_dir "fake-cargo-$Index-compile.stderr.txt") $compilerStderr
    if ($compilerExitCode -ne 0) {
        Fail-Manifest "inert fake-cargo fixture compilation failed with exit code $compilerExitCode"
    }
    if (-not [System.IO.File]::Exists($toolPath)) {
        Fail-Manifest "inert fake-cargo fixture compilation did not produce cargo.exe"
    }
    return $toolPath
}

function Get-TestOnlyFakeCreateIgnoredFile {
    $value = [System.Environment]::GetEnvironmentVariable($script:TestOnlyFakeCreateIgnoredFile)
    if ([string]::IsNullOrEmpty($value)) {
        return $false
    }
    if ($value -cne "1") {
        Fail-Manifest "$($script:TestOnlyFakeCreateIgnoredFile) must equal 1 when present"
    }
    if ([System.Environment]::GetEnvironmentVariable($script:TestOnlyFakeCargoOptIn) -ne "1") {
        Fail-Manifest "$($script:TestOnlyFakeCreateIgnoredFile) requires the test-only fake-cargo process opt-in"
    }
    return $true
}

function Invoke-ApprovedCommand {
    param(
        [pscustomobject]$Approved,
        [pscustomobject]$Paths,
        [pscustomobject]$Runtime,
        [pscustomobject]$Materialization,
        [pscustomobject]$TestFixture,
        [pscustomobject]$Bootstrap,
        [bool]$Yolo
    )

    $isFixture = $null -ne $Approved.fixture
    if (-not $isFixture -and $null -eq $Bootstrap) {
        Fail-Manifest "pinned native Windows bootstrap is required before a production command can launch"
    }
    if (
        -not $isFixture -and
        ($null -eq $Materialization -or -not $Materialization.materialized)
    ) {
        Fail-Manifest "production Windows commands require a materialized candidate"
    }
    $workingDirectory = if ($null -ne $Materialization -and $Materialization.materialized) {
        Assert-RunPath (Join-Path $Paths.candidate_root "codex-rs")
    } else {
        $Paths.helper_state
    }
    if (-not [System.IO.Directory]::Exists($workingDirectory)) {
        Fail-Manifest "approved command working directory does not exist: $workingDirectory"
    }
    $python3Path = "F:\codex-tools\bin\python3.exe"
    if (-not [System.IO.File]::Exists($python3Path)) {
        Fail-Manifest "required native test Python is unavailable: $python3Path"
    }
    $python3Bin = [System.IO.Path]::GetDirectoryName($python3Path)
    $childNativeGit = if ($null -eq $script:NativeGit) { Resolve-NativeGit } else { $script:NativeGit }
    $gitUsrBin = Resolve-NativeGitUsrBin $childNativeGit

    $stdoutPath = Assert-RunPath (Join-Path $Paths.evidence_dir "command-$($Approved.index).stdout.txt")
    $stderrPath = Assert-RunPath (Join-Path $Paths.evidence_dir "command-$($Approved.index).stderr.txt")
    $argvLogPath = Assert-RunPath (Join-Path $Paths.evidence_dir "command-$($Approved.index).argv.txt")
    $envLogPath = Assert-RunPath (Join-Path $Paths.evidence_dir "command-$($Approved.index).env.txt")
    $fakeTool = $null
    $directCargo = $null
    $createIgnoredFile = $false
    if ($isFixture) {
        $fakeTool = New-FakeCargoTool $Paths $Approved.index
        if (
            -not [System.IO.Path]::IsPathFullyQualified($fakeTool) -or
            -not [System.IO.File]::Exists($fakeTool)
        ) {
            Fail-Manifest "inert fake-cargo tool must be an existing absolute path"
        }
        $fakeItem = Get-Item -LiteralPath $fakeTool -Force
        if (
            $fakeItem -isnot [System.IO.FileInfo] -or
            ($fakeItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            Fail-Manifest "inert fake-cargo tool must be a non-reparse regular file"
        }
        $createIgnoredFile = Get-TestOnlyFakeCreateIgnoredFile
    } else {
        $capturedCargo = $Bootstrap.direct_toolchain.cargo
        $directCargo = Assert-AbsoluteCDriveRegularFile ([string]$capturedCargo.path) "direct toolchain cargo.exe before launch"
        if (
            $directCargo.path -cne $capturedCargo.path -or
            $directCargo.sha256 -cne $capturedCargo.sha256
        ) {
            Fail-Manifest "direct toolchain cargo.exe changed after bootstrap capture"
        }
    }

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = if ($isFixture) { $fakeTool } else { $directCargo.path }
    $startInfo.WorkingDirectory = $workingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.CreateNoWindow = $true
    foreach ($argument in $Approved.argv[1..($Approved.argv.Length - 1)]) {
        $null = $startInfo.ArgumentList.Add($argument)
    }
    $startInfo.Environment["CARGO_BUILD_JOBS"] = [string]$Runtime.resource_contract.cargo_build_jobs
    $startInfo.Environment["NEXTEST_TEST_THREADS"] = [string]$Runtime.resource_contract.nextest_test_threads
    $startInfo.Environment["RUST_MIN_STACK"] = "8388608"
    $startInfo.Environment["FORCE_COLOR"] = "0"
    $startInfo.Environment["PYTHONPYCACHEPREFIX"] = Assert-RunPath (Join-Path $Paths.temp_dir "python-pycache")
    $processPath = [System.Environment]::GetEnvironmentVariable("PATH", "Process")
    if ([string]::IsNullOrWhiteSpace($processPath)) {
        Fail-Manifest "native command PATH is unavailable after bootstrap preparation"
    }
    $startInfo.Environment["PATH"] = "$python3Bin;$($gitUsrBin.directory);$processPath"
    foreach ($name in @("RUSTY_V8_ARCHIVE", "RUSTY_V8_SRC_BINDING_PATH")) {
        $value = [System.Environment]::GetEnvironmentVariable($name, "Process")
        if ([string]::IsNullOrEmpty($value)) {
            $null = $startInfo.Environment.Remove($name)
        } else {
            $startInfo.Environment[$name] = $value
        }
    }
    foreach ($name in @("RUSTY_V8_MIRROR", "V8_FROM_SOURCE")) {
        $null = $startInfo.Environment.Remove($name)
    }
    if ($isFixture) {
        $startInfo.Environment["CARGO_VALIDATE_WINDOWS_FAKE_ARGV_LOG"] = $argvLogPath
        $startInfo.Environment["CARGO_VALIDATE_WINDOWS_FAKE_ENV_LOG"] = $envLogPath
        $startInfo.Environment["CARGO_VALIDATE_WINDOWS_FAKE_EXIT_CODE"] = [string]$Approved.fixture.exit_code
        if ($createIgnoredFile) {
            $startInfo.Environment["CARGO_VALIDATE_WINDOWS_FAKE_CREATE_IGNORED"] = "1"
        }
    }

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $commandPreflight = Invoke-ExecutionPreflight $Runtime $Paths $TestFixture "before-approved-command-$($Approved.index)" $Yolo
    try {
        if (-not $process.Start()) {
            Fail-Manifest "direct cargo process did not start"
        }
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $stdout = $stdoutTask.GetAwaiter().GetResult()
        $stderr = $stderrTask.GetAwaiter().GetResult()
        $exitCode = $process.ExitCode
    } finally {
        $process.Dispose()
    }
    Write-TextEvidence $stdoutPath $stdout
    Write-TextEvidence $stderrPath $stderr
    if (
        $isFixture -and
        (-not [System.IO.File]::Exists($argvLogPath) -or -not [System.IO.File]::Exists($envLogPath))
    ) {
        Fail-Manifest "inert fake-cargo did not produce required argv/env evidence"
    }

    return [pscustomobject]@{
        index = $Approved.index
        command_id = $Approved.command_id
        argv = $Approved.argv
        launch_file_name = $startInfo.FileName
        launch_arguments = @($Approved.argv[1..($Approved.argv.Length - 1)])
        working_directory = $startInfo.WorkingDirectory
        status = if ($exitCode -eq 0) { "success" } else { "failed" }
        exit_code = $exitCode
        stdout_path = $stdoutPath
        stderr_path = $stderrPath
        launch_kind = if ($isFixture) { "test-only-fake-cargo" } else { "direct-toolchain-cargo" }
        direct_toolchain_cargo = $directCargo
        fake_tool_path = $fakeTool
        fake_argv_path = if ($isFixture) { $argvLogPath } else { $null }
        fake_env_path = if ($isFixture) { $envLogPath } else { $null }
        command_preflight = $commandPreflight
    }
}

try {
    $unboundArguments = Get-Variable -Name args -ValueOnly -ErrorAction SilentlyContinue
    if ($null -ne $unboundArguments -and @($unboundArguments).Count -ne 0) {
        Fail-Manifest "only -Manifest <path> is accepted"
    }
    $manifestPath = [System.IO.Path]::GetFullPath($Manifest)
    if (-not [System.IO.File]::Exists($manifestPath)) {
        Fail-Manifest "manifest does not exist: $manifestPath"
    }
    $manifestData = ConvertFrom-Json -InputObject ([System.IO.File]::ReadAllText($manifestPath, [System.Text.Encoding]::UTF8)) -AsHashtable -Depth 64 -NoEnumerate
    $manifestData = Get-RequiredMap $manifestData "manifest"

    # Validate the complete root/runtime contract, including the one canonical
    # workflow-namespace validator, before New-RunPaths can create any F: state.
    Assert-ManifestRoot $manifestData
    $runtime = Get-WindowsRuntime $manifestData
    $yolo = (Assert-StringArray $manifestData["flags"] "manifest.flags") -contains "yolo"
    $script:Runtime = $runtime
    $script:CacheRoot = $runtime.cache_root
    $script:RunPaths = New-RunPaths $runtime.workflow_namespace $runtime.reuse_run_root
    $script:Preflight = [ordered]@{
        schema = 2
        status = "started"
        cache_root = $script:CacheRoot
        paths = $script:RunPaths
        resource_contract = $runtime.resource_contract
        yolo = $yolo
        windows_runtime = [ordered]@{
            cache_root = $runtime.cache_root
            workflow_namespace = $runtime.workflow_namespace
            reuse_run_root = $runtime.reuse_run_root
            minimum_free_disk_gib = $runtime.minimum_free_disk_gib
            minimum_available_memory_gib = $runtime.minimum_available_memory_gib
            target = $runtime.target
            rust_toolchain = $runtime.rust_toolchain
            source_materialization = $runtime.source_materialization
        }
        native_mutex = [ordered]@{ name = $script:NativeMutexName; status = "not-required" }
        native_git = $null
        direct_toolchain = $null
        bootstrap = [ordered]@{
            status = "pending"
            source = $null
            direct_toolchain = $null
            error = $null
        }
        execution_preflight = $script:LivePreflightChecks
        candidate_materialization = [ordered]@{
            status = "not-required"
            materialized = $false
        }
        manifest_path = $manifestPath
        plan_id = $null
        validation_tooling_digest = $null
        candidate_identity = $null
        error = $null
    }
    Write-JsonEvidence (Join-Path $script:RunPaths.evidence_dir "preflight.json") $script:Preflight

    $testFixture = Get-TestOnlyPreflightFixture
    $bootstrapFixture = Get-TestOnlyBootstrapFixture
    $planId = Get-RequiredSha256 $manifestData["plan_id"] "plan_id"
    $toolingDigest = Get-RequiredSha256 $manifestData["validation_tooling_digest"] "validation_tooling_digest"
    $candidate = Get-CandidateIdentity $manifestData
    $script:Preflight.plan_id = $planId
    $script:Preflight.validation_tooling_digest = $toolingDigest
    $script:Preflight.candidate_identity = $candidate
    $commands = (Get-RequiredArray $manifestData["commands"] "commands").items
    $approved = [System.Collections.Generic.List[object]]::new()
    for ($index = 0; $index -lt $commands.Count; $index++) {
        $command = Get-RequiredMap $commands[$index] "commands[$index]"
        $tested = Test-ManifestCommand $command ($index + 1) $runtime
        if ($tested.disposition -eq "approved") {
            $approved.Add($tested)
        } else {
            $script:CommandResults.Add($tested)
        }
    }

    $reuseReceipt = $null
    if ($null -ne $runtime.reuse_run_root) {
        if (-not $candidate.is_bound) {
            Fail-Manifest "selected reuse working set requires a bound candidate identity"
        }
        $reuseReceipt = Get-ReuseReceipt $script:RunPaths
    }
    $hasProductionCommand = @($approved | Where-Object { $null -eq $_.fixture }).Count -ne 0
    $requiresBootstrap = $hasProductionCommand -or $null -ne $bootstrapFixture -or $null -ne $reuseReceipt
    if ($null -ne $bootstrapFixture -and $approved.Count -eq 0) {
        Fail-Manifest "test-only bootstrap fixtures require an approved Windows command"
    }
    if ($null -ne $bootstrapFixture -and $hasProductionCommand) {
        Fail-Manifest "test-only bootstrap fixtures cannot enable a production Windows command"
    }
    if ($requiresBootstrap -and -not $candidate.is_bound) {
        Fail-Manifest "pinned native Windows bootstrap requires a bound candidate identity"
    }
    if ($requiresBootstrap) {
        # Direct Rust toolchain discovery intentionally precedes Set-RunEnvironment:
        # the user-level C-drive Rustup tree is a read-only input, while every
        # command-visible Cargo/Rustup home remains below the F-drive run root.
        $directToolchain = Capture-DirectRustToolchain $runtime
        $script:DirectToolchain = $directToolchain
    } else {
        $directToolchain = $null
    }

    Set-RunEnvironment $script:RunPaths
    if ($null -ne $directToolchain) {
        Prepend-PathEntries @($directToolchain.toolchain_bin)
    }
    $script:Preflight.direct_toolchain = $directToolchain
    $script:Preflight.bootstrap = [ordered]@{
        status = if ($requiresBootstrap) { "pending" } else { "not-required" }
        source = if ($null -ne $reuseReceipt) { "reused" } elseif ($null -eq $bootstrapFixture) { $null } else { "test-only-fixture" }
        direct_toolchain = $directToolchain
        error = $null
    }
    Write-JsonEvidence (Join-Path $script:RunPaths.evidence_dir "preflight.json") $script:Preflight

    $materialization = $null
    if ($approved.Count -ne 0) {
        $script:Preflight.native_mutex = Enter-NativeExecutionMutex $testFixture
        $null = Invoke-ExecutionPreflight $runtime $script:RunPaths $testFixture "before-materialization-or-command" $yolo
        Write-JsonEvidence (Join-Path $script:RunPaths.evidence_dir "preflight.json") $script:Preflight
        if ($candidate.is_bound) {
            $script:Preflight.candidate_materialization = [ordered]@{
                status = "started"
                materialized = $false
            }
            $nativeGit = Resolve-NativeGit
            $script:NativeGit = $nativeGit
            $script:Preflight.native_git = $nativeGit
            Write-JsonEvidence (Join-Path $script:RunPaths.evidence_dir "preflight.json") $script:Preflight
            $materialization = Materialize-Candidate $runtime $candidate $script:RunPaths $nativeGit $reuseReceipt
            $script:Materialization = $materialization
            $script:Preflight.candidate_materialization = $materialization
        } else {
            $script:Preflight.candidate_materialization = [ordered]@{
                status = "all-null-candidate-test-fixture-only"
                materialized = $false
            }
        }
    }

    $script:Preflight.status = "accepted"
    Write-JsonEvidence (Join-Path $script:RunPaths.evidence_dir "preflight.json") $script:Preflight

    if ($approved.Count -eq 0) {
        $script:Status = "success"
        $script:ExitCode = 0
    } else {
        $script:Status = "success"
        $script:ExitCode = 0
        if ($requiresBootstrap) {
            if ($null -eq $materialization -or -not $materialization.materialized) {
                Fail-Manifest "pinned native Windows bootstrap requires a materialized candidate"
            }
            $script:Preflight.bootstrap = [ordered]@{
                status = "started"
                source = if ($null -ne $reuseReceipt) { "reused" } elseif ($null -eq $bootstrapFixture) { "native-https" } else { "test-only-fixture" }
                direct_toolchain = $directToolchain
                error = $null
            }
            Write-JsonEvidence (Join-Path $script:RunPaths.evidence_dir "preflight.json") $script:Preflight
            $bootstrap = Initialize-NativeWindowsBootstrap $runtime $script:RunPaths $materialization $directToolchain $bootstrapFixture $reuseReceipt
            $script:Bootstrap = $bootstrap
            $script:Preflight.bootstrap = $bootstrap
            Write-JsonEvidence (Join-Path $script:RunPaths.evidence_dir "preflight.json") $script:Preflight
        } else {
            $bootstrap = $null
        }
        foreach ($command in $approved) {
            $result = Invoke-ApprovedCommand $command $script:RunPaths $runtime $materialization $testFixture $bootstrap $yolo
            $script:CommandResults.Add($result)
            if ($null -ne $materialization) {
                $candidateAfterCommand = Assert-CandidateIntegrity $script:NativeGit.path $script:GitEnvironment $script:RunPaths $candidate $materialization.source_symlink_state "candidate-after-command-$($command.index)" -AllowIgnoredUntracked
                $materialization.candidate_after_command = Get-SnapshotSummary $candidateAfterCommand
                $script:Preflight.candidate_materialization = $materialization
            }
            if ($result.exit_code -ne 0) {
                $script:Status = "command-failed"
                $script:ExitCode = $result.exit_code
                break
            }
        }
    }
    Finalize-SourceIntegrity
} catch {
    $failure = $_.Exception.Message
    if (-not $script:SourceFinalized -and $null -ne $script:SourceSnapshotBefore) {
        try {
            Finalize-SourceIntegrity
        } catch {
            $failure = "$failure; source final integrity check failed: $($_.Exception.Message)"
        }
    }
    $script:Status = "preflight-failed"
    $script:ExitCode = 1
    $script:Failure = $failure
    if ($null -ne $script:Preflight) {
        $script:Preflight.status = "failed"
        $script:Preflight.error = $script:Failure
        if (
            $null -ne $script:Preflight.bootstrap -and
            $script:Preflight.bootstrap.status -in @("pending", "started")
        ) {
            $script:Preflight.bootstrap.status = "failed"
            $script:Preflight.bootstrap.error = $script:Failure
        }
    }
    [Console]::Error.WriteLine("cargo-validate-windows: $script:Failure")
} finally {
    try {
        if ($null -ne $script:RunPaths) {
            if ($null -ne $script:Preflight) {
                Write-JsonEvidence (Join-Path $script:RunPaths.evidence_dir "preflight.json") $script:Preflight
            }
            $result = [ordered]@{
                schema = 2
                status = $script:Status
                exit_code = $script:ExitCode
                cache_root = $script:CacheRoot
                paths = $script:RunPaths
                resource_contract = if ($null -eq $script:Runtime) { $null } else { $script:Runtime.resource_contract }
                bootstrap = if ($null -eq $script:Preflight) { $null } else { $script:Preflight.bootstrap }
                candidate_materialization = if ($null -eq $script:Preflight) { $null } else { $script:Preflight.candidate_materialization }
                manifest_path = if ($null -eq $script:Preflight) { $null } else { $script:Preflight.manifest_path }
                plan_id = if ($null -eq $script:Preflight) { $null } else { $script:Preflight.plan_id }
                validation_tooling_digest = if ($null -eq $script:Preflight) { $null } else { $script:Preflight.validation_tooling_digest }
                candidate_identity = if ($null -eq $script:Preflight) { $null } else { $script:Preflight.candidate_identity }
                command_results = $script:CommandResults.ToArray()
                error = $script:Failure
            }
            $resultPath = Join-Path $script:RunPaths.evidence_dir "result.json"
            Write-JsonEvidence $resultPath $result
            [Console]::Out.WriteLine((@{
                        schema = 1
                        status = $script:Status
                        exit_code = $script:ExitCode
                        evidence_dir = $script:RunPaths.evidence_dir
                        result_path = $resultPath
                    } | ConvertTo-Json -Compress))
        }
    } finally {
        Exit-NativeExecutionMutex
    }
}

exit $script:ExitCode
