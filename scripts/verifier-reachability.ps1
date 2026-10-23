param(
    [string]$Workflows = '',
    [string]$ScriptsRoot = 'scripts',
    [string]$ExtraEntries = ''
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$workflowList = @($Workflows -split ',' | Where-Object { $_ })
$extraList = @($ExtraEntries -split ',' | Where-Object { $_ })

$astCache = @{}
function Read-ScriptAst([string]$Path) {
    $full = [IO.Path]::GetFullPath($Path)
    if (-not $astCache.ContainsKey($full)) {
        $tokens = $null; $errors = $null
        $ast = [Management.Automation.Language.Parser]::ParseFile($full, [ref]$tokens, [ref]$errors)
        if ($errors.Count) { throw "Parse error in $full" }
        $astCache[$full] = $ast
    }
    return $astCache[$full]
}
function Get-Definitions($Ast) { @($Ast.FindAll({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] }, $true)) }
function Get-CommandNames($Ast) { @($Ast.FindAll({ param($n) $n -is [Management.Automation.Language.CommandAst] }, $true) | ForEach-Object { $_.GetCommandName() } | Where-Object { $_ }) }
function Get-Relative([string]$Full) { $root = [IO.Path]::GetFullPath($ScriptsRoot).TrimEnd('\') + '\'; if ($Full.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) { return $Full.Substring($root.Length).Replace('\', '/') } return $Full }
function Get-ScriptReferences([string]$Path, [string]$Kind) {
    $text = (Read-ScriptAst $Path).Extent.Text
    $pattern = if ($Kind -ceq 'dot') { "(?m)^\s*\.\s+\(Join-Path\s+\`$PSScriptRoot\s+'([^']+\.ps1)'\)" } else { "&\s+\(Join-Path\s+\`$PSScriptRoot\s+'([^']+\.ps1)'\)" }
    @([regex]::Matches($text, $pattern) | ForEach-Object { [IO.Path]::GetFullPath((Join-Path (Split-Path -Parent $Path) $_.Groups[1].Value)) })
}

$repoFunctions = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($file in Get-ChildItem -LiteralPath $ScriptsRoot -Recurse -Filter '*.ps1') { foreach ($d in Get-Definitions (Read-ScriptAst $file.FullName)) { $null = $repoFunctions.Add($d.Name) } }

$entries = [Collections.Generic.List[string]]::new()
foreach ($workflow in $workflowList) {
    foreach ($m in [regex]::Matches((Get-Content -LiteralPath $workflow -Raw), '\./scripts/([A-Za-z0-9/_.-]+\.ps1)')) { $entries.Add([IO.Path]::GetFullPath((Join-Path $ScriptsRoot $m.Groups[1].Value))) }
}
foreach ($extra in $extraList) { $entries.Add([IO.Path]::GetFullPath((Join-Path $ScriptsRoot $extra))) }

$problems = [Collections.Generic.List[string]]::new()
$seenEntries = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
$checkedFiles = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
$queue = [Collections.Generic.Queue[string]]::new()
foreach ($e in $entries) { $queue.Enqueue($e) }
while ($queue.Count) {
    $entry = $queue.Dequeue()
    if (-not $seenEntries.Add($entry)) { continue }
    if (-not (Test-Path -LiteralPath $entry -PathType Leaf)) { $problems.Add("missing script $(Get-Relative $entry)"); continue }
    $available = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $contextFiles = [Collections.Generic.List[string]]::new()
    $pending = [Collections.Generic.Queue[string]]::new(); $pending.Enqueue($entry)
    $visited = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    while ($pending.Count) {
        $file = $pending.Dequeue()
        if (-not $visited.Add($file)) { continue }
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { $problems.Add("$(Get-Relative $entry) dot-sources missing $(Get-Relative $file)"); continue }
        $contextFiles.Add($file)
        foreach ($d in Get-Definitions (Read-ScriptAst $file)) { $null = $available.Add($d.Name) }
        foreach ($dot in Get-ScriptReferences $file 'dot') { $pending.Enqueue($dot) }
        foreach ($child in Get-ScriptReferences $file 'call') { $queue.Enqueue($child) }
    }
    $needed = [Collections.Generic.List[object]]::new()
    foreach ($file in $contextFiles) { foreach ($name in Get-CommandNames (Read-ScriptAst $file)) { $needed.Add([pscustomobject]@{ Name = $name; From = Get-Relative $file }) } }
    $entryText = (Read-ScriptAst $entry).Extent.Text
    foreach ($import in [regex]::Matches($entryText, "(?m)^Import-[A-Za-z]+Functions\s+\(Join-Path\s+\`$PSScriptRoot\s+'([^']+\.ps1)'\)\s+@\(([^)]*)\)")) {
        $source = [IO.Path]::GetFullPath((Join-Path (Split-Path -Parent $entry) $import.Groups[1].Value))
        $sourceAst = Read-ScriptAst $source
        foreach ($name in @([regex]::Matches($import.Groups[2].Value, "'([^']+)'") | ForEach-Object { $_.Groups[1].Value })) {
            $null = $available.Add($name)
            $definition = @(Get-Definitions $sourceAst | Where-Object { $_.Name -ceq $name })
            if ($definition.Count -ne 1) { $problems.Add("$(Get-Relative $entry) imports $name, which $(Get-Relative $source) does not define exactly once"); continue }
            foreach ($nested in Get-Definitions $definition[0].Body) { $null = $available.Add($nested.Name) }
            foreach ($called in Get-CommandNames $definition[0].Body) { $needed.Add([pscustomobject]@{ Name = $called; From = "$name in $(Get-Relative $source)" }) }
        }
    }
    foreach ($item in $needed) {
        if ($repoFunctions.Contains($item.Name) -and -not $available.Contains($item.Name)) { $problems.Add("$(Get-Relative $entry) cannot resolve $($item.Name), called by $($item.From)") }
    }
    foreach ($file in $contextFiles) {
        if (-not $checkedFiles.Add($file)) { continue }
        foreach ($closure in (Read-ScriptAst $file).FindAll({ param($n) $n -is [Management.Automation.Language.InvokeMemberExpressionAst] -and $n.Member.Value -eq 'GetNewClosure' }, $true)) {
            $calls = @(Get-CommandNames $closure.Expression | Where-Object { $repoFunctions.Contains($_) })
            if ($calls.Count) { $problems.Add("$(Get-Relative $file):$($closure.Extent.StartLineNumber) GetNewClosure() scriptblock calls script functions it cannot see when the verifier runs as a script: $(($calls | Sort-Object -Unique) -join ', ')") }
        }
    }
}
$problems | Sort-Object -Unique | ForEach-Object { Write-Output "PROBLEM $_" }
Write-Output ("ENTRIES {0}" -f (($seenEntries | ForEach-Object { Get-Relative $_ } | Sort-Object) -join ', '))
Write-Output 'ANALYSIS DONE'
