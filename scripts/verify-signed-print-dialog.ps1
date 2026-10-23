[CmdletBinding()]
param(
    [switch]$PreflightOnly,
    [string]$SignedArtifactRoot,
    [string]$WorkRoot,
    [string]$OutputRoot,
    [string]$WebDriverRoot,
    [string]$WebViewProfileRoot
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'ocr/installer-package.ps1')
. (Join-Path $PSScriptRoot 'ocr/windows-signing.ps1')
. (Join-Path $PSScriptRoot 'installed-app-launch.ps1')
. (Join-Path $PSScriptRoot 'installed-print-dialog.ps1')

# Import the already-reviewed installer verifier functions without executing its workflow body.
$trustedVerifier = Join-Path $PSScriptRoot 'verify-signed-ocr-upgrade.ps1'
$tokens = $null; $parseErrors = $null
$trustedAst = [Management.Automation.Language.Parser]::ParseFile($trustedVerifier,[ref]$tokens,[ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw 'The trusted signed-installer verifier did not parse.' }
foreach ($definition in @($trustedAst.FindAll({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] },$true))) {
    Invoke-Expression $definition.Extent.Text
}

$script:Pins = [ordered]@{
    Repository = 'Joshua-Beel/PDF-Workstation'
    SignedArtifactId = [uint64]12510717103
    SignedRunId = [uint64]39969839829
    SignedSourceRevision = '4a0f89c5b2f04004c77adbdd7ccee8442dd1c41d'
    SignedInstallerName = 'PDF Workstation_0.3.0_x64-setup.exe'
    SignedInstallerBytes = [uint64]11201296
    SignedInstallerSha256 = '3DE99F111652727BB59F9C17FF4E3CCA42ED48E9DE5E0614920280FFB0BA9FE5'
    SignedRecordName = 'artifact-verification.json'
    SignedRecordBytes = [uint64]7715
    SignedRecordSha256 = '0383DAE8FA04705FA26F63CB3CC4EDE16442129D87F99E7900BD1EF301C4FB5C'
    ExpectedPublisher = 'Joshua Beel'
}

function Write-PrintUnavailableRecord {
    param([Parameter(Mandatory = $true)][string]$Root,[Parameter(Mandatory = $true)]$PrinterFacts,[string]$WorkflowSourceRevision)
    if (Test-Path -LiteralPath $Root) { throw 'Unavailable-print output root must be fresh.' }
    [IO.Directory]::CreateDirectory($Root) | Out-Null
    $record = [ordered]@{
        schemaVersion = 1
        scope = 'Hosted signed installed Windows print-dialog verification.'
        status = 'explicit-unavailable'
        workflowSourceRevision = $WorkflowSourceRevision
        signedSourceRevision = $script:Pins.SignedSourceRevision
        printerPreflight = [ordered]@{ printerCount=[int]$PrinterFacts.printerCount;anyPrinterAvailable=$false;microsoftPrintToPdfAvailable=$false;featureInstallationAttempted=$false }
        installationAttempted = $false
        reason = 'No Windows printer was available on the hosted runner before installation.'
    }
    $json = $record | ConvertTo-Json -Depth 8
    if ($json -match '(?i)([A-Z]:\\|\\Users\\|runneradmin|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|12510717103|39969839829|raw\.log)') { throw 'Unavailable-print record contains restricted evidence.' }
    [IO.File]::WriteAllText((Join-Path $Root 'print-dialog-verification.json'),$json,[Text.UTF8Encoding]::new($false))
}

function Assert-SignedPrintingSourceBlobs {
    param([Parameter(Mandatory = $true)][string]$ProjectRoot,[Parameter(Mandatory = $true)][string]$WorkflowRevision)
    $exactPaths = @(
        'src-tauri/src/printing.rs','src-tauri/src/print_commands.rs','src-tauri/src/service.rs','src/PrintDialog.tsx',
        'src/bridge.ts','src-tauri/src/main.rs','src-tauri/resources/welcome.pdf','src-tauri/tauri.conf.json'
    )
    foreach ($path in $exactPaths) {
        $workflowBlob = (& git -C $ProjectRoot rev-parse "${WorkflowRevision}:$path").Trim()
        if ($LASTEXITCODE -ne 0 -or $workflowBlob -cnotmatch '^[a-f0-9]{40,64}$') { throw 'Could not resolve a workflow printing source blob.' }
        $signedBlob = (& git -C $ProjectRoot rev-parse "$($script:Pins.SignedSourceRevision):$path").Trim()
        if ($LASTEXITCODE -ne 0 -or $signedBlob -cne $workflowBlob) { throw "Workflow printing source does not match the signed application: $path" }
    }
    $workflowShellBlob = (& git -C $ProjectRoot rev-parse "${WorkflowRevision}:src/App.tsx").Trim()
    if ($LASTEXITCODE -ne 0 -or $workflowShellBlob -cnotmatch '^[a-f0-9]{40,64}$') { throw 'Could not resolve the workflow application shell blob.' }
    $signedShellBlob = (& git -C $ProjectRoot rev-parse "$($script:Pins.SignedSourceRevision):src/App.tsx").Trim()
    if ($LASTEXITCODE -ne 0 -or $signedShellBlob -cnotmatch '^[a-f0-9]{40,64}$') { throw 'Could not resolve the signed application shell blob.' }
    return [pscustomobject]@{ matchedImplementationBlobCount=$exactPaths.Count;appShellBlobMatchedSignedSource=$workflowShellBlob -ceq $signedShellBlob }
}

function Write-SanitizedPrintRecord {
    param(
        [Parameter(Mandatory = $true)][string]$Root,[Parameter(Mandatory = $true)][string]$WorkflowSourceRevision,
        [Parameter(Mandatory = $true)]$Receipt,[Parameter(Mandatory = $true)]$ReceiptSets,
        [Parameter(Mandatory = $true)]$PrinterFacts,[Parameter(Mandatory = $true)]$PrintResult,
        [Parameter(Mandatory = $true)]$SourceBinding
    )
    if (Test-Path -LiteralPath $Root) { throw 'Print verification output root must be fresh.' }
    [IO.Directory]::CreateDirectory($Root) | Out-Null
    $welcome = @($ReceiptSets.Base | Where-Object { [string]$_.target -ceq 'resources/welcome.pdf' })
    if ($welcome.Count -ne 1) { throw 'Welcome sample receipt is not unique.' }
    $record = [ordered]@{
        schemaVersion = 1
        scope = 'Hosted signed installed Windows print-dialog verification; physical paper output and non-Microsoft printer drivers remain unverified.'
        status = 'verified'
        workflowSourceRevision = $WorkflowSourceRevision
        signedSourceRevision = $script:Pins.SignedSourceRevision
        sourceBinding = [ordered]@{
            claim='Exact signed printing implementation/resource subset; application shell parity reported separately.'
            exactSignedPrintingImplementationSubsetMatched=$true
            matchedImplementationBlobCount=[int]$SourceBinding.matchedImplementationBlobCount
            appShellBlobMatchedSignedSource=[bool]$SourceBinding.appShellBlobMatchedSignedSource
        }
        installedReceipts = [ordered]@{
            application=[ordered]@{bytes=[uint64]$Receipt.packagedApplication.bytes;sha256=[string]$Receipt.packagedApplication.sha256;publisher=$script:Pins.ExpectedPublisher;trustedTimestamp=$true}
            sample=[ordered]@{name='welcome.pdf';bytes=[uint64]$welcome[0].bytes;sha256=[string]$welcome[0].sha256}
            pdfium=[ordered]@{bytes=[uint64]$ReceiptSets.Pdfium.bytes;sha256=[string]$ReceiptSets.Pdfium.sha256;publisher=$script:Pins.ExpectedPublisher;trustedTimestamp=$true}
        }
        printerPreflight = [ordered]@{ printerCount=[int]$PrinterFacts.printerCount;anyPrinterAvailable=[bool]$PrinterFacts.anyPrinterAvailable;microsoftPrintToPdfAvailable=[bool]$PrinterFacts.microsoftPrintToPdfAvailable;featureInstallationAttempted=$false }
        nativeDialog = [ordered]@{
            opened=[bool]$PrintResult.nativeDialogOpenVerified;cancelled=[bool]$PrintResult.nativeDialogCancelVerified
            cancellationResultTransported=[bool]$PrintResult.cancelResultTransportVerified;reopened=[bool]$PrintResult.nativeDialogReopenVerified
            webDialogClosed=[bool]$PrintResult.webPrintDialogClosed;automation='process-bound-.NET-UIAutomation'
        }
        pdfOutput = $PrintResult.output
        runtime = [ordered]@{ nativeDriverVersion=[string]$PrintResult.nativeDriverVersion;webView2RuntimeVersion=[string]$PrintResult.returnedRuntimeVersion;profileBinding=[string]$PrintResult.profileBinding }
        cleanup = [ordered]@{ sessionDeleted=[bool]$PrintResult.sessionDeleted;ownedProcessTreeStopped=[bool]$PrintResult.ownedProcessTreeStopped;relevantProcessesRemaining=[int]$PrintResult.relevantProcessesRemaining;nativeDialogsRemaining=0 }
    }
    $json = $record | ConvertTo-Json -Depth 12
    if ($json -match '(?i)([A-Z]:\\|\\Users\\|runneradmin|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|12510717103|39969839829|raw\.log|\.pdf-workstation)') { throw 'Sanitized print record contains a host path, credential identifier, artifact identifier, or raw evidence reference.' }
    $path = Join-Path $Root 'print-dialog-verification.json'
    [IO.File]::WriteAllText($path,$json,[Text.UTF8Encoding]::new($false))
    $files = @(Get-ChildItem -LiteralPath $Root -File -Force)
    if ($files.Count -ne 1 -or -not $files[0].FullName.Equals($path,[StringComparison]::OrdinalIgnoreCase)) { throw 'Print output root must contain exactly one sanitized JSON record.' }
    return $path
}

$printerFacts = Get-PrintCapabilityFacts
Assert-PrintCapabilityFacts -Facts $printerFacts
$workflowRevision = if ($env:GITHUB_SHA) { [string]$env:GITHUB_SHA } else { '' }
if (-not [bool]$printerFacts.anyPrinterAvailable) {
    if ([string]::IsNullOrWhiteSpace($OutputRoot)) { throw 'OutputRoot is required to record explicit printer unavailability.' }
    if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) { throw 'RUNNER_TEMP is unavailable for explicit printer unavailability evidence.' }
    $unavailableOutput = Resolve-RunnerPath -Path $OutputRoot -RunnerTemp ([string]$env:RUNNER_TEMP) -MustBeFresh
    Write-PrintUnavailableRecord -Root $unavailableOutput -PrinterFacts $printerFacts -WorkflowSourceRevision $workflowRevision
    throw 'No Windows printer is available; verification stopped before installing the signed application or any Windows feature.'
}
if ($PreflightOnly) {
    Write-Output ('Printer preflight succeeded: printerCount=' + [int]$printerFacts.printerCount + ';microsoftPrintToPdfAvailable=' + ([bool]$printerFacts.microsoftPrintToPdfAvailable).ToString().ToLowerInvariant() + ';featureInstallationAttempted=false')
    return
}

foreach ($required in @($SignedArtifactRoot,$WorkRoot,$OutputRoot,$WebDriverRoot,$WebViewProfileRoot)) {
    if ([string]::IsNullOrWhiteSpace($required)) { throw 'All full-verification paths are required.' }
}
$projectRoot = Split-Path -Parent $PSScriptRoot
$runnerFacts = Get-HostedRunnerFacts -ProjectRoot $projectRoot
Assert-HostedRunnerFacts -Facts $runnerFacts
$sourceBinding = Assert-SignedPrintingSourceBlobs -ProjectRoot $projectRoot -WorkflowRevision ([string]$runnerFacts.githubSha)
if ([int]$sourceBinding.matchedImplementationBlobCount -ne 8) { throw 'The signed printing source binding did not cover exactly eight implementation and resource files.' }
$runnerTemp = [string]$env:RUNNER_TEMP
if ([string]::IsNullOrWhiteSpace($runnerTemp)) { throw 'RUNNER_TEMP is unavailable.' }
$signedRoot = Resolve-RunnerPath -Path $SignedArtifactRoot -RunnerTemp $runnerTemp -MustExist
$work = Resolve-RunnerPath -Path $WorkRoot -RunnerTemp $runnerTemp -MustBeFresh
$output = Resolve-RunnerPath -Path $OutputRoot -RunnerTemp $runnerTemp -MustBeFresh
$drivers = Resolve-RunnerPath -Path $WebDriverRoot -RunnerTemp $runnerTemp -MustExist
$profile = Resolve-RunnerPath -Path $WebViewProfileRoot -RunnerTemp $runnerTemp -MustBeFresh

$signedInstaller = Join-Path $signedRoot $script:Pins.SignedInstallerName
$recordPath = Join-Path $signedRoot $script:Pins.SignedRecordName
$signedFiles = @(Get-ChildItem -LiteralPath $signedRoot -Recurse -File -Force)
if ($signedFiles.Count -ne 2 -or -not (Test-Path -LiteralPath $signedInstaller -PathType Leaf) -or -not (Test-Path -LiteralPath $recordPath -PathType Leaf)) { throw 'Downloaded signed artifact must contain exactly the installer and sanitized record.' }
Assert-FileReceipt -Path $signedInstaller -Bytes $script:Pins.SignedInstallerBytes -Sha256 $script:Pins.SignedInstallerSha256 -Kind 'Signed 0.3.0 installer'
Assert-FileReceipt -Path $recordPath -Bytes $script:Pins.SignedRecordBytes -Sha256 $script:Pins.SignedRecordSha256 -Kind 'Signed artifact verification record'
$null = Assert-TrustedWindowsSignature -Path $signedInstaller -ExpectedPublisher $script:Pins.ExpectedPublisher
$recordText = Get-Content -LiteralPath $recordPath -Raw -Encoding UTF8
if ($recordText -match '(?i)([A-Z]:\\|\\Users\\|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|raw\.log)') { throw 'Downloaded signed artifact record is not sanitized.' }
$receipt = $recordText | ConvertFrom-Json
$receiptSets = Assert-SignedArtifactReceipt -Receipt $receipt

$installRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'PDF Workstation')).TrimEnd('\')
$settingsRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'local.pdfworkstation.desktop')).TrimEnd('\')
$registryPath = 'Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\PDF Workstation'
$fresh = Get-FreshInstallFacts -InstallRoot $installRoot -SettingsRoot $settingsRoot -RegistryPath $registryPath -InstallerPaths @($signedInstaller)
Assert-FreshInstallFacts -Facts $fresh
[IO.Directory]::CreateDirectory($work) | Out-Null
Assert-NoReparseAncestors -Path $work
$extractedRoot = Join-Path $work 'signed-extracted'
$extracted = Expand-InstallerProof -Installer $signedInstaller -Destination $extractedRoot
$null = Assert-ExtractedSignedArtifact -Files $extracted -ExtractionRoot $extractedRoot -Receipt $receipt -ReceiptSets $receiptSets

Invoke-BoundedSilentInstaller -Path $signedInstaller
$processFacts = Get-ConflictingProcessFacts -InstallerPaths @($signedInstaller)
if ($processFacts.applicationProcessPresent -or $processFacts.installerProcessPresent) { throw 'Signed silent install left an application or installer process running.' }
$installed = Get-InstallFacts -RegistryPath $registryPath -ExpectedInstallRoot $installRoot
Assert-InstallFacts -Facts $installed -ExpectedVersion '0.3.0' -ExpectedInstallRoot $installRoot -ExpectedApplication $receipt.packagedApplication -ExpectedSignatureStatus 'Valid' -ExpectedPublisher $script:Pins.ExpectedPublisher -ExpectedTimestamp $true
$resources = Assert-InstalledSignedResources -InstallRoot $installRoot -Receipt $receipt -ReceiptSets $receiptSets
if (Test-Path -LiteralPath $settingsRoot) { throw 'Silent install unexpectedly created the application settings root.' }
[IO.Directory]::CreateDirectory($settingsRoot) | Out-Null
Assert-NoReparseAncestors -Path $settingsRoot

$welcomeReceipt = @($receiptSets.Base | Where-Object { [string]$_.target -ceq 'resources/welcome.pdf' })
if ($welcomeReceipt.Count -ne 1) { throw 'Installed welcome sample receipt is not unique.' }
$outputPdf = Join-Path $work 'printed-welcome-page-1.pdf'
$printResult = Invoke-InstalledPrintDialogVerification `
    -ApplicationPath $resources.Application.FullName -ApplicationReceipt $receipt.packagedApplication `
    -SamplePath $resources.Welcome.FullName -SampleReceipt $welcomeReceipt[0] `
    -PdfiumPath $resources.Pdfium.FullName -PdfiumReceipt $receiptSets.Pdfium `
    -WebDriverRoot $drivers -ProfileRoot $profile -SettingsRoot $settingsRoot -OutputPdfPath $outputPdf `
    -ExpectedPublisher $script:Pins.ExpectedPublisher -PrinterFacts $printerFacts

Assert-FileReceipt -Path $resources.Application.FullName -Bytes ([uint64]$receipt.packagedApplication.bytes) -Sha256 ([string]$receipt.packagedApplication.sha256) -Kind 'Post-print installed application'
Assert-FileReceipt -Path $resources.Welcome.FullName -Bytes ([uint64]$welcomeReceipt[0].bytes) -Sha256 ([string]$welcomeReceipt[0].sha256) -Kind 'Post-print installed sample'
Assert-FileReceipt -Path $resources.Pdfium.FullName -Bytes ([uint64]$receiptSets.Pdfium.bytes) -Sha256 ([string]$receiptSets.Pdfium.sha256) -Kind 'Post-print installed PDFium'
$proof = Write-SanitizedPrintRecord -Root $output -WorkflowSourceRevision ([string]$runnerFacts.githubSha) -Receipt $receipt -ReceiptSets $receiptSets -PrinterFacts $printerFacts -PrintResult $printResult -SourceBinding $sourceBinding
Write-Output 'Signed installed print-dialog verification succeeded.'
Write-Output ('Sanitized record SHA-256: ' + (Get-ExactSha256 -Path $proof))
