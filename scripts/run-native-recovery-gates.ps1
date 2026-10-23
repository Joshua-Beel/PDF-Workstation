param([string]$CargoPath = 'cargo')
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$manifest = 'src-tauri/Cargo.toml'
& $CargoPath test --locked --manifest-path $manifest -- --test-threads=1
if ($LASTEXITCODE -ne 0) { throw 'The serialized native test gate failed.' }

$required = @(
  @{ Name = 'service::tests::recovery_checkpoint_ack_is_durable_and_explicit_restore_replays_exact_state'; Target = @('--bin', 'pdf-workstation'); Ignored = $true },
  @{ Name = 'service::tests::explicit_discard_is_idempotent_and_resolves_post_publish_lock'; Target = @('--bin', 'pdf-workstation'); Ignored = $true },
  @{ Name = 'service::tests::keep_recovered_edits_never_serves_a_render_cached_during_the_offer'; Target = @('--bin', 'pdf-workstation'); Ignored = $true },
  @{ Name = 'service::recovery_restart_tests::restart_recovery_survives_fresh_workers'; Target = @('--bin', 'pdf-workstation'); Ignored = $true },
  @{ Name = 'recovery_store::process_tests::cross_process_lock_serializes_admission_and_generations'; Target = @('--bin', 'pdf-workstation'); Ignored = $true },
  @{ Name = 'service::tests::open_refuses_past_the_document_cap_and_non_files_without_loading_anything'; Target = @('--bin', 'pdf-workstation'); Ignored = $true },
  @{ Name = 'aggregate_record_count_rejects_new_sources_but_allows_exact_replacement'; Target = @('--test', 'recovery_store'); Ignored = $false }
)
foreach ($probe in $required) {
  $testName = [string]$probe.Name
  $arguments = @('test', '--locked', '--manifest-path', $manifest) + @($probe.Target) + @($testName, '--')
  if ([bool]$probe.Ignored) { $arguments += '--ignored' }
  $arguments += @('--exact', '--test-threads=1')
  $output = @(& $CargoPath @arguments 2>&1 | ForEach-Object { $_.ToString() })
  $status = $LASTEXITCODE
  $text = $output -join "`n"
  if ([Text.Encoding]::UTF8.GetByteCount($text) -gt 65536) { throw "The required native test receipt exceeded its output bound: $testName" }
  $output | Write-Output
  if ($status -ne 0) { throw "The required native test failed: $testName" }
  $footers = @([regex]::Matches($text, 'test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; \d+ filtered out; finished in [^\r\n]+'))
  if ($footers.Count -ne 1) { throw "The required native test did not produce one exact passing receipt: $testName" }
}
