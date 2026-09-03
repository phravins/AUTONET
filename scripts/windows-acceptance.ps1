#Requires -Version 5.1
<#
    Milestone 2b hardware acceptance: run AutoNet beside the operating system's
    own answer, under one named network condition, and keep everything.

      scripts\windows-acceptance.ps1 wifi
      scripts\windows-acceptance.ps1 ethernet
      scripts\windows-acceptance.ps1 both
      scripts\windows-acceptance.ps1 vpn

    Everything lands in acceptance\<scenario>\ (gitignored), plus one fixture in
    tests\fixtures\windows-real-<scenario>.json. Record the results in
    docs\milestone-2b-acceptance.md - the checklist is the deliverable; this
    script only gathers the evidence for it.

    Deliberately NOT $ErrorActionPreference = 'Stop', for the reason the macOS
    script gives at scripts/macos-acceptance.sh:16. A failing live test, a
    `no address` exit from `autonet ip`, an absent `netsh` - each of those is a
    *result*, and a run that aborts on the first one tells us less than a run
    that finishes and reports it. Every exit code is recorded in SUMMARY.txt.

    Windows PowerShell 5.1, which ships in-box. Requiring PowerShell 7 would make
    the acceptance run depend on an install, and every cmdlet used here
    (Get-NetAdapter, Get-NetRoute, Get-NetIPInterface, ConvertFrom-Json) is
    present in 5.1 already.

    Deliberately no jq, because Windows does not ship it and ConvertFrom-Json is
    built in. The human-readable tables are the at-a-glance view; the --json
    files are what sections 50 and 51 actually compute against.

    If PowerShell will not run an unsigned script:
      powershell -ExecutionPolicy Bypass -File scripts\windows-acceptance.ps1 wifi
#>

[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('wifi', 'ethernet', 'both', 'vpn')]
    [string]$Scenario
)

$ErrorActionPreference = 'Continue'

if (-not $Scenario) {
    Write-Host 'usage: scripts\windows-acceptance.ps1 <wifi|ethernet|both|vpn>'
    Write-Host ''
    Write-Host '  wifi      Wi-Fi associated, nothing else up'
    Write-Host '  ethernet  a wire, Wi-Fi off'
    Write-Host '  both      Wi-Fi and Ethernet up at once'
    Write-Host '  vpn       a tunnel up over whatever else is connected'
    exit 64
}

$repo = Split-Path -Parent $PSScriptRoot
$out = Join-Path $repo "acceptance\$Scenario"
$fixture = Join-Path $repo "tests\fixtures\windows-real-$Scenario.json"
$summary = Join-Path $out 'SUMMARY.txt'

New-Item -ItemType Directory -Force -Path $out | Out-Null
Set-Location $repo

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Names and exit codes, in order, for SUMMARY.txt.
$script:StepNames = New-Object System.Collections.ArrayList
$script:StepCodes = New-Object System.Collections.ArrayList

# UTF-8 with no BOM. Windows PowerShell's `-Encoding UTF8` writes one, and a BOM
# at the head of tests\fixtures\windows-real-*.json makes serde_json reject the
# whole file - which would look like a capture bug rather than an encoding one.
function Write-TextFile {
    param([string]$Path, [string]$Text)
    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, $Text, $utf8NoBom)
}

function Add-Step {
    param([string]$Name, $Code)
    [void]$script:StepNames.Add($Name)
    [void]$script:StepCodes.Add("$Code")
    '  {0,-34} {1}' -f $Name, $Code | Write-Host
}

# Run an external command, tee its combined output to a file, and remember what
# it returned. Combined, because a backend error on stderr belongs beside the
# stdout it failed to produce.
function Invoke-Recorded {
    param([string]$File, [string]$Name, [string]$Exe, [string[]]$Arguments = @())

    $header = "`$ $Exe $($Arguments -join ' ')`r`n`r`n"
    $body = ''
    try {
        $body = (& $Exe @Arguments 2>&1 | Out-String)
        $code = $LASTEXITCODE
    }
    catch {
        $body = "$_"
        $code = 'threw'
    }
    Write-TextFile (Join-Path $out $File) ($header + $body)
    Add-Step $Name $code
    return $code
}

# Same, for the operating system's own tools: absent is a note, not a failure.
# It is what lets this script be smoke-tested on a machine that has none of them
# - a run there proves the plumbing works and nothing whatsoever about Windows.
function Invoke-RecordedOs {
    param([string]$File, [string]$Name, [string]$Exe, [string[]]$Arguments = @())

    if (-not (Get-Command $Exe -ErrorAction SilentlyContinue)) {
        Write-TextFile (Join-Path $out $File) "$Exe`: not available on this system`r`n"
        Add-Step $Name 'n/a'
        return
    }
    [void](Invoke-Recorded $File $Name $Exe $Arguments)
}

# Cmdlets rather than executables: there is no exit code, so success is "it did
# not throw". Absent cmdlet is a note, exactly as above.
function Invoke-RecordedCmdlet {
    param([string]$File, [string]$Name, [string]$Requires, [scriptblock]$Body)

    if ($Requires -and -not (Get-Command $Requires -ErrorAction SilentlyContinue)) {
        Write-TextFile (Join-Path $out $File) "$Requires`: not available on this system`r`n"
        Add-Step $Name 'n/a'
        return
    }

    $header = "# $Name`r`n`r`n"
    try {
        $body = (& $Body 2>&1 | Out-String)
        Write-TextFile (Join-Path $out $File) ($header + $body)
        Add-Step $Name 0
    }
    catch {
        Write-TextFile (Join-Path $out $File) ($header + "$_`r`n")
        Add-Step $Name 'threw'
    }
}

function Write-Section {
    param([string]$Title)
    Write-Host ''
    Write-Host "=== $Title ==="
}

# Read a property that may not exist on this build of Windows without turning a
# missing field into a crash. A '?' in the table is information; a stack trace
# in the middle of an acceptance run is not.
function Get-Prop {
    param($Object, [string]$Name)
    if ($null -eq $Object) { return '?' }
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property -or $null -eq $property.Value) { return '?' }
    return $property.Value
}

# `kind` is a string for every variant except Other, which is externally tagged
# and arrives as {"other": "..."}. Rendered as `other:virtual-ethernet` so the
# TAP-VPN case is visible at a glance rather than printing as a bare object.
function Format-Kind {
    param($Kind)
    if ($null -eq $Kind) { return '?' }
    if ($Kind -is [string]) { return $Kind }
    $other = $Kind.PSObject.Properties['other']
    if ($other) { return "other:$($other.Value)" }
    return "$Kind"
}

function Read-AutonetJson {
    param([string]$File)
    $path = Join-Path $out $File
    if (-not (Test-Path $path)) { return $null }
    try {
        # Skip the `$ command` header Invoke-Recorded writes, then parse.
        $text = (Get-Content $path -Raw)
        $brace = $text.IndexOf('{')
        if ($brace -lt 0) { return $null }
        return ($text.Substring($brace) | ConvertFrom-Json)
    }
    catch {
        return $null
    }
}

# ---------------------------------------------------------------------------
# 00 - what machine, what tree
# ---------------------------------------------------------------------------

$isWindowsHost = ($env:OS -eq 'Windows_NT')

$environment = @()
$environment += "scenario:   $Scenario"
$environment += "date:       $((Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'))"
$environment += "os:         $([System.Environment]::OSVersion.VersionString)"
$environment += "powershell: $($PSVersionTable.PSVersion) ($($PSVersionTable.PSEdition))"
$environment += "commit:     $((& git rev-parse HEAD 2>$null) -join '')"
$dirty = (& git status --porcelain 2>$null)
$environment += "dirty:      $(if ($dirty) { 'yes' } else { 'no' })"
if (Get-Command rustc -ErrorAction SilentlyContinue) {
    $environment += "rustc:      $((& rustc --version) -join '')"
}
$environmentText = ($environment -join "`r`n") + "`r`n"
Write-TextFile (Join-Path $out '00-environment.txt') $environmentText

Write-Host "AutoNet acceptance - scenario '$Scenario' - output in acceptance\$Scenario\"
Write-Host $environmentText

if (-not $isWindowsHost) {
    Write-Host ''
    Write-Host '!!  NOT Windows.'
    Write-Host '!!  This run exercises the script, not the Windows backend. Nothing it'
    Write-Host '!!  produces is evidence for Milestone 2b. No fixture will be written.'
}

# ---------------------------------------------------------------------------
# 10 - the operating system's own answer, captured first
# ---------------------------------------------------------------------------
# First, so that if the network changes mid-run the OS view is the one taken
# *before* AutoNet's, and a disagreement can be read as "the network moved"
# rather than silently blamed on the backend.
#
# Both the cmdlets and the legacy tools, because they are not redundant: the
# cmdlets return typed objects that sections 50 and 51 can compute against,
# while `route print` and `netsh` are the output a human reads in a bug report
# and the form every Microsoft answer is written in.

Write-Section "the OS's own view"

Invoke-RecordedCmdlet '10-os-get-netadapter.txt' 'Get-NetAdapter (all fields)' 'Get-NetAdapter' {
    Get-NetAdapter -IncludeHidden | Format-List *
}
Invoke-RecordedCmdlet '11-os-get-netipaddress.txt' 'Get-NetIPAddress' 'Get-NetIPAddress' {
    Get-NetIPAddress | Sort-Object InterfaceIndex, AddressFamily |
        Format-Table InterfaceIndex, InterfaceAlias, AddressFamily, IPAddress,
                     PrefixLength, PrefixOrigin, SuffixOrigin, AddressState -AutoSize
}
Invoke-RecordedCmdlet '12-os-get-netipinterface.txt' 'Get-NetIPInterface' 'Get-NetIPInterface' {
    Get-NetIPInterface | Sort-Object InterfaceIndex, AddressFamily |
        Format-Table InterfaceIndex, InterfaceAlias, AddressFamily, InterfaceMetric,
                     AutomaticMetric, ConnectionState, Dhcp, NlMtu -AutoSize
}
Invoke-RecordedCmdlet '13-os-get-netroute.txt' 'Get-NetRoute' 'Get-NetRoute' {
    Get-NetRoute | Sort-Object AddressFamily, InterfaceIndex, DestinationPrefix |
        Format-Table InterfaceIndex, InterfaceAlias, AddressFamily, DestinationPrefix,
                     NextHop, RouteMetric, Publish, Store -AutoSize
}
Invoke-RecordedOs '14-os-route-print.txt' 'route print' 'route.exe' @('print')
Invoke-RecordedOs '15-os-netsh-ipv4.txt' 'netsh ipv4 show interfaces' 'netsh.exe' `
    @('interface', 'ipv4', 'show', 'interfaces')
Invoke-RecordedOs '16-os-netsh-ipv6.txt' 'netsh ipv6 show interfaces' 'netsh.exe' `
    @('interface', 'ipv6', 'show', 'interfaces')
Invoke-RecordedOs '17-os-ipconfig.txt' 'ipconfig /all' 'ipconfig.exe' @('/all')

# ---------------------------------------------------------------------------
# 20 - AutoNet
# ---------------------------------------------------------------------------
# Built once and invoked as a binary, so that --json output is exactly the
# document and not a document with cargo's progress lines in front of it. This
# is the same set of commands the brief asks for as `cargo run -p autonet-cli --`
# and produces identical output with nothing prepended.

Write-Section 'building'
& cargo build --release -p autonet-cli
if ($LASTEXITCODE -ne 0) {
    Write-Host 'the CLI did not build - stopping, since nothing below would mean anything'
    exit 1
}
$autonet = Join-Path $repo 'target\release\autonet.exe'
if (-not (Test-Path $autonet)) {
    $autonet = Join-Path $repo 'target\release\autonet'
}

Write-Section "AutoNet's answer"
[void](Invoke-Recorded '20-autonet-status.txt' 'status' $autonet @('status'))
[void](Invoke-Recorded '21-autonet-status.json' 'status --json -v' $autonet @('status', '--json', '-v'))
[void](Invoke-Recorded '22-autonet-ip.txt' 'ip' $autonet @('ip'))
[void](Invoke-Recorded '23-autonet-ip.json' 'ip --json' $autonet @('ip', '--json'))
[void](Invoke-Recorded '24-autonet-interfaces.txt' 'interfaces -v' $autonet @('interfaces', '-v'))
[void](Invoke-Recorded '25-autonet-interfaces.json' 'interfaces --json -v' $autonet @('interfaces', '--json', '-v'))
[void](Invoke-Recorded '26-autonet-routes.txt' 'routes -v' $autonet @('routes', '-v'))
[void](Invoke-Recorded '27-autonet-routes.json' 'routes --json -v' $autonet @('routes', '--json', '-v'))

# ---------------------------------------------------------------------------
# 30 - the live tests
# ---------------------------------------------------------------------------
# --ignored runs exactly the live set; --nocapture is not optional here. Several
# of these tests skip themselves rather than assume a configuration - a machine
# with IPv6 off, one NIC or no tunnel is a correctly-configured machine - and
# the `skipped:` lines they print are the only way to tell "verified" from "had
# nothing to look at". A skip is not a pass.

Write-Section 'live tests (the whole point of the exercise)'
[void](Invoke-Recorded '30-live-tests.txt' 'cargo test -- --ignored' 'cargo' `
    @('test', '-p', 'autonet-platform', '--', '--ignored', '--nocapture'))

# ---------------------------------------------------------------------------
# 40 - the capture
# ---------------------------------------------------------------------------

Write-Section 'capture'
if ($isWindowsHost) {
    $capturePath = Join-Path $out '40-capture.json'
    $captureErrPath = Join-Path $out '40-capture.err'

    # Start-Process rather than a pipeline, for one reason: it writes the child's
    # bytes straight to the file. A pipeline would decode and re-encode them, and
    # Windows PowerShell's default re-encoding is UTF-16 with a BOM, which
    # serde_json rejects - a fixture that fails to parse would look like a
    # capture bug rather than an encoding one. It also separates cargo's build
    # chatter on stderr from the JSON on stdout without any filtering.
    $captureCode = 1
    try {
        $process = Start-Process -FilePath 'cargo' -PassThru -Wait -NoNewWindow `
            -WorkingDirectory $repo `
            -ArgumentList @('run', '-q', '-p', 'autonet-platform', '--example', 'capture') `
            -RedirectStandardOutput $capturePath -RedirectStandardError $captureErrPath
        $captureCode = $process.ExitCode
    }
    catch {
        Write-TextFile $captureErrPath "$_`r`n"
    }

    $captured = if (Test-Path $capturePath) { (Get-Content $capturePath -Raw) } else { '' }

    if ($captureCode -eq 0 -and $captured -and $captured.Trim()) {
        Copy-Item -Path $capturePath -Destination $fixture -Force
        Add-Step "capture -> windows-real-$Scenario.json" 0
        Write-Host "  wrote tests\fixtures\windows-real-$Scenario.json"
        Write-Host '  NOTE: a committed capture publishes this machine''s adapter names,'
        Write-Host '        addresses and prefixes. MACs are stripped; addresses are not,'
        Write-Host '        because they are what the fixture is for. Read it before'
        Write-Host '        committing it.'
    }
    else {
        Add-Step 'capture' 'failed'
        Write-Host "  capture FAILED - see acceptance\$Scenario\40-capture.err"
    }
}
else {
    Write-Host '  skipped: a capture from a non-Windows host would be mislabelled as windows-real-*'
}

# ---------------------------------------------------------------------------
# 50 - the metric cross-check: is MIB_IPFORWARD_ROW2.Metric meaningful?
# ---------------------------------------------------------------------------
# Milestone 2b's open question 1, and the only thing that can settle it. No live
# test can: `Route.metric` is already the sum `winroute::effective_metric`
# produced, the model carries no separate interface-metric field, and both
# candidate readings produce a plausible number.
#
# Windows' own answer is two typed objects, so this parses nothing and invents
# no parser of its own to be wrong about. `Get-NetRoute` supplies RouteMetric,
# `Get-NetIPInterface` supplies InterfaceMetric, and AutoNet's routes --json
# supplies what the backend made of them. The verdict is arithmetic.

Write-Section 'cross-check 1: route metrics'

$metricLines = @()
$metricLines += 'Does autonet''s metric equal RouteMetric + InterfaceMetric?'
$metricLines += ''
$metricLines += '  sum        row.Metric is an offset, as Task 4 assumed - assumption holds'
$metricLines += '  route      the interface metric never applied - TASK 4 finding'
$metricLines += '  interface  row.Metric is inert, macOS rmx_hopcount again - TASK 4 finding'
$metricLines += '  ?          none of the three - report this table verbatim'
$metricLines += ''

$routesJson = Read-AutonetJson '27-autonet-routes.json'
$haveNetRoute = [bool](Get-Command Get-NetRoute -ErrorAction SilentlyContinue)
$haveNetIf = [bool](Get-Command Get-NetIPInterface -ErrorAction SilentlyContinue)

if (-not $routesJson -or -not $routesJson.routes) {
    $metricLines += 'skipped: acceptance\27-autonet-routes.json carried no routes to check.'
}
elseif (-not $haveNetRoute -or -not $haveNetIf) {
    $metricLines += 'skipped: Get-NetRoute / Get-NetIPInterface are not available on this system.'
}
else {
    $netRoutes = @(Get-NetRoute -ErrorAction SilentlyContinue)
    $netIfs = @(Get-NetIPInterface -ErrorAction SilentlyContinue)

    $metricLines += ('{0,-34} {1,-5} {2,6} {3,12} {4,9} {5,6} {6,8}  {7}' -f `
            'Dest', 'Fam', 'IfIdx', 'RouteMetric', 'IfMetric', 'Sum', 'autonet', 'verdict')
    $metricLines += ('-' * 100)

    $verdicts = @{}
    foreach ($route in $routesJson.routes) {
        $family = "$($route.family)"
        $wildcard = if ($family -eq 'ipv6') { '::/0' } else { '0.0.0.0/0' }
        # AutoNet emits null for a default route; Windows spells it as the
        # wildcard prefix. Same route, two vocabularies.
        $dest = if ($null -eq $route.destination) { $wildcard } else { "$($route.destination)" }
        $index = [int]$route.interface_index
        $winFamily = if ($family -eq 'ipv6') { 'IPv6' } else { 'IPv4' }

        $matched = @($netRoutes | Where-Object {
                $_.InterfaceIndex -eq $index -and
                "$($_.AddressFamily)" -eq $winFamily -and
                "$($_.DestinationPrefix)" -eq $dest
            })
        $ifMatched = @($netIfs | Where-Object {
                $_.InterfaceIndex -eq $index -and "$($_.AddressFamily)" -eq $winFamily
            })

        $routeMetrics = @($matched | ForEach-Object { [int]$_.RouteMetric } | Sort-Object -Unique)
        $ifMetric = if ($ifMatched.Count -gt 0) { [int]$ifMatched[0].InterfaceMetric } else { $null }
        $autonetMetric = [int]$route.metric

        $verdict = '?'
        if ($routeMetrics.Count -eq 0 -or $null -eq $ifMetric) {
            $verdict = 'unmatched'
        }
        elseif ($routeMetrics.Count -gt 1) {
            # Two Windows rows for one destination on one interface: the sum is
            # not a single number, so this row cannot vote. Reported, not hidden.
            $verdict = 'ambiguous'
        }
        elseif ($autonetMetric -eq ($routeMetrics[0] + $ifMetric)) { $verdict = 'sum' }
        elseif ($autonetMetric -eq $routeMetrics[0]) { $verdict = 'route' }
        elseif ($autonetMetric -eq $ifMetric) { $verdict = 'interface' }

        if (-not $verdicts.ContainsKey($verdict)) { $verdicts[$verdict] = 0 }
        $verdicts[$verdict] = $verdicts[$verdict] + 1

        $routeText = if ($routeMetrics.Count -eq 0) { '-' } else { ($routeMetrics -join ',') }
        $ifText = if ($null -eq $ifMetric) { '-' } else { "$ifMetric" }
        $sumText = if ($routeMetrics.Count -eq 1 -and $null -ne $ifMetric) {
            "$($routeMetrics[0] + $ifMetric)"
        }
        else { '-' }

        $metricLines += ('{0,-34} {1,-5} {2,6} {3,12} {4,9} {5,6} {6,8}  {7}' -f `
                $dest, $family, $index, $routeText, $ifText, $sumText, $autonetMetric, $verdict)
    }

    $metricLines += ''
    $metricLines += '--- verdict ---'
    foreach ($key in ($verdicts.Keys | Sort-Object)) {
        $metricLines += ('  {0,-10} {1} route(s)' -f $key, $verdicts[$key])
    }
    $metricLines += ''
    $metricLines += 'The answer to open question 1 is whichever verdict the routes agree on.'
    $metricLines += 'A split between `sum` and `route` is itself a finding: the join reached'
    $metricLines += 'some adapters and missed others.'
}

Write-TextFile (Join-Path $out '50-metric-crosscheck.txt') (($metricLines -join "`r`n") + "`r`n")
Add-Step 'metric cross-check' 0

# ---------------------------------------------------------------------------
# 51 - the classification cross-check: what did the classifier actually see?
# ---------------------------------------------------------------------------
# Open questions 2 and 3. Get-NetAdapter exposes very nearly the exact evidence
# set `wintype::classify` consumes - InterfaceType, NdisPhysicalMedium,
# MediaType, HardwareInterface, Virtual - so putting them beside AutoNet's
# answer lands any misclassification on a named rung of the ladder instead of
# needing a debugger on a machine we do not have.
#
# The InterfaceMetric columns are also the raw data the brief's step 3 asks for:
# the actual per-adapter Ipv4Metric/Ipv6Metric values Windows reports, in one
# place, so the tie-break margin is arithmetic rather than recollection.

Write-Section 'cross-check 2: interface classification and metrics'

$kindLines = @()
$kindLines += 'What Windows reports about each adapter, beside what autonet made of it.'
$kindLines += ''
$kindLines += 'wintype::classify consults, in order: loopback, wireless (IfType 71 or an'
$kindLines += '802.11 NDIS medium), declared tunnel, WWAN/WiMAX, then the Ethernet family'
$kindLines += 'where AccessType == POINT_TO_POINT gives vpn, HardwareInterface == False'
$kindLines += 'gives other:virtual-ethernet, and anything else gives ethernet.'
$kindLines += ''
$kindLines += 'IfType 6 = Ethernet, 24 = loopback, 71 = 802.11, 131 = tunnel, 23 = PPP.'
$kindLines += ''

$interfacesJson = Read-AutonetJson '25-autonet-interfaces.json'
$haveNetAdapter = [bool](Get-Command Get-NetAdapter -ErrorAction SilentlyContinue)

if (-not $interfacesJson -or -not $interfacesJson.interfaces) {
    $kindLines += 'skipped: acceptance\25-autonet-interfaces.json carried no interfaces to check.'
}
else {
    $adapters = if ($haveNetAdapter) {
        @(Get-NetAdapter -IncludeHidden -ErrorAction SilentlyContinue)
    }
    else { @() }
    $netIfs = if ($haveNetIf) { @(Get-NetIPInterface -ErrorAction SilentlyContinue) } else { @() }

    if (-not $haveNetAdapter) {
        $kindLines += 'note: Get-NetAdapter is unavailable, so the Windows-reported columns are all `?`.'
        $kindLines += ''
    }

    $kindLines += ('{0,-22} {1,6} {2,7} {3,8} {4,6} {5,6} {6,5} {7,6} {8,6}  {9}' -f `
            'Name', 'IfIdx', 'IfType', 'NdisPhys', 'Media', 'HwIf', 'Virt', 'v4Met', 'v6Met', 'autonet kind')
    $kindLines += ('-' * 118)

    foreach ($interface in ($interfacesJson.interfaces | Sort-Object index)) {
        $index = [int]$interface.index
        # Compared, not cast: Get-Prop yields '?' for a field this build of
        # Windows does not expose, and [int]'?' throws mid-Where-Object.
        $adapter = $adapters | Where-Object { $_.ifIndex -eq $index } | Select-Object -First 1

        $v4 = $netIfs | Where-Object {
            $_.InterfaceIndex -eq $index -and "$($_.AddressFamily)" -eq 'IPv4'
        } | Select-Object -First 1
        $v6 = $netIfs | Where-Object {
            $_.InterfaceIndex -eq $index -and "$($_.AddressFamily)" -eq 'IPv6'
        } | Select-Object -First 1

        $kindLines += ('{0,-22} {1,6} {2,7} {3,8} {4,6} {5,6} {6,5} {7,6} {8,6}  {9}' -f `
                "$($interface.name)",
            $index,
            (Get-Prop $adapter 'InterfaceType'),
            (Get-Prop $adapter 'NdisPhysicalMedium'),
            (Get-Prop $adapter 'MediaType'),
            (Get-Prop $adapter 'HardwareInterface'),
            (Get-Prop $adapter 'Virtual'),
            (Get-Prop $v4 'InterfaceMetric'),
            (Get-Prop $v6 'InterfaceMetric'),
            (Format-Kind $interface.kind))
    }

    $kindLines += ''
    $kindLines += '--- adapter descriptions, for the write-up only ---'
    $kindLines += 'Recorded so a human can say which row is the VPN. AutoNet never reads them:'
    $kindLines += 'matching "TAP-" or "WireGuard" classifies today''s VPNs and misses tomorrow''s.'
    $kindLines += ''
    foreach ($adapter in ($adapters | Sort-Object ifIndex)) {
        $kindLines += ('  {0,6}  {1,-22}  {2}' -f `
            (Get-Prop $adapter 'ifIndex'), (Get-Prop $adapter 'Name'), (Get-Prop $adapter 'InterfaceDescription'))
    }
}

Write-TextFile (Join-Path $out '51-kind-crosscheck.txt') (($kindLines -join "`r`n") + "`r`n")
Add-Step 'classification cross-check' 0

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

function Get-Evidence {
    param([string]$File)
    $path = Join-Path $out $File
    if (Test-Path $path) { return (Get-Content $path -Raw) }
    return "(not captured)`r`n"
}

$summaryLines = @()
$summaryLines += "AutoNet Milestone 2b acceptance - scenario '$Scenario'"
$summaryLines += ''
$summaryLines += $environmentText
$summaryLines += '--- exit codes ---'
for ($i = 0; $i -lt $script:StepNames.Count; $i++) {
    $summaryLines += ('{0,-38} {1}' -f $script:StepNames[$i], $script:StepCodes[$i])
}
$summaryLines += ''
$summaryLines += '--- side by side: what each one says the machine looks like ---'
$summaryLines += ''
$summaryLines += '### autonet interfaces -v'
$summaryLines += (Get-Evidence '24-autonet-interfaces.txt')
$summaryLines += '### autonet routes -v'
$summaryLines += (Get-Evidence '26-autonet-routes.txt')
$summaryLines += '### autonet ip'
$summaryLines += (Get-Evidence '22-autonet-ip.txt')
$summaryLines += '### netsh interface ipv4 show interfaces'
$summaryLines += (Get-Evidence '15-os-netsh-ipv4.txt')
$summaryLines += '### Get-NetRoute'
$summaryLines += (Get-Evidence '13-os-get-netroute.txt')
$summaryLines += ''
$summaryLines += '--- cross-check 1: route metrics (open question 1) ---'
$summaryLines += (Get-Evidence '50-metric-crosscheck.txt')
$summaryLines += '--- cross-check 2: classification and metrics (open questions 2 and 3) ---'
$summaryLines += (Get-Evidence '51-kind-crosscheck.txt')
$summaryLines += '--- live test results ---'
$summaryLines += 'A `skipped:` line is not a pass. It says the machine lacked the condition.'
$summaryLines += ''

$livePath = Join-Path $out '30-live-tests.txt'
if (Test-Path $livePath) {
    $lines = @(Select-String -Path $livePath -Pattern '^test |^running |result:|skipped:|note:|checked ' |
            ForEach-Object { $_.Line })
    if ($lines.Count -gt 0) { $summaryLines += $lines }
    else { $summaryLines += '(no test output captured)' }
}
else {
    $summaryLines += '(no test output captured)'
}

Write-TextFile $summary (($summaryLines -join "`r`n") + "`r`n")

Write-Section 'done'
Write-Host "Full write-up: acceptance\$Scenario\SUMMARY.txt"
Write-Host "Now fill in the '$Scenario' column of docs\milestone-2b-acceptance.md."
Write-Host ''
Write-Host 'Anything wrong is a finding against Task 2, 3 or 4 - the doc''s'
Write-Host 'trace-to-task table says which. Do not fix it here.'
