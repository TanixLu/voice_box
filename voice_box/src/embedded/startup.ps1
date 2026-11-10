$ErrorActionPreference = 'Stop'
$stream = $null
$sha = $null
try {
    $path = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__VOICEBOX_PATH__'))
    $expected = '__VOICEBOX_SHA256__'
    $stream = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    # Reject redirected paths while the target is locked against replacement.
    for ($entry = $path; $entry; $entry = [IO.Path]::GetDirectoryName($entry)) {
        if (([IO.File]::GetAttributes($entry) -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            exit 1
        }
    }
    $sha = [Security.Cryptography.SHA256]::Create()
    $actual = [BitConverter]::ToString($sha.ComputeHash($stream)).Replace('-', '')
    if ($actual -cne $expected) { exit 2 }
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $path
    $start.Arguments = '__VOICEBOX_ARGUMENT__'
    $start.WorkingDirectory = [IO.Path]::GetDirectoryName($path)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $child = [Diagnostics.Process]::Start($start)
    if ($null -eq $child) { throw 'Process creation failed' }
    $child.Dispose()
} catch {
    exit 3
} finally {
    if ($null -ne $sha) { $sha.Dispose() }
    if ($null -ne $stream) { $stream.Dispose() }
}
exit 0
