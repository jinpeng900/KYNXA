[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$MainExecutable,
    [Parameter(Mandatory = $true)][string]$ExpansionExecutable,
    [switch]$SkipDesktop
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ([IntPtr]::Size -ne 8) { throw 'Use 64-bit PowerShell to register these Windows x64 applications.' }

# Register the two current-user launch entries without changing either application's data or ports.
# 注册两个当前用户启动入口，不修改任何一份软件的数据目录或端口。
$applications = @(
    @{ Name = 'KYNXA 正式版'; Id = 'KYNXA.Desktop'; Executable = [IO.Path]::GetFullPath($MainExecutable) },
    @{ Name = 'KYNXA 能力扩展版'; Id = 'KYNXA.Expansion.Desktop'; Executable = [IO.Path]::GetFullPath($ExpansionExecutable) }
)
foreach ($application in $applications) {
    if (-not (Test-Path -LiteralPath $application.Executable -PathType Leaf)) {
        throw "Application executable is missing: $($application.Executable)"
    }
}
if ($applications[0].Executable -eq $applications[1].Executable) { throw 'The two editions must use different executables.' }

if (-not ('Kynxa.ShortcutIdentity' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace Kynxa {
    public static class ShortcutIdentity {
        [StructLayout(LayoutKind.Sequential, Pack = 4)]
        private struct PropertyKey { public Guid Format; public uint Id; }
        [StructLayout(LayoutKind.Explicit, Size = 24)]
        private struct PropertyValue {
            [FieldOffset(0)] public ushort Type;
            [FieldOffset(8)] public IntPtr Text;
        }
        [ComImport, Guid("886d8eeb-8cf2-4446-8d02-cdba1dbdcf99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        private interface IPropertyStore {
            [PreserveSig] int GetCount(out uint count);
            [PreserveSig] int GetAt(uint index, out PropertyKey key);
            [PreserveSig] int GetValue(ref PropertyKey key, out PropertyValue value);
            [PreserveSig] int SetValue(ref PropertyKey key, ref PropertyValue value);
            [PreserveSig] int Commit();
        }
        [DllImport("shell32.dll", CharSet = CharSet.Unicode, PreserveSig = true)]
        private static extern int SHGetPropertyStoreFromParsingName(string path, IntPtr context,
            uint flags, ref Guid interfaceId, out IPropertyStore store);
        public static void Set(string path, string appId) {
            Guid interfaceId = typeof(IPropertyStore).GUID;
            IPropertyStore store;
            Marshal.ThrowExceptionForHR(SHGetPropertyStoreFromParsingName(path, IntPtr.Zero, 2, ref interfaceId, out store));
            var key = new PropertyKey { Format = new Guid("9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3"), Id = 5 };
            var value = new PropertyValue { Type = 31, Text = Marshal.StringToCoTaskMemUni(appId) };
            try {
                Marshal.ThrowExceptionForHR(store.SetValue(ref key, ref value));
                Marshal.ThrowExceptionForHR(store.Commit());
            }
            finally { Marshal.FreeCoTaskMem(value.Text); Marshal.ReleaseComObject(store); }
        }
    }
}
'@
}

$shortcutFolders = @([Environment]::GetFolderPath('Programs'))
if (-not $SkipDesktop) { $shortcutFolders += [Environment]::GetFolderPath('DesktopDirectory') }
$shell = New-Object -ComObject WScript.Shell
try {
    foreach ($application in $applications) {
        $root = [IO.Path]::GetDirectoryName($application.Executable)
        $icon = Join-Path $root 'Assets\AppIcon.ico'
        if (-not (Test-Path -LiteralPath $icon -PathType Leaf)) { $icon = $application.Executable }
        foreach ($folder in $shortcutFolders) {
            [IO.Directory]::CreateDirectory($folder) | Out-Null
            $shortcutPath = Join-Path $folder ($application.Name + '.lnk')
            $shortcut = $shell.CreateShortcut($shortcutPath)
            try {
                $shortcut.TargetPath = $application.Executable
                $shortcut.WorkingDirectory = $root
                $shortcut.Description = $application.Name
                $shortcut.IconLocation = $icon
                $shortcut.Save()
            }
            finally { [Runtime.InteropServices.Marshal]::ReleaseComObject($shortcut) | Out-Null }
            [Kynxa.ShortcutIdentity]::Set($shortcutPath, $application.Id)
        }
        $identityKey = 'HKCU:\Software\Classes\AppUserModelId\' + $application.Id
        New-Item -Path $identityKey -Force | Out-Null
        New-ItemProperty -LiteralPath $identityKey -Name DisplayName -Value $application.Name -PropertyType String -Force | Out-Null
        New-ItemProperty -LiteralPath $identityKey -Name IconUri -Value $icon -PropertyType String -Force | Out-Null
        Write-Output "$($application.Name): $($application.Executable) [$($application.Id)]"
    }
}
finally { [Runtime.InteropServices.Marshal]::ReleaseComObject($shell) | Out-Null }
