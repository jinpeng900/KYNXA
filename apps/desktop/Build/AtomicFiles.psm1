Set-StrictMode -Version Latest

# Publish a completed sibling file with one Windows rename; never delete the previous checkpoint first.
# 已完成的同目录临时文件通过一次 Windows 改名发布，不先删除上一份检查点。
if (-not ('KynxaAtomicFiles' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
public static class KynxaAtomicFiles {
    [DllImport("kernel32.dll", EntryPoint = "MoveFileExW", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    static extern bool MoveFileEx(string existing, string destination, uint flags);

    public static void Publish(string source, string destination) {
        source = Path.GetFullPath(source);
        destination = Path.GetFullPath(destination);
        if (String.Equals(source, destination, StringComparison.OrdinalIgnoreCase) ||
            !String.Equals(Path.GetDirectoryName(source), Path.GetDirectoryName(destination), StringComparison.OrdinalIgnoreCase)) {
            throw new IOException("Atomic state publication requires a distinct temporary file in the destination directory.");
        }
        const uint replaceExisting = 0x1;
        const uint writeThrough = 0x8;
        if (!MoveFileEx(source, destination, replaceExisting | writeThrough)) {
            int error = Marshal.GetLastWin32Error();
            throw new Win32Exception(error, "Cannot publish state file '" + destination + "' (Win32 " + error + "): " + new Win32Exception(error).Message);
        }
    }
}
'@
}

function Move-KynxaAtomicFile {
    param([Parameter(Mandatory = $true)][string]$Source, [Parameter(Mandatory = $true)][string]$Destination)
    [KynxaAtomicFiles]::Publish($Source, $Destination)
}

Export-ModuleMember -Function Move-KynxaAtomicFile
