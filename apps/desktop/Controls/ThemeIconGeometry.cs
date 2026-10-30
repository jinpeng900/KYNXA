namespace KYNXA_Desktop.Controls;

// Fixed geometry comes from Resources/UI/Icons and Styles/Icons.xaml keys.
// 几何由仓库图标和样式键预先生成，运行时不读取 SVG 或用户内容。
internal static class ThemeIconGeometry
{
    internal static readonly IReadOnlyDictionary<string, ThemeIconDefinition> Icons =
        new Dictionary<string, ThemeIconDefinition>(StringComparer.Ordinal)
    {
        ["IconSidebarToggle"] = new(24, 24, [new("M6,4 H18 A3,3 0 0 1 21,7 V17 A3,3 0 0 1 18,20 H6 A3,3 0 0 1 3,17 V7 A3,3 0 0 1 6,4 Z", 1.8, true, false), new("M9 4v16", 1.8, true, false)]),
        ["IconPermissionAsk"] = new(24, 24, [new("M8 13V6.5a1.5 1.5 0 0 1 3 0V11m0-1V4.5a1.5 1.5 0 0 1 3 0V11m0-1V6a1.5 1.5 0 0 1 3 0v6m0-2V8.5a1.5 1.5 0 0 1 3 0V15a6 6 0 0 1-6 6h-1.5a6 6 0 0 1-4.8-2.4L3.4 13a1.7 1.7 0 0 1 2.5-2.2L8 13Z", 1.6, true, false)]),
        ["IconPermissionSmart"] = new(24, 24, [new("m12 3 8 3v6c0 5-4.5 8-8 9-3.5-1-8-4-8-9V6l8-3Z", 1.6, true, false), new("m9 9 3 3-3 3m5 0h2", 1.6, true, false)]),
        ["IconPermissionFull"] = new(24, 24, [new("m12 3 8 3v6c0 5-4.5 8-8 9-3.5-1-8-4-8-9V6l8-3Z", 1.6, true, false), new("M12 7v6m0 3v.1", 1.6, true, false)]),
        ["IconSearch"] = new(24, 24, [new("M17,10.8 A6.2,6.2 0 1 1 4.6,10.8 A6.2,6.2 0 1 1 17,10.8 Z", 1.8, true, false), new("m15.5 15.5 4.2 4.2", 1.8, true, false)]),
        ["IconAdd"] = new(24, 24, [new("M12 4v16M4 12h16", 1.8, true, false)]),
        ["IconKnowledge"] = new(24, 24, [new("M3.5 5.2c3.2-1.3 5.8-.5 8.5 1.3v13c-2.7-1.8-5.3-2.6-8.5-1.3zM20.5 5.2c-3.2-1.3-5.8-.5-8.5 1.3v13c2.7-1.8 5.3-2.6 8.5-1.3z", 1.8, true, false)]),
        ["IconWork"] = new(24, 24, [new("M3 7.5h7l2-2h3.5a2 2 0 0 1 2 2v1H21v9.3a2.2 2.2 0 0 1-2.2 2.2H5.2A2.2 2.2 0 0 1 3 17.8z", 1.8, true, false)]),
        ["IconFolderAdd"] = new(20, 20, [new("M11 16H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3l2 2h6a2 2 0 0 1 2 2v3M2 7h15M15 12v6m-3-3h6", 1.4, true, false)]),
        ["IconChatAdd"] = new(20, 20, [new("M17 9.5a7 7 0 0 1-7 7H3l1-4a7 7 0 1 1 13-3Z", 1.4, true, false), new("M10 6.5v6m-3-3h6", 1.4, true, false)]),
        ["IconPinnedRecent"] = new(24, 24, [new("M10.5 20A8.5 8.5 0 1 1 20 10M11.5 6v5.5l-3 2", 1.7, true, false), new("M15 11h6l-1 4 2 2h-8l2-2-1-4ZM18 17v5", 1.7, true, false)]),
        ["IconPinnedProject"] = new(24, 24, [new("M11 19H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2M3 9h10", 1.7, true, false), new("M15 11h6l-1 4 2 2h-8l2-2-1-4ZM18 17v5", 1.7, true, false)]),
        ["IconPinnedChat"] = new(24, 24, [new("M11 18H9l-5 3v-5a3 3 0 0 1-1-2V7a3 3 0 0 1 3-3h12a3 3 0 0 1 3 3v2M7 8h9M7 12h4", 1.7, true, false), new("M15 11h6l-1 4 2 2h-8l2-2-1-4ZM18 17v5", 1.7, true, false)]),
        // Use quarter arcs to preserve the .8 radius despite float rounding at opposite endpoints.
        // 用四段圆弧保留 .8 半径，避免对径端点的浮点取整使圆心偏移。
        ["IconConnect"] = new(24, 24, [new("M5,4 H19 A2,2 0 0 1 21,6 V15 A2,2 0 0 1 19,17 H5 A2,2 0 0 1 3,15 V6 A2,2 0 0 1 5,4 Z", 1.8, true, false), new("M8 21h8M12 17v4", 1.8, true, false), new("M18.3,13.5 A0.8,0.8 0 0 1 17.5,14.3 A0.8,0.8 0 0 1 16.7,13.5 A0.8,0.8 0 0 1 17.5,12.7 A0.8,0.8 0 0 1 18.3,13.5 Z", 1.8, false, true)]),
        ["IconScheduledTask"] = new(24, 24, [new("M20.5,12 A8.5,8.5 0 1 1 3.5,12 A8.5,8.5 0 1 1 20.5,12 Z", 1.8, true, false), new("M12 7v5l3.5 2", 1.8, true, false)]),
        ["IconSkills"] = new(24, 24, [new("m12 3-9 4.5 9 4.5 9-4.5zM4.5 11l7.5 3.8 7.5-3.8M4.5 15l7.5 3.8 7.5-3.8", 1.8, true, false)]),
        ["IconModelManagement"] = new(24, 24, [new("M4 6h7m4 0h5M4 12h3m4 0h9M4 18h8m4 0h4", 1.8, true, false), new("M15,6 A2,2 0 1 1 11,6 A2,2 0 1 1 15,6 Z", 1.8, true, false), new("M11,12 A2,2 0 1 1 7,12 A2,2 0 1 1 11,12 Z", 1.8, true, false), new("M16,18 A2,2 0 1 1 12,18 A2,2 0 1 1 16,18 Z", 1.8, true, false)]),
        ["IconWebSearch"] = new(24, 24, [new("M21,12 A9,9 0 1 1 3,12 A9,9 0 1 1 21,12 Z", 1.8, true, false), new("M3.5 9h17M3.5 15h17M12 3c2.4 2.5 3.5 5.5 3.5 9s-1.1 6.5-3.5 9c-2.4-2.5-3.5-5.5-3.5-9S9.6 5.5 12 3z", 1.8, true, false)]),
        ["IconAttachment"] = new(24, 24, [new("m8.2 12.8 6.7-6.7a3.6 3.6 0 1 1 5.1 5.1l-9.2 9.2a5.2 5.2 0 0 1-7.4-7.4l9-9", 1.8, true, false)]),
        ["IconDeepThinking"] = new(24, 24, [new("M8.5 17.2h7M9.4 20h5.2M8.5 14.5c-1.5-1.1-2.5-2.9-2.5-5a6 6 0 1 1 12 0c0 2.1-1 3.9-2.5 5-.7.5-.9 1.1-.9 2.1H9.4c0-1-.2-1.6-.9-2.1z", 1.8, true, false)]),
        ["IconQuickMode"] = new(24, 24, [new("m13.5 2-8 12h6L10.5 22l8-12h-6z", 1.8, true, false)]),
        ["IconModelSelect"] = new(24, 24, [new("M4 6h7m4 0h5M4 12h3m4 0h9M4 18h8m4 0h4", 1.8, true, false), new("M15,6 A2,2 0 1 1 11,6 A2,2 0 1 1 15,6 Z", 1.8, true, false), new("M11,12 A2,2 0 1 1 7,12 A2,2 0 1 1 11,12 Z", 1.8, true, false), new("M16,18 A2,2 0 1 1 12,18 A2,2 0 1 1 16,18 Z", 1.8, true, false)]),
        ["IconSend"] = new(24, 24, [new("M12 20V4M6 10l6-6 6 6", 2, true, false)]),
        ["IconSendDisabled"] = new(24, 24, [new("M12 20V4M6 10l6-6 6 6", 2, true, false)]),
        ["IconSettings"] = new(24, 24, [new("M15,12 A3,3 0 1 1 9,12 A3,3 0 1 1 15,12 Z", 1.8, true, false), new("M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1-2.8 2.8-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.6v.2h-4V21a1.7 1.7 0 0 0-1-1.6 1.7 1.7 0 0 0-1.9.3l-.1.1L4.2 17l.1-.1a1.7 1.7 0 0 0 .3-1.9A1.7 1.7 0 0 0 3 14H2.8v-4H3a1.7 1.7 0 0 0 1.6-1 1.7 1.7 0 0 0-.3-1.9L4.2 7 7 4.2l.1.1A1.7 1.7 0 0 0 9 4.6a1.7 1.7 0 0 0 1-1.6v-.2h4V3a1.7 1.7 0 0 0 1 1.6 1.7 1.7 0 0 0 1.9-.3l.1-.1L19.8 7l-.1.1a1.7 1.7 0 0 0-.3 1.9 1.7 1.7 0 0 0 1.6 1h.2v4H21a1.7 1.7 0 0 0-1.6 1z", 1.8, true, false)]),
        ["IconChat"] = new(24, 24, [new("M20 15a3 3 0 0 1-3 3H9l-5 3v-6a3 3 0 0 1-1-2.2V7a3 3 0 0 1 3-3h11a3 3 0 0 1 3 3z", 1.8, true, false)]),
        ["IconDeviceStatus"] = new(24, 24, [new("M5,4 H19 A2,2 0 0 1 21,6 V15 A2,2 0 0 1 19,17 H5 A2,2 0 0 1 3,15 V6 A2,2 0 0 1 5,4 Z", 1.8, true, false), new("M8 21h8M12 17v4", 1.8, true, false)]),
        ["IconDocument"] = new(24, 24, [new("M6 2.5h8l4 4V21.5H6zM14 2.5v4h4", 1.8, true, false)]),
        ["IconHistory"] = new(24, 24, [new("M4.2 7.2A9 9 0 1 1 3 13M3 5v4h4", 1.8, true, false), new("M12 7v5l3 2", 1.8, true, false)])
    };
}
