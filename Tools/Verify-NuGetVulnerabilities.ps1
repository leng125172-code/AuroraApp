[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$solutionPath = Join-Path $PSScriptRoot '..\Sources\DotNet\Aurora.slnx'
$json = & dotnet package list --project $solutionPath --vulnerable --include-transitive --format json --output-version 1 --no-restore
if ($LASTEXITCODE -ne 0) {
    throw "dotnet package list failed with exit code $LASTEXITCODE"
}

$report = $json | ConvertFrom-Json -Depth 100
$findings = [System.Collections.Generic.List[object]]::new()

function Find-Vulnerabilities {
    param([Parameter(Mandatory)] [object] $Node)

    if ($Node -is [System.Collections.IDictionary]) {
        foreach ($key in $Node.Keys) {
            if ($key -eq 'vulnerabilities' -and $Node[$key].Count -gt 0) {
                foreach ($finding in $Node[$key]) {
                    $findings.Add($finding)
                }
            }
            Find-Vulnerabilities -Node $Node[$key]
        }
        return
    }

    if ($Node -is [pscustomobject]) {
        foreach ($property in $Node.PSObject.Properties) {
            if ($property.Name -eq 'vulnerabilities' -and $property.Value.Count -gt 0) {
                foreach ($finding in $property.Value) {
                    $findings.Add($finding)
                }
            }
            Find-Vulnerabilities -Node $property.Value
        }
        return
    }

    if ($Node -is [System.Collections.IEnumerable] -and $Node -isnot [string]) {
        foreach ($item in $Node) {
            Find-Vulnerabilities -Node $item
        }
    }
}

Find-Vulnerabilities -Node $report
if ($findings.Count -gt 0) {
    $findings | ConvertTo-Json -Depth 20 | Write-Error
    throw "NuGet audit found $($findings.Count) vulnerability entries"
}

Write-Output 'NuGet audit found no known vulnerabilities.'
