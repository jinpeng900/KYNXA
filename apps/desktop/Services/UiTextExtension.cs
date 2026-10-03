using Microsoft.UI.Xaml.Markup;

namespace KYNXA_Desktop.Services;

public sealed class UiTextExtension : MarkupExtension
{
    public string Key { get; set; } = string.Empty;

    protected override object ProvideValue() => UiText.Get(Key);
}
