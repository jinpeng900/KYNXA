; KYNXA Windows x64 installer. The release staging directory is the only payload source.
; KYNXA Windows x64 安装器。发布暂存目录是唯一的软件载荷来源。
#ifndef PayloadRoot
  #error PayloadRoot must point to the prepared release staging directory.
#endif
#ifndef PackageOutputDir
  #define PackageOutputDir "..\..\..\..\artifacts\installer\output"
#endif
#ifndef PackageVersion
  #define PackageVersion "1.8.8"
#endif
#ifndef LauncherFile
  #define LauncherFile "KYNXA.Desktop.exe"
#endif
#ifndef IconFile
  #define IconFile "..\..\Assets\AppIcon.ico"
#endif
#ifndef BrandImageFile
  #define BrandImageFile "..\..\Resources\UI\Brand\kynxa-logo.png"
#endif

[Setup]
AppId={{4255F6EE-5944-46C1-9D9C-D8C1959952D1}
AppName=KYNXA
AppVersion={#PackageVersion}
AppPublisher=KYNXA
AppPublisherURL=https://github.com/jinpeng900/KYNXA
AppSupportURL=https://github.com/jinpeng900/KYNXA/issues
DefaultDirName={localappdata}\Programs\KYNXA
DefaultGroupName=KYNXA
UninstallDisplayName=KYNXA
UninstallDisplayIcon={app}\Assets\AppIcon.ico
SetupIconFile={#IconFile}
OutputDir={#PackageOutputDir}
OutputBaseFilename=KYNXA-{#PackageVersion}-windows-x64-setup
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
PrivilegesRequired=lowest
WizardStyle=modern
WizardSizePercent=120,150
WizardResizable=no
DisableWelcomePage=yes
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
UsePreviousAppDir=yes
UsePreviousGroup=yes
UsePreviousLanguage=yes
ShowLanguageDialog=no
; Keep the full runtime package quick to build and extract on ordinary developer machines.
; 完整运行资源采用快速压缩，缩短普通开发机的打包及用户解压等待。
Compression=zip/6
SolidCompression=no
DiskSpanning=no
CloseApplications=yes
RestartApplications=no
Uninstallable=yes
UninstallLogMode=append
SetupLogging=yes

[Languages]
Name: "zhcn"; MessagesFile: "compiler:Default.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[LangOptions]
zhcn.LanguageName=简体中文
zhcn.LanguageID=$0804
zhcn.DialogFontName=Microsoft YaHei UI
zhcn.DialogFontSize=9
en.LanguageName=English
en.DialogFontName=Segoe UI
en.DialogFontSize=9

[Messages]
zhcn.ButtonBack=< 返回
zhcn.ButtonNext=安装(&I)
zhcn.ButtonInstall=安装(&I)
zhcn.ButtonCancel=取消
zhcn.ButtonFinish=完成
zhcn.WizardInstalling=正在安装
zhcn.InstallingLabel=正在安装 KYNXA，请稍候。
zhcn.FinishedHeadingLabel=KYNXA 已安装
zhcn.FinishedLabelNoIcons=软件和中英文检索资源已安装。勾选的本地对话模型将在首次启动后后台下载，可在设置中查看进度。
zhcn.FinishedLabel=软件和中英文检索资源已安装。勾选的本地对话模型将在首次启动后后台下载，可在设置中查看进度。
zhcn.RunEntryExec=启动 KYNXA
zhcn.ConfirmUninstall=是否卸载 KYNXA？聊天、模型、用户工具和数据会保留。

[Files]
; Package only staged runtime files, not a development output or existing installation.
; 只打包暂存的运行文件，不直接读取开发输出或现有安装。
; Exclude private directories only at the root, preserving nested third-party names.
; 私有目录仅从根目录排除，保留第三方依赖内的同名目录。
Source: "{#PayloadRoot}\*"; DestDir: "{app}"; Excludes: "\Data,\Data\*,\Extensions,\Extensions\*,\.git,\.git\*,*.pdb,\prerequisites,\prerequisites\*,\runtime\rerank\builtin-multilingual-reranker,\runtime\rerank\builtin-multilingual-reranker\*"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#BrandImageFile}"; DestName: "kynxa-installer-logo.png"; Flags: dontcopy
Source: "Configure-Kynxa.ps1"; DestName: "Configure-Kynxa-Read.ps1"; Flags: dontcopy
Source: "{#PayloadRoot}\prerequisites\MicrosoftEdgeWebView2RuntimeInstallerX64.exe"; Flags: dontcopy skipifsourcedoesntexist

[InstallDelete]
; Remove only the previously shipped model files on upgrade; preserve unknown files and user data.
; 升级时只删除旧版本随包模型的精确文件，保留未知文件和用户数据。
Type: files; Name: "{app}\runtime\rerank\builtin-multilingual-reranker\onnx\model_quantized.onnx"
Type: files; Name: "{app}\runtime\rerank\builtin-multilingual-reranker\manifest.json"
Type: files; Name: "{app}\runtime\rerank\builtin-multilingual-reranker\config.json"
Type: files; Name: "{app}\runtime\rerank\builtin-multilingual-reranker\tokenizer_config.json"
Type: files; Name: "{app}\runtime\rerank\builtin-multilingual-reranker\special_tokens_map.json"
Type: files; Name: "{app}\runtime\rerank\builtin-multilingual-reranker\tokenizer.json"
Type: files; Name: "{app}\runtime\rerank\builtin-multilingual-reranker\MODEL_CARD.md"
Type: files; Name: "{app}\runtime\rerank\builtin-multilingual-reranker\CONVERSION_MODEL_CARD.md"
Type: files; Name: "{app}\runtime\rerank\builtin-multilingual-reranker\LICENSE"
Type: dirifempty; Name: "{app}\runtime\rerank\builtin-multilingual-reranker\onnx"
Type: dirifempty; Name: "{app}\runtime\rerank\builtin-multilingual-reranker"
Type: dirifempty; Name: "{app}\runtime\rerank"

[Icons]
Name: "{userprograms}\KYNXA"; Filename: "{app}\{#LauncherFile}"; WorkingDir: "{app}"; IconFilename: "{app}\Assets\AppIcon.ico"
Name: "{userdesktop}\KYNXA"; Filename: "{app}\{#LauncherFile}"; WorkingDir: "{app}"; IconFilename: "{app}\Assets\AppIcon.ico"; Check: WantsDesktopShortcut

[Run]
Filename: "{app}\{#LauncherFile}"; WorkingDir: "{app}"; Description: "{code:GetLaunchCaption}"; Flags: postinstall shellexec skipifsilent

[Code]
type
  TPhysicalMemoryStatus = record
    Length: Cardinal;
    Load: Cardinal;
    TotalPhysical: Int64;
    AvailablePhysical: Int64;
    TotalPageFile: Int64;
    AvailablePageFile: Int64;
    TotalVirtual: Int64;
    AvailableVirtual: Int64;
    AvailableExtendedVirtual: Int64;
  end;

function GlobalMemoryStatusEx(var MemoryStatus: TPhysicalMemoryStatus): Boolean;
  external 'GlobalMemoryStatusEx@kernel32.dll stdcall';
function SetWindowTheme(Window: HWND; SubAppName, SubIdList: String): Longint;
  external 'SetWindowTheme@uxtheme.dll stdcall';
function CreateRoundRectRgn(Left, Top, Right, Bottom, Width, Height: Integer): THandle;
  external 'CreateRoundRectRgn@gdi32.dll stdcall';
function SetWindowRgn(Window: HWND; Region: THandle; Redraw: Boolean): Integer;
  external 'SetWindowRgn@user32.dll stdcall';
function DeleteObject(GraphicObject: THandle): Boolean;
  external 'DeleteObject@gdi32.dll stdcall';

var
  OptionsPage: TWizardPage;
  BrandImage: TBitmapImage;
  BrandTitle, HardwareLabel, ModelLabel, ModelHint: TNewStaticText;
  ApplicationLabel, DataLabel, ExtensionLabel, LanguageLabel: TNewStaticText;
  DiskLabel, BaseResourcesHint: TNewStaticText;
  ApplicationEdit, DataEdit, ExtensionEdit: TNewEdit;
  ApplicationBrowse, DataBrowse, ExtensionBrowse: TNewButton;
  ModelCombo, LanguageCombo: TNewComboBox;
  DownloadModelCheck, DesktopShortcutCheck: TNewCheckBox;
  ModelIds: TStringList;
  TotalMemoryGb: Integer;
  LastApplicationDirectory: String;
  IsUpdatingDirectories: Boolean;

function IsEnglish: Boolean;
begin
  Result := Assigned(LanguageCombo) and (LanguageCombo.ItemIndex = 1);
end;

function LocalText(ChineseText, EnglishText: String): String;
begin
  if IsEnglish then Result := EnglishText else Result := ChineseText;
end;

function GetLaunchCaption(Param: String): String;
begin
  Result := LocalText('启动 KYNXA', 'Launch KYNXA');
end;

function WantsDesktopShortcut: Boolean;
begin
  Result := DesktopShortcutCheck.Checked;
end;

function PageY(Coordinate: Integer): Integer;
begin
  { Fit one compact page to the actual DPI-scaled surface, including small screens. }
  { 按实际 DPI 页面高度安排纵向位置，完整显示底部路径和空间信息。 }
  Result := MulDiv(ScaleY(Coordinate), OptionsPage.SurfaceHeight, ScaleY(455));
end;

procedure ApplyNeutralStyle(Control: TWinControl);
var
  Region: THandle;
begin
  { Keep Windows keyboard/accessibility behavior while removing themed blue borders. }
  { 保留 Windows 键盘和辅助功能行为，同时去掉系统主题蓝色边框。 }
  SetWindowTheme(Control.Handle, '', '');
  Region := CreateRoundRectRgn(0, 0, Control.Width + 1, Control.Height + 1,
    ScaleX(8), ScaleY(8));
  if SetWindowRgn(Control.Handle, Region, True) = 0 then DeleteObject(Region);
end;

function NewLabel(Top, Height: Integer): TNewStaticText;
begin
  Result := TNewStaticText.Create(WizardForm);
  Result.Parent := OptionsPage.Surface;
  Result.SetBounds(0, PageY(Top), OptionsPage.SurfaceWidth, PageY(Height));
  Result.AutoSize := False;
  Result.Font.Color := $003B3936;
end;

procedure UpdateDiskHint;
var
  FreeBytes, TotalBytes: Int64;
  FreeText: String;
begin
  FreeText := LocalText('读取中', 'checking');
  if GetSpaceOnDisk64(AddBackslash(ExtractFileDrive(ApplicationEdit.Text)), FreeBytes, TotalBytes) then
    FreeText := Format('%.1f GiB', [FreeBytes / 1073741824.0]);
  DiskLabel.Caption := LocalText('软件目录可用空间：', 'Free at application location: ') + FreeText;
end;

procedure UpdateModelHint(Sender: TObject);
var
  Details: String;
begin
  case ModelCombo.ItemIndex of
    0: Details := LocalText('下载约 1.7 GiB，建议内存 8 GB', 'About 1.7 GiB download; 8 GB RAM recommended');
    1: Details := LocalText('下载约 2.3 GiB，建议内存 8 GB', 'About 2.3 GiB download; 8 GB RAM recommended');
    2: Details := LocalText('下载约 4.7 GiB，建议内存 16 GB', 'About 4.7 GiB download; 16 GB RAM recommended');
    3: Details := LocalText('下载约 8.4 GiB，建议内存 24 GB', 'About 8.4 GiB download; 24 GB RAM recommended');
    4: Details := LocalText('下载约 17.3 GiB，建议内存 32 GB', 'About 17.3 GiB download; 32 GB RAM recommended');
    5: Details := LocalText('下载约 18.4 GiB，建议内存 48 GB', 'About 18.4 GiB download; 48 GB RAM recommended');
    6: Details := LocalText('下载约 32.4 GiB，建议内存 64 GB', 'About 32.4 GiB download; 64 GB RAM recommended');
  end;
  ModelHint.Caption := Details + #13#10 + LocalText(
    '勾选后首次启动后台下载；支持断点续传，进度在设置中查看。',
    'If selected, downloads after first launch. Resume and progress are available in Settings.');
  ModelCombo.Enabled := DownloadModelCheck.Checked;
end;

procedure UpdateLanguage(Sender: TObject);
begin
  OptionsPage.Caption := LocalText('安装 KYNXA', 'Install KYNXA');
  OptionsPage.Description := LocalText('选择位置与可选模型，安装后即可使用。',
    'Choose locations and an optional model. No developer environment is required.');
  BrandTitle.Caption := 'KYNXA';
  HardwareLabel.Caption := Format(LocalText('本机内存 %d GB · 已为你选择推荐模型',
    '%d GB RAM detected · a suitable model is selected'), [TotalMemoryGb]);
  ModelLabel.Caption := LocalText('可选本地对话模型', 'Optional local chat model');
  DownloadModelCheck.Caption := LocalText('下载并启用', 'Download and enable');
  ApplicationLabel.Caption := LocalText('软件安装位置', 'Application');
  DataLabel.Caption := LocalText('数据存储位置', 'Data');
  ExtensionLabel.Caption := LocalText('用户工具位置', 'User tools');
  LanguageLabel.Caption := LocalText('界面语言', 'Language');
  BaseResourcesHint.Caption := LocalText(
    '基础资源已随包提供。可选模型自动切换可用下载源，支持断点续传。',
    'Base resources are included. Optional model downloads resume and switch available sources automatically.');
  ApplicationBrowse.Caption := LocalText('浏览', 'Browse');
  DataBrowse.Caption := ApplicationBrowse.Caption;
  ExtensionBrowse.Caption := ApplicationBrowse.Caption;
  DesktopShortcutCheck.Caption := LocalText('创建桌面快捷方式', 'Create a desktop shortcut');
  WizardForm.NextButton.Caption := LocalText('安装', 'Install');
  WizardForm.CancelButton.Caption := LocalText('取消', 'Cancel');
  UpdateModelHint(Sender);
  UpdateDiskHint;
end;

procedure ApplicationDirectoryChanged(Sender: TObject);
begin
  if IsUpdatingDirectories then Exit;
  IsUpdatingDirectories := True;
  try
    { User-overridden data/tool directories stay independent during later app path edits. }
    { 只跟随仍为默认值的数据/工具路径，保留用户已单独选择的位置。 }
    if CompareText(DataEdit.Text, AddBackslash(LastApplicationDirectory) + 'Data') = 0 then
      DataEdit.Text := AddBackslash(ApplicationEdit.Text) + 'Data';
    if CompareText(ExtensionEdit.Text, AddBackslash(LastApplicationDirectory) + 'Extensions') = 0 then
      ExtensionEdit.Text := AddBackslash(ApplicationEdit.Text) + 'Extensions';
    LastApplicationDirectory := ApplicationEdit.Text;
    WizardForm.DirEdit.Text := ApplicationEdit.Text;
    UpdateDiskHint;
  finally
    IsUpdatingDirectories := False;
  end;
end;

procedure BrowseDirectory(Sender: TObject);
var
  SelectedDirectory: String;
  TargetEdit: TNewEdit;
begin
  if Sender = ApplicationBrowse then TargetEdit := ApplicationEdit
  else if Sender = DataBrowse then TargetEdit := DataEdit
  else TargetEdit := ExtensionEdit;
  SelectedDirectory := TargetEdit.Text;
  if BrowseForFolder(LocalText('选择文件夹', 'Choose a folder'), SelectedDirectory, True) then
    TargetEdit.Text := SelectedDirectory;
end;

procedure AddDirectoryControls(Top: Integer; var Caption: TNewStaticText;
  var Edit: TNewEdit; var Browse: TNewButton);
begin
  Caption := NewLabel(Top + 5, 24);
  Caption.Width := ScaleX(100);
  Edit := TNewEdit.Create(WizardForm);
  Edit.Parent := OptionsPage.Surface;
  Edit.SetBounds(ScaleX(106), PageY(Top), OptionsPage.SurfaceWidth - ScaleX(180), PageY(30));
  Edit.Font.Name := 'Segoe UI';
  Browse := TNewButton.Create(WizardForm);
  Browse.Parent := OptionsPage.Surface;
  Browse.SetBounds(OptionsPage.SurfaceWidth - ScaleX(66), PageY(Top), ScaleX(66), PageY(30));
  Browse.OnClick := @BrowseDirectory;
  ApplyNeutralStyle(Edit);
  ApplyNeutralStyle(Browse);
end;

procedure ReadExistingConfiguration(var SavedLanguage: String);
var
  ResultCode: Integer;
  Helper, OutputFile, Parameters: String;
begin
  if not FileExists(AddBackslash(LastApplicationDirectory) + 'kynxa-installation.json') then Exit;
  { The shared helper resolves current migration pointers; the GUI does not parse storage JSON. }
  { 共享只读入口解析当前迁移指针，安装界面不另建一套存储 JSON 解析规则。 }
  ExtractTemporaryFile('Configure-Kynxa-Read.ps1');
  Helper := ExpandConstant('{tmp}\Configure-Kynxa-Read.ps1');
  OutputFile := ExpandConstant('{tmp}\kynxa-existing-installation.ini');
  Parameters := '-NoLogo -NoProfile -ExecutionPolicy Bypass -File ' + AddQuotes(Helper) +
    ' -ReadConfiguration -ApplicationRoot ' + AddQuotes(LastApplicationDirectory) +
    ' -OutputFile ' + AddQuotes(OutputFile);
  if Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    Parameters, ExpandConstant('{tmp}'), SW_HIDE, ewWaitUntilTerminated, ResultCode) and
    (ResultCode = 0) and FileExists(OutputFile) then
  begin
    DataEdit.Text := GetIniString('Kynxa', 'DataRoot', DataEdit.Text, OutputFile);
    ExtensionEdit.Text := GetIniString('Kynxa', 'ExtensionRoot', ExtensionEdit.Text, OutputFile);
    SavedLanguage := GetIniString('Kynxa', 'Language', SavedLanguage, OutputFile);
  end
  else Log('Existing KYNXA paths could not be read; installer settings remain available.');
end;

procedure InitializeWizard;
var
  MemoryStatus: TPhysicalMemoryStatus;
  RecommendedIndex: Integer;
  SavedLanguage: String;
begin
  MemoryStatus.Length := 64;
  TotalMemoryGb := 8;
  if GlobalMemoryStatusEx(MemoryStatus) then
    TotalMemoryGb := Round(MemoryStatus.TotalPhysical / 1073741824.0);
  RecommendedIndex := 0;
  if TotalMemoryGb >= 12 then RecommendedIndex := 1;
  if TotalMemoryGb >= 16 then RecommendedIndex := 2;
  if TotalMemoryGb >= 24 then RecommendedIndex := 3;
  if TotalMemoryGb >= 32 then RecommendedIndex := 4;
  if TotalMemoryGb >= 96 then RecommendedIndex := 5;
  if TotalMemoryGb >= 128 then RecommendedIndex := 6;

  WizardForm.Color := $00F7F6F4;
  WizardForm.MainPanel.Color := $00F7F6F4;
  WizardForm.PageNameLabel.Font.Color := $003B3936;
  WizardForm.PageDescriptionLabel.Font.Color := $00757068;
  OptionsPage := CreateCustomPage(wpWelcome, '', '');
  OptionsPage.Surface.Color := $00F7F6F4;
  BrandImage := TBitmapImage.Create(WizardForm);
  BrandImage.Parent := OptionsPage.Surface;
  BrandImage.SetBounds(0, 0, PageY(46), PageY(46));
  BrandImage.Stretch := True;
  ExtractTemporaryFile('kynxa-installer-logo.png');
  BrandImage.PngImage.LoadFromFile(ExpandConstant('{tmp}\kynxa-installer-logo.png'));
  BrandTitle := NewLabel(7, 36);
  BrandTitle.Left := ScaleX(60);
  BrandTitle.Width := OptionsPage.SurfaceWidth - ScaleX(60);
  BrandTitle.Font.Size := 21;
  BrandTitle.Font.Style := [fsBold];
  HardwareLabel := NewLabel(58, 23);
  HardwareLabel.Font.Color := $00757068;
  ModelLabel := NewLabel(91, 25);
  ModelLabel.Font.Style := [fsBold];
  DownloadModelCheck := TNewCheckBox.Create(WizardForm);
  DownloadModelCheck.Parent := OptionsPage.Surface;
  DownloadModelCheck.SetBounds(OptionsPage.SurfaceWidth - ScaleX(158), PageY(89), ScaleX(158), PageY(25));
  DownloadModelCheck.Checked := False;
  DownloadModelCheck.OnClick := @UpdateModelHint;
  ApplyNeutralStyle(DownloadModelCheck);
  ModelCombo := TNewComboBox.Create(WizardForm);
  ModelCombo.Parent := OptionsPage.Surface;
  ModelCombo.SetBounds(0, PageY(121), OptionsPage.SurfaceWidth, PageY(29));
  ModelCombo.Style := csDropDownList;
  ModelIds := TStringList.Create;
  ModelCombo.Items.Add('Qwen3 1.7B Q8_0'); ModelIds.Add('q3-1p7b-q80');
  ModelCombo.Items.Add('Qwen3 4B Q4_K_M'); ModelIds.Add('q3-4b-q4km');
  ModelCombo.Items.Add('Qwen3 8B Q4_K_M'); ModelIds.Add('q3-8b-q4km');
  ModelCombo.Items.Add('Qwen3 14B Q4_K_M'); ModelIds.Add('q3-14b-q4km');
  ModelCombo.Items.Add('Qwen3 30B-A3B Q4_K_M'); ModelIds.Add('q3-30b-a3b-q4km');
  ModelCombo.Items.Add('Qwen3 32B Q4_K_M'); ModelIds.Add('q3-32b-q4km');
  ModelCombo.Items.Add('Qwen3 32B Q8_0'); ModelIds.Add('q3-32b-q80');
  ModelCombo.ItemIndex := RecommendedIndex;
  ModelCombo.OnChange := @UpdateModelHint;
  ApplyNeutralStyle(ModelCombo);
  ModelHint := NewLabel(157, 40);
  ModelHint.Font.Color := $00757068;
  ModelHint.WordWrap := True;
  AddDirectoryControls(210, ApplicationLabel, ApplicationEdit, ApplicationBrowse);
  AddDirectoryControls(251, DataLabel, DataEdit, DataBrowse);
  AddDirectoryControls(292, ExtensionLabel, ExtensionEdit, ExtensionBrowse);
  LastApplicationDirectory := WizardDirValue;
  ApplicationEdit.Text := LastApplicationDirectory;
  DataEdit.Text := GetPreviousData('DataRoot', AddBackslash(LastApplicationDirectory) + 'Data');
  ExtensionEdit.Text := GetPreviousData('ExtensionRoot', AddBackslash(LastApplicationDirectory) + 'Extensions');
  ApplicationEdit.OnChange := @ApplicationDirectoryChanged;
  LanguageLabel := NewLabel(339, 24);
  LanguageLabel.Width := ScaleX(100);
  LanguageCombo := TNewComboBox.Create(WizardForm);
  LanguageCombo.Parent := OptionsPage.Surface;
  LanguageCombo.SetBounds(ScaleX(106), PageY(333), ScaleX(190), PageY(30));
  LanguageCombo.Style := csDropDownList;
  LanguageCombo.Items.Add('简体中文');
  LanguageCombo.Items.Add('English');
  SavedLanguage := GetPreviousData('UiLanguage', ActiveLanguage);
  ReadExistingConfiguration(SavedLanguage);
  if SavedLanguage = 'en' then LanguageCombo.ItemIndex := 1 else LanguageCombo.ItemIndex := 0;
  LanguageCombo.OnChange := @UpdateLanguage;
  ApplyNeutralStyle(LanguageCombo);
  BaseResourcesHint := NewLabel(378, 38);
  BaseResourcesHint.WordWrap := True;
  BaseResourcesHint.Font.Color := $00757068;
  DiskLabel := NewLabel(421, 24);
  DiskLabel.Font.Color := $00757068;
  DesktopShortcutCheck := TNewCheckBox.Create(WizardForm);
  DesktopShortcutCheck.Parent := WizardForm;
  DesktopShortcutCheck.SetBounds(WizardForm.CancelButton.Left - ScaleX(350),
    WizardForm.NextButton.Top, ScaleX(240), WizardForm.NextButton.Height);
  DesktopShortcutCheck.Left := ScaleX(18);
  DesktopShortcutCheck.Checked := GetPreviousData('DesktopShortcut', '1') = '1';
  ApplyNeutralStyle(DesktopShortcutCheck);
  ApplyNeutralStyle(WizardForm.NextButton);
  ApplyNeutralStyle(WizardForm.CancelButton);
  UpdateLanguage(LanguageCombo);
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := (PageID = wpSelectDir) or (PageID = wpSelectProgramGroup) or
    (PageID = wpSelectTasks) or (PageID = wpReady);
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  DesktopShortcutCheck.Visible := CurPageID = OptionsPage.ID;
  if CurPageID = OptionsPage.ID then
  begin
    WizardForm.BackButton.Visible := False;
    UpdateLanguage(LanguageCombo);
  end;
  if CurPageID = wpInstalling then
  begin
    WizardForm.PageNameLabel.Caption := LocalText('正在安装 KYNXA', 'Installing KYNXA');
    WizardForm.PageDescriptionLabel.Caption := LocalText('安装软件和中英文检索资源。',
      'Installing application and multilingual retrieval resources.');
  end;
  if CurPageID = wpFinished then
  begin
    WizardForm.NextButton.Caption := LocalText('完成', 'Finish');
    WizardForm.FinishedHeadingLabel.Caption := LocalText('KYNXA 已安装', 'KYNXA is installed');
    WizardForm.FinishedLabel.Caption := LocalText(
      '软件和检索资源已就绪。可选对话模型将在首次启动后后台下载，进度可在设置中查看。',
      'Application and retrieval resources are ready. Optional chat model downloads start on first launch; check Settings for progress.');
  end;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  Directories: array[0..2] of String;
  Index: Integer;
begin
  Result := True;
  if CurPageID <> OptionsPage.ID then Exit;
  Directories[0] := Trim(ApplicationEdit.Text);
  Directories[1] := Trim(DataEdit.Text);
  Directories[2] := Trim(ExtensionEdit.Text);
  for Index := 0 to 2 do
  begin
    if (Directories[Index] = '') or (ExtractFileDrive(Directories[Index]) = '') or
      (Pos('"', Directories[Index]) > 0) then
    begin
      MsgBox(LocalText('请选择完整、有效的安装和数据路径。',
        'Choose valid absolute application, data and tool locations.'), mbError, MB_OK);
      Result := False;
      Exit;
    end;
  end;
  { Data directories must remain distinct from the application root and one another. }
  { 数据与工具目录独立于软件根目录，也不能互相重叠为同一个目录。 }
  if (CompareText(RemoveBackslashUnlessRoot(Directories[0]), RemoveBackslashUnlessRoot(Directories[1])) = 0) or
    (CompareText(RemoveBackslashUnlessRoot(Directories[0]), RemoveBackslashUnlessRoot(Directories[2])) = 0) or
    (CompareText(RemoveBackslashUnlessRoot(Directories[1]), RemoveBackslashUnlessRoot(Directories[2])) = 0) then
  begin
    MsgBox(LocalText('软件、数据和工具请分别使用独立目录。',
      'Application, data and tools must use separate directories.'), mbError, MB_OK);
    Result := False;
    Exit;
  end;
  WizardForm.DirEdit.Text := Directories[0];
end;

procedure RegisterPreviousData(PreviousDataKey: Integer);
var
  Language: String;
begin
  if IsEnglish then Language := 'en' else Language := 'zh-CN';
  SetPreviousData(PreviousDataKey, 'DataRoot', Trim(DataEdit.Text));
  SetPreviousData(PreviousDataKey, 'ExtensionRoot', Trim(ExtensionEdit.Text));
  SetPreviousData(PreviousDataKey, 'UiLanguage', Language);
  if DesktopShortcutCheck.Checked then
    SetPreviousData(PreviousDataKey, 'DesktopShortcut', '1')
  else SetPreviousData(PreviousDataKey, 'DesktopShortcut', '0');
end;

function HasWebView2: Boolean;
var
  Roots: array[0..2] of Integer;
  RootIndex, ClientIndex: Integer;
  Clients: TArrayOfString;
  Name, Version, ClientKey: String;
begin
  Result := False;
  Roots[0] := HKCU;
  Roots[1] := HKLM32;
  Roots[2] := HKLM64;
  for RootIndex := 0 to 2 do
    if RegGetSubkeyNames(Roots[RootIndex], 'Software\Microsoft\EdgeUpdate\Clients', Clients) then
      for ClientIndex := 0 to GetArrayLength(Clients) - 1 do
      begin
        ClientKey := 'Software\Microsoft\EdgeUpdate\Clients\' + Clients[ClientIndex];
        if RegQueryStringValue(Roots[RootIndex], ClientKey, 'name', Name) and
          (Pos('WebView2', Name) > 0) and
          RegQueryStringValue(Roots[RootIndex], ClientKey, 'pv', Version) and
          (Version <> '') and (Version <> '0.0.0.0') then
        begin
          Result := True;
          Exit;
        end;
      end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
  Helper, Parameters, Language: String;
begin
  if CurStep <> ssPostInstall then Exit;
  if not HasWebView2 then
  begin
    WizardForm.StatusLabel.Caption := LocalText('正在安装 WebView2 运行时…',
      'Installing the WebView2 runtime...');
    ExtractTemporaryFile('MicrosoftEdgeWebView2RuntimeInstallerX64.exe');
    if not Exec(ExpandConstant('{tmp}\MicrosoftEdgeWebView2RuntimeInstallerX64.exe'),
      '/silent /install', '', SW_HIDE, ewWaitUntilTerminated, ResultCode) or
      ((ResultCode <> 0) and (ResultCode <> 3010) and (not HasWebView2)) then
      RaiseException(LocalText('WebView2 安装失败。请查看安装日志后重新运行安装器。',
        'WebView2 installation failed. Review the setup log and run the installer again.'));
  end;
  WizardForm.StatusLabel.Caption := LocalText('正在配置存储、语言与模型…',
    'Configuring storage, language and models...');
  Helper := ExpandConstant('{app}\installer\Configure-Kynxa.ps1');
  if IsEnglish then Language := 'en' else Language := 'zh-CN';
  Parameters := '-NoLogo -NoProfile -ExecutionPolicy Bypass -File ' + AddQuotes(Helper) +
    ' -ApplicationRoot ' + AddQuotes(ExpandConstant('{app}')) +
    ' -DataRoot ' + AddQuotes(Trim(DataEdit.Text)) +
    ' -ExtensionRoot ' + AddQuotes(Trim(ExtensionEdit.Text)) +
    ' -Language ' + AddQuotes(Language) +
    ' -DownloadSource auto';
  if DownloadModelCheck.Checked then Parameters := Parameters +
    ' -ModelId ' + AddQuotes(ModelIds[ModelCombo.ItemIndex]);
  if not Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    Parameters, ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, ResultCode) or
    (ResultCode <> 0) then
    RaiseException(LocalText('安装配置失败，请查看软件目录 installer\configuration.log 后重新运行安装器。已安装文件和数据会保留。',
      'Configuration failed. Review installer\configuration.log and rerun setup. Installed files and data are retained.'));
end;

procedure DeinitializeSetup;
begin
  if Assigned(ModelIds) then ModelIds.Free;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ResultCode: Integer;
  Helper, Parameters: String;
begin
  if CurUninstallStep <> usUninstall then Exit;
  Helper := ExpandConstant('{app}\installer\Remove-Kynxa.ps1');
  if not FileExists(Helper) then Exit;
  { The helper stops only processes owned by this application directory. }
  { 此入口只停止本软件目录的进程，不删除聊天、模型、工具或其它用户数据。 }
  Parameters := '-NoLogo -NoProfile -ExecutionPolicy Bypass -File ' + AddQuotes(Helper) +
    ' -ApplicationRoot ' + AddQuotes(ExpandConstant('{app}'));
  if not Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    Parameters, ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, ResultCode) or
    (ResultCode <> 0) then
    Log('KYNXA process cleanup did not complete; user data remains unchanged.');
end;
