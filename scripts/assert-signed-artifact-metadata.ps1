[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$TargetSourceRevision,
    [Parameter(Mandatory=$true)][string]$ArtifactRunId,
    [Parameter(Mandatory=$true)][string]$ArtifactId,
    [Parameter(Mandatory=$true)][string]$ArtifactName,
    [Parameter(Mandatory=$true)][string]$ArtifactBytes,
    [Parameter(Mandatory=$true)][string]$ArtifactDigest,
    [scriptblock]$JsonProvider
)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest

function Import-MetadataFunctions { param([string]$Path,[string[]]$Names)
 $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile($Path,[ref]$tokens,[ref]$errors);if($errors.Count){throw 'A trusted metadata helper did not parse.'}
 foreach($name in $Names){$node=$ast.Find({param($n)$n-is[Management.Automation.Language.FunctionDefinitionAst]-and$n.Name-ceq$name},$true);if(-not$node){throw "Missing trusted metadata helper: $name"};Set-Item -Path ("Function:script:"+$name) -Value $node.Body.GetScriptBlock()}
}
Import-MetadataFunctions (Join-Path $PSScriptRoot 'verify-default-draft.ps1') @('ConvertTo-ExactUInt64','New-GitHubGetRequest','Assert-GitHubResponseSuccess','Invoke-GitHubJson')

function Get-SignedArtifactJson { param([string]$Uri,[string]$Token)
 if($JsonProvider){return &$JsonProvider $Uri}
 return Invoke-GitHubJson $Uri $Token
}

function Assert-SignedArtifactMetadata { param([string]$Token)
 if($TargetSourceRevision-cnotmatch'^[a-f0-9]{40}$'-or$ArtifactName-cne"signed-ocr-installer-$TargetSourceRevision"-or$ArtifactDigest-cnotmatch'^sha256:[a-f0-9]{64}$'){throw 'Signed artifact identity input is malformed.'}
 $runId=ConvertTo-ExactUInt64 $ArtifactRunId 'Signed artifact run id';$artifactIdValue=ConvertTo-ExactUInt64 $ArtifactId 'Signed artifact id';$bytes=ConvertTo-ExactUInt64 $ArtifactBytes 'Signed artifact bytes'
 $artifact=Get-SignedArtifactJson "https://api.github.com/repos/Joshua-Beel/PDF-Workstation/actions/artifacts/$artifactIdValue" $Token
 if([uint64]$artifact.id-ne$artifactIdValue-or[string]$artifact.name-cne$ArtifactName-or[uint64]$artifact.size_in_bytes-ne$bytes-or[string]$artifact.digest-cne$ArtifactDigest-or[bool]$artifact.expired-or[uint64]$artifact.workflow_run.id-ne$runId-or[string]$artifact.workflow_run.head_sha-cne$TargetSourceRevision-or[string]$artifact.workflow_run.head_branch-cne'master'){throw 'Signed artifact metadata mismatch.'}
 $run=Get-SignedArtifactJson "https://api.github.com/repos/Joshua-Beel/PDF-Workstation/actions/runs/$runId" $Token
 if([uint64]$run.id-ne$runId-or[string]$run.name-cne'Manual signed OCR artifact validation'-or[string]$run.event-cne'workflow_dispatch'-or[string]$run.status-cne'completed'-or[string]$run.conclusion-cne'success'-or[string]$run.head_branch-cne'master'-or[string]$run.head_sha-cne$TargetSourceRevision-or[string]$run.path-cne'.github/workflows/ocr-signed-artifact.yml'-or[string]$run.repository.full_name-cne'Joshua-Beel/PDF-Workstation'){throw 'Signed artifact workflow run identity mismatch.'}
}

$token=$env:GITHUB_TOKEN
if(-not$JsonProvider-and[string]::IsNullOrWhiteSpace($token)){throw 'GitHub token is unavailable for signed artifact metadata.'}
try{Assert-SignedArtifactMetadata $token}finally{$token=$null}
[Console]::Out.WriteLine('SIGNED_ARTIFACT_METADATA_VERIFIED')
