using System.Reflection;
using System.Runtime.ExceptionServices;
using KYNXA.ToolHost;

// Inspect the real launch classification with synthetic PE files; never invoke Launch or create a window.
// 使用合成 PE 文件检查真实启动分类，绝不调用 Launch 或创建窗口。
int checks = 0;
string fixture = Directory.CreateDirectory(Path.Combine(Path.GetTempPath(), "kynxa-application-policy-" + Guid.NewGuid().ToString("N"))).FullName;
void Check(bool condition, string label) { if (!condition) throw new Exception(label); checks++; }
object? Invoke(string method, params object[] arguments)
{
    try { return typeof(DesktopApplications).GetMethod(method, BindingFlags.NonPublic | BindingFlags.Static)!.Invoke(null, arguments); }
    catch (TargetInvocationException error) when (error.InnerException is not null)
    { ExceptionDispatchInfo.Capture(error.InnerException).Throw(); throw; }
}
string Executable(string name, ushort subsystem)
{
    string path = Path.Combine(fixture, name + ".exe");
    using var output = new BinaryWriter(File.Open(path, FileMode.Create));
    output.BaseStream.SetLength(256);
    output.Write((ushort)0x5a4d);
    output.BaseStream.Position = 0x3c; output.Write(64);
    output.BaseStream.Position = 64; output.Write(0x00004550);
    output.BaseStream.Position = 88; output.Write((ushort)0x20b);
    output.BaseStream.Position = 156; output.Write(subsystem);
    return path;
}
void InvalidEntry(string path, string[] arguments)
{
    try { Invoke("ValidateWindowedRuntimeEntry", path, arguments); throw new Exception("Invalid runtime entry was accepted."); }
    catch (DesktopException error) { Check(error.Code == "DESKTOP_LAUNCH_BLOCKED", "Invalid runtime entry keeps the launch boundary"); }
}
try
{
    string gui = Executable("synthetic-gui", 2), blender = Executable("blender", 3);
    Check((bool)Invoke("IsApplicationExecutable", gui)!, "GUI PE accepted");
    Check((bool)Invoke("IsApplicationExecutable", blender)!, "Console-subsystem application accepted");
    Check(!(bool)Invoke("IsApplicationExecutable", Executable("unsupported", 1))!, "Unsupported PE rejected");
    foreach (string name in new[] { "cmd", "powershell", "python", "python3", "node", "dotnet", "windowsterminal" })
        Check((bool)Invoke("IsBlockedExecutable", Executable(name, 3))!, "Shell/runtime remains in host terminal: " + name);
    Check(!(bool)Invoke("IsBlockedExecutable", blender)!, "Blender is an application");
    string pythonWindowed = Executable("pythonw", 2), javaWindowed = Executable("javaw", 2);
    Check(!(bool)Invoke("IsBlockedExecutable", pythonWindowed)!, "Windowed Python can use a file entry");
    Check(!(bool)Invoke("IsBlockedExecutable", javaWindowed)!, "Windowed Java can use a jar entry");
    string script = Path.Combine(fixture, "application.pyw"), jar = Path.Combine(fixture, "application.jar");
    File.WriteAllText(script, "# Synthetic fixture, never executed.");
    File.WriteAllText(jar, "Synthetic fixture, never executed.");
    Invoke("ValidateWindowedRuntimeEntry", pythonWindowed, new[] { script }); checks++;
    Invoke("ValidateWindowedRuntimeEntry", javaWindowed, new[] { "-jar", jar }); checks++;
    InvalidEntry(pythonWindowed, ["-c", "print('fixture')"]);
    InvalidEntry(pythonWindowed, ["-m", "fixture"]);
    InvalidEntry(pythonWindowed, ["relative.pyw"]);
    InvalidEntry(javaWindowed, ["-jar", "missing.jar"]);
    InvalidEntry(javaWindowed, ["FixtureMain"]);
    Console.WriteLine($"PASS application launch classification: {checks} checks; no application or desktop interaction.");
}
finally
{
    string relative = Path.GetRelativePath(Path.GetTempPath(), fixture);
    if (relative.Length == 0 || relative.StartsWith("..") || Path.IsPathFullyQualified(relative)) throw new Exception("Unsafe fixture cleanup.");
    Directory.Delete(fixture, recursive: true);
}

namespace KYNXA.ToolHost
{
    // The shared P/Invoke check references this exception; no sandbox operation is exercised here.
    // 共用 P/Invoke 检查引用此异常，本夹具不执行任何沙箱操作。
    internal sealed class SandboxException(string code, string message) : Exception(message)
    {
        public string Code { get; } = code;
    }
}
