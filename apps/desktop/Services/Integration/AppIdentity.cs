using System.Diagnostics;
using System.Runtime.InteropServices;

namespace KYNXA_Desktop.Services;

internal static class AppIdentity
{
    public const string DisplayName = "KYNXA 正式版";
    public const string AppUserModelId = "KYNXA.Desktop";

    public static void Configure()
    {
        int result = SetCurrentProcessExplicitAppUserModelID(AppUserModelId);
        if (result < 0) Debug.WriteLine($"Application identity: 0x{result:X8}");
    }

    [DllImport("shell32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern int SetCurrentProcessExplicitAppUserModelID(string appId);
}
