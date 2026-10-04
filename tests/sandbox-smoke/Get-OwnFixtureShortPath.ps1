param([Parameter(Mandatory = $true)][string]$LiteralPath)

[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

Add-Type -TypeDefinition 'using System.Text; using System.Runtime.InteropServices; public static class KynxaFixturePathNative { [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] public static extern uint GetShortPathNameW(string path, StringBuilder buffer, uint size); }'
$fixtureBuffer = [System.Text.StringBuilder]::new(32768)
$fixtureLength = [KynxaFixturePathNative]::GetShortPathNameW($LiteralPath, $fixtureBuffer, 32768)
if ($fixtureLength -eq 0 -or $fixtureLength -ge $fixtureBuffer.Capacity) { throw 'Cannot obtain the fixture short path.' }
$fixtureBuffer.ToString()
