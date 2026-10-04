using System.Text;
using System.Text.Json;

namespace KYNXA.ToolHost;

internal static class Program
{
    internal static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);
    private static int _checks;

    private static int Main()
    {
        byte[] oemChineseLine = [0xd6, 0xd0, 0xce, 0xc4, 0x0d, 0x0a]; // CP936: 中文
        byte[] utf8ChineseLine = [0xe4, 0xb8, 0xad, 0xe6, 0x96, 0x87, 0x0d, 0x0a];
        Equal(HostTerminalRunner.DecodeOutput([], true, 936), "", "Empty output");
        Equal(HostTerminalRunner.DecodeOutput(Encoding.ASCII.GetBytes("fixture\r\n"), true, 936), "fixture\r\n", "ASCII output");
        Equal(HostTerminalRunner.DecodeOutput(oemChineseLine, true, 936), "中文\r\n", "OEM builtin line");
        Equal(HostTerminalRunner.DecodeOutput(utf8ChineseLine, true, 936), "中文\r\n", "UTF-8 external line");
        Equal(HostTerminalRunner.DecodeOutput([.. oemChineseLine, .. utf8ChineseLine, .. oemChineseLine], true, 936),
            "中文\r\n中文\r\n中文\r\n", "Alternating encodings remain separate");
        Equal(HostTerminalRunner.DecodeOutput([0xd6, 0xd0, 0xce, 0xc4], true, 936), "中文", "OEM final line without newline");
        Equal(HostTerminalRunner.DecodeOutput([0xe4, 0xb8, 0xad, 0xe6, 0x96, 0x87], true, 936), "中文", "UTF-8 final line without newline");
        Equal(HostTerminalRunner.DecodeOutput(utf8ChineseLine, false, 936), "中文\r\n", "PowerShell declared UTF-8 output");
        // These bytes are also valid in some OEM pages: an unmarked stream cannot disambiguate them.
        Equal(HostTerminalRunner.DecodeOutput([0xc2, 0xa3, 0x0a], true, 936), "£\n", "Documented strict UTF-8 precedence");
        Console.WriteLine($"PASS: {_checks} host terminal decoding checks");
        return 0;
    }

    private static void Equal(string actual, string expected, string check)
    {
        if (actual != expected) throw new InvalidOperationException(check);
        _checks++;
    }
}

// Linked native declarations reference this exception; decoding checks never launch a process or call native sandbox methods.
internal sealed class SandboxException(string code, string message) : Exception(message)
{
    public string Code { get; } = code;
}
