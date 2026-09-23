using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using System.Numerics;
using Windows.Graphics;

namespace KYNXA_Desktop;

public sealed partial class ModelManagementWindow : Window
{
    public ModelManagementWindow()
    {
        InitializeComponent();

        ExtendsContentIntoTitleBar = true;
        SetTitleBar(ModelTitleBar);
        AppWindow.SetIcon("Assets/AppIcon.ico");

        if (AppWindowTitleBar.IsCustomizationSupported())
        {
            AppWindow.TitleBar.ButtonBackgroundColor = Microsoft.UI.Colors.Transparent;
            AppWindow.TitleBar.ButtonInactiveBackgroundColor = Microsoft.UI.Colors.Transparent;
        }

        SizeAndCenterWindow();
        ShowCategory(0);
    }

    private void SizeAndCenterWindow()
    {
        DisplayArea area = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Primary);
        RectInt32 workArea = area.WorkArea;
        int width = Math.Min(1000, Math.Max(720, (int)(workArea.Width * 0.88)));
        int height = Math.Min(680, Math.Max(520, (int)(workArea.Height * 0.84)));
        int x = workArea.X + ((workArea.Width - width) / 2);
        int y = workArea.Y + ((workArea.Height - height) / 2);
        AppWindow.MoveAndResize(new RectInt32(x, y, width, height));
    }

    private void LocalModelsButton_Click(object sender, RoutedEventArgs e) => ShowCategory(0);
    private void OfficialModelsButton_Click(object sender, RoutedEventArgs e) => ShowCategory(1);
    private void CustomModelsButton_Click(object sender, RoutedEventArgs e) => ShowCategory(2);

    public void ShowCustomModels() => ShowCategory(2);

    private void ShowCategory(int index)
    {
        LocalModelsHost.Visibility = index == 0 ? Visibility.Visible : Visibility.Collapsed;
        OfficialModelsHost.Visibility = index == 1 ? Visibility.Visible : Visibility.Collapsed;
        CustomModelsHost.Visibility = index == 2 ? Visibility.Visible : Visibility.Collapsed;
        ModelCategoryPill.Translation = new Vector3(index * 120, 0, 8);
    }

    private void AddCustomConnectionButton_Click(object sender, RoutedEventArgs e)
    {
        CustomConnectionList.Visibility = Visibility.Collapsed;
        CustomConnectionEditor.Visibility = Visibility.Visible;
        ConnectionDiagnostics.Visibility = Visibility.Collapsed;
    }

    private void CloseCustomEditorButton_Click(object sender, RoutedEventArgs e)
    {
        CustomConnectionEditor.Visibility = Visibility.Collapsed;
        CustomConnectionList.Visibility = Visibility.Visible;
    }

    private void TestConnectionButton_Click(object sender, RoutedEventArgs e) =>
        ConnectionDiagnostics.Visibility = Visibility.Visible;
}
