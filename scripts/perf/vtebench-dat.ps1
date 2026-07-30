# Strict, bounded parser for vtebench's column-oriented Gnuplot DAT format.

function Read-KettlePerfVtebenchDatText {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string]$Text,
        [ValidateRange(1, 10000)]
        [int]$ExpectedColumns,
        [ValidateRange(1, 100000)]
        [int]$MaximumRows = 10000,
        [string]$Source = '<held vtebench DAT>'
    )

    $reader = [IO.StringReader]::new($Text)
    try {
        $header = $reader.ReadLine()
        if ($null -eq $header -or -not $header.Trim()) {
            throw "vtebench DAT has no header: $Source"
        }
        $names = [string[]]@($header.Trim() -split '\s+')
        if ($names.Count -ne $ExpectedColumns) {
            throw (
                "vtebench DAT has $($names.Count) columns, expected " +
                "${ExpectedColumns}: $Source"
            )
        }
        $nameSet = [Collections.Generic.HashSet[string]]::new(
            [StringComparer]::OrdinalIgnoreCase
        )
        foreach ($name in $names) {
            if (
                $name -notmatch '^[A-Za-z0-9_.+/-]+$' -or
                -not $nameSet.Add($name)
            ) {
                throw "vtebench DAT has an invalid or duplicate name '$name': $Source"
            }
        }

        $sampleLists = [ordered]@{}
        foreach ($name in $names) {
            $sampleLists[$name] = [Collections.Generic.List[double]]::new()
        }
        $sampleRows = 0
        while ($null -ne ($line = $reader.ReadLine())) {
            if (-not $line.Trim()) {
                continue
            }
            $sampleRows++
            if ($sampleRows -gt $MaximumRows) {
                throw "vtebench DAT exceeds the sample-row bound: $Source"
            }
            $values = [string[]]@($line.Trim() -split '\s+')
            if ($values.Count -ne $names.Count) {
                throw (
                    "vtebench DAT sample row has $($values.Count) values, " +
                    "expected $($names.Count): $Source"
                )
            }
            for ($index = 0; $index -lt $names.Count; $index++) {
                $value = $values[$index]
                if ($value -ceq '_') {
                    continue
                }
                $parsed = [uint64]0
                if (
                    $value -notmatch '^[0-9]+$' -or
                    -not [uint64]::TryParse(
                        $value,
                        [Globalization.NumberStyles]::None,
                        [Globalization.CultureInfo]::InvariantCulture,
                        [ref]$parsed
                    )
                ) {
                    throw "vtebench DAT has invalid sample '$value': $Source"
                }
                $sampleLists[$names[$index]].Add([double]$parsed)
            }
        }
        if ($sampleRows -eq 0) {
            throw "vtebench DAT has no sample rows: $Source"
        }

        $samples = [ordered]@{}
        foreach ($name in $names) {
            if ($sampleLists[$name].Count -eq 0) {
                throw "vtebench DAT column '$name' has no numeric samples: $Source"
            }
            $samples[$name] = [double[]]$sampleLists[$name].ToArray()
        }
        return [pscustomobject]@{
            Names = $names
            Samples = $samples
            SampleRows = $sampleRows
        }
    } finally {
        $reader.Dispose()
    }
}

function Read-KettlePerfVtebenchDatBytes {
    param(
        [Parameter(Mandatory)]
        [byte[]]$Bytes,
        [ValidateRange(1, 10000)]
        [int]$ExpectedColumns,
        [ValidateRange(1, 100000)]
        [int]$MaximumRows = 10000,
        [string]$Source = '<held vtebench DAT>'
    )

    if (
        $Bytes.Length -ge 3 -and
        $Bytes[0] -eq 0xef -and
        $Bytes[1] -eq 0xbb -and
        $Bytes[2] -eq 0xbf
    ) {
        throw "UTF-8 BOM is not accepted in vtebench DAT: $Source"
    }
    $utf8 = [Text.UTF8Encoding]::new($false, $true)
    try {
        $text = $utf8.GetString($Bytes)
    } catch {
        throw "vtebench DAT is not strict UTF-8: $Source"
    }
    return Read-KettlePerfVtebenchDatText `
        -Text $text -ExpectedColumns $ExpectedColumns `
        -MaximumRows $MaximumRows -Source $Source
}

function Read-KettlePerfVtebenchDat {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [ValidateRange(1, 10000)]
        [int]$ExpectedColumns,
        [ValidateRange(1, 2147483647)]
        [long]$MaximumBytes = 64MB,
        [ValidateRange(1, 100000)]
        [int]$MaximumRows = 10000
    )

    $fullPath = [IO.Path]::GetFullPath($Path)
    $item = Get-Item -LiteralPath $fullPath -Force -ErrorAction Stop
    if (
        $item.PSIsContainer -or
        ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -lt 1 -or
        $item.Length -gt $MaximumBytes
    ) {
        throw "vtebench DAT is not a bounded ordinary file: $fullPath"
    }
    $stream = [IO.FileStream]::new(
        $fullPath,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::Read,
        65536,
        [IO.FileOptions]::SequentialScan
    )
    try {
        if ($stream.Length -lt 1 -or $stream.Length -gt $MaximumBytes) {
            throw "vtebench DAT size is outside its bound: $fullPath"
        }
        $bytes = [byte[]]::new([int]$stream.Length)
        $offset = 0
        while ($offset -lt $bytes.Length) {
            $read = $stream.Read(
                $bytes,
                $offset,
                $bytes.Length - $offset
            )
            if ($read -eq 0) {
                throw "vtebench DAT ended during its held read: $fullPath"
            }
            $offset += $read
        }
    } finally {
        $stream.Dispose()
    }
    return Read-KettlePerfVtebenchDatBytes `
        -Bytes $bytes -ExpectedColumns $ExpectedColumns `
        -MaximumRows $MaximumRows -Source $fullPath
}
