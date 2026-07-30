# Shared fail-closed display-connection classification for acquisition and
# scoring. DisplayConfig exposes this enumeration as a signed Int32 while WMI
# exposes the same 32-bit value as UInt32, so normalize the bit pattern before
# applying the physical-output allowlist.

function ConvertTo-KettlePerfCanonicalOutputTechnology {
    param($Value)

    if (
        $null -eq $Value -or
        $Value -is [bool] -or
        $Value -is [string] -or
        -not (
            $Value -is [sbyte] -or
            $Value -is [byte] -or
            $Value -is [int16] -or
            $Value -is [uint16] -or
            $Value -is [int32] -or
            $Value -is [uint32] -or
            $Value -is [int64] -or
            $Value -is [uint64] -or
            $Value -is [single] -or
            $Value -is [double] -or
            $Value -is [decimal]
        )
    ) {
        return $null
    }

    try {
        $number = [decimal]$Value
    } catch {
        return $null
    }
    if (
        [decimal]::Truncate($number) -ne $number -or
        $number -lt [int]::MinValue -or
        $number -gt [uint32]::MaxValue
    ) {
        return $null
    }
    if ($number -lt 0) {
        $number += [decimal]4294967296
    }
    return [uint64]$number
}

function Test-KettlePerfPhysicalOutputTechnology {
    param($Value)

    $canonical = ConvertTo-KettlePerfCanonicalOutputTechnology -Value $Value
    if ($null -eq $canonical) {
        return $false
    }

    # Explicit physical connector types from
    # DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY. Miracast (15), indirect wired
    # (16), and indirect virtual (17) can synthesize display identity and are
    # intentionally excluded from physical release evidence. Value 18 is a
    # physical DisplayPort USB tunnel; 0x80000000 is an internal panel.
    return [uint64]$canonical -in [uint64[]]@(
        0, 1, 2, 3, 4, 5, 6,
        8, 9, 10, 11, 12, 13, 14,
        18,
        2147483648
    )
}
