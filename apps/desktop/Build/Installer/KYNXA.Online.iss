; Small online bootstrap. The full installer owns installation, upgrades and uninstall registration.
; 轻量联网引导程序；真正安装、升级及卸载登记由完整安装器统一负责。
#ifndef OnlinePayloadRoot
  #error OnlinePayloadRoot must point to the prepared online bootstrap files.
#endif
#ifndef PackageOutputDir
  #define PackageOutputDir "..\..\..\..\artifacts\installer\output"
#endif
#ifndef PackageVersion
  #define PackageVersion "1.5.0"
#endif
#ifndef IconFile
  #define IconFile "..\..\Assets\AppIcon.ico"
#endif
#ifndef BrandImageFile
  #define BrandImageFile "..\..\Resources\UI\Brand\kynxa-logo.png"
#endif

[Setup]
AppId=KYNXA.OnlineBootstrap
AppName=KYNXA Online
AppVersion={#PackageVersion}
AppPublisher=KYNXA
AppPublisherURL=https://github.com/jinpeng900/KYNXA
SetupIconFile={#IconFile}
OutputDir={#PackageOutputDir}
OutputBaseFilename=KYNXA-{#PackageVersion}-windows-x64-online-setup
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
PrivilegesRequired=lowest
CreateAppDir=no
Uninstallable=no
WizardStyle=modern
DisableWelcomePage=yes
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
DisableFinishedPage=yes
ShowLanguageDialog=no
Compression=lzma2/fast
SolidCompression=yes
SetupLogging=yes

[Files]
Source: "{#OnlinePayloadRoot}\Start-KynxaOnline.ps1"; Flags: dontcopy
Source: "{#OnlinePayloadRoot}\Get-KynxaInstaller.ps1"; Flags: dontcopy
Source: "{#OnlinePayloadRoot}\RuntimeDownloads.psm1"; Flags: dontcopy
Source: "{#OnlinePayloadRoot}\installer-release.json"; Flags: dontcopy
Source: "{#BrandImageFile}"; DestName: "kynxa-installer-logo.png"; Flags: dontcopy

[Code]
procedure CurPageChanged(CurPageID: Integer);
begin
  { The download window is the only bootstrap UI; no empty installation wizard is exposed. }
  { 引导阶段只显示下载窗口，不暴露空白安装向导。 }
  WizardForm.Hide;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
  Parameters: String;
begin
  if CurStep <> ssPostInstall then Exit;
  WizardForm.Hide;
  ExtractTemporaryFile('Start-KynxaOnline.ps1');
  ExtractTemporaryFile('Get-KynxaInstaller.ps1');
  ExtractTemporaryFile('RuntimeDownloads.psm1');
  ExtractTemporaryFile('installer-release.json');
  ExtractTemporaryFile('kynxa-installer-logo.png');
  Parameters := '-NoLogo -NoProfile -STA -ExecutionPolicy Bypass -File ' +
    AddQuotes(ExpandConstant('{tmp}\Start-KynxaOnline.ps1')) +
    ' -ManifestPath ' + AddQuotes(ExpandConstant('{tmp}\installer-release.json')) +
    ' -BootstrapPath ' + AddQuotes(ExpandConstant('{srcexe}'));
  if not Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    Parameters, ExpandConstant('{tmp}'), SW_HIDE, ewWaitUntilTerminated, ResultCode) then
    RaiseException('The KYNXA online download window could not be started.');
  if ResultCode <> 0 then
    RaiseException('The KYNXA online download window exited with code ' + IntToStr(ResultCode) + '.');
end;
