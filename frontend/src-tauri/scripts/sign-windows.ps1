param(
    [Parameter(Mandatory=$true)]
    [string]$FilePath
)

# Check if signing is enabled
if (-not $env:DIGICERT_KEYPAIR_ALIAS) {
    Write-Host "Skipping signing - DIGICERT_KEYPAIR_ALIAS not set"
    exit 0
}

Write-Host "Signing: $FilePath"
Write-Host "Using keypair alias: $env:DIGICERT_KEYPAIR_ALIAS"

# Sign the file
smctl sign -i $FilePath -k $env:DIGICERT_KEYPAIR_ALIAS

if ($LASTEXITCODE -ne 0) {
    Write-Error "Signing failed with exit code: $LASTEXITCODE"
    exit $LASTEXITCODE
}

Write-Host "Successfully signed: $FilePath"
