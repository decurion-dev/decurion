; Decurion installer for Windows (Inno Setup 6).
;
; Built by .github/workflows/release.yml:
;   iscc /DAppVersion=0.1.0 /DBinDir=<folder with the .exe files> /DOutDir=<out> decurion.iss
;
; Installs for the current user only, so it needs no administrator prompt.
; Missing helpers (Docker Desktop, Git, Claude Code, Tailscale) are offered
; on one page and installed with winget after Decurion itself is in place.

#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
#ifndef BinDir
  #define BinDir "..\..\target\release"
#endif
#ifndef OutDir
  #define OutDir "..\..\target\installer"
#endif
#define RepoRoot "..\.."

[Setup]
AppId={{D1C3222D-059F-4FE0-A01C-927A5CA3321F}
AppName=Decurion
AppVersion={#AppVersion}
AppVerName=Decurion {#AppVersion}
AppPublisher=The Decurion contributors
AppPublisherURL=https://github.com/decurion-dev/decurion
AppSupportURL=https://github.com/decurion-dev/decurion/issues
AppUpdatesURL=https://github.com/decurion-dev/decurion/releases
DefaultDirName={autopf}\Decurion
DefaultGroupName=Decurion
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041
LicenseFile={#RepoRoot}\LICENSE
OutputDir={#OutDir}
OutputBaseFilename=Decurion-Setup-{#AppVersion}-x64
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
UninstallDisplayName=Decurion
UninstallDisplayIcon={sys}\WindowsPowerShell\v1.0\powershell.exe

[Tasks]
Name: "desktopicon"; Description: "Put a Decurion shortcut on my desktop"; Flags: unchecked

[Files]
Source: "{#BinDir}\decurion-hub.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\decurion-runner.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "start-decurion.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "save-claude-sign-in.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\docker\claude-worker\Dockerfile"; DestDir: "{app}\docker\claude-worker"; Flags: ignoreversion
Source: "{#RepoRoot}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\NOTICE"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#RepoRoot}\README.md"; DestDir: "{app}"; Flags: ignoreversion isreadme

[Icons]
Name: "{autoprograms}\Decurion"; Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; \
  Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\start-decurion.ps1"""; \
  WorkingDir: "{app}"; Comment: "Start Decurion and open your board"
Name: "{autoprograms}\Decurion - Save a Claude sign-in"; Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; \
  Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\save-claude-sign-in.ps1"""; \
  WorkingDir: "{app}"; Comment: "Save a Claude account sign-in or API key for Decurion"
Name: "{autodesktop}\Decurion"; Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; \
  Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\start-decurion.ps1"""; \
  WorkingDir: "{app}"; Comment: "Start Decurion and open your board"; Tasks: desktopicon

[Run]
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; \
  Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\start-decurion.ps1"""; \
  WorkingDir: "{app}"; Description: "Open Decurion now"; Flags: postinstall nowait skipifsilent

[Messages]
FinishedLabel=Decurion is installed. Open it any time from the Start menu.%n%nIf Docker Desktop was just installed, restart your PC before you open Decurion.

[Code]
type
  THelper = record
    Name: String;
    WingetId: String;
    Site: String;
    Missing: Boolean;
  end;

var
  Helpers: array[0..3] of THelper;
  HelperPage: TInputOptionWizardPage;
  ManualPage: TOutputMsgWizardPage;
  HasWinget: Boolean;
  OptionIndex: array[0..3] of Integer;

function OnPath(Exe: String): Boolean;
var
  Code: Integer;
begin
  Result := Exec(ExpandConstant('{cmd}'), '/C where ' + Exe + ' >nul 2>&1', '',
    SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
end;

procedure AddHelper(I: Integer; Name, WingetId, Site: String; Found: Boolean);
begin
  Helpers[I].Name := Name;
  Helpers[I].WingetId := WingetId;
  Helpers[I].Site := Site;
  Helpers[I].Missing := not Found;
end;

function AnyMissing: Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 0 to 3 do
    if Helpers[I].Missing then
      Result := True;
end;

procedure InitializeWizard;
var
  I: Integer;
  Manual: String;
begin
  AddHelper(0, 'Docker Desktop (the closed rooms your agents work in)', 'Docker.DockerDesktop',
    'https://www.docker.com/products/docker-desktop/',
    OnPath('docker') or FileExists(ExpandConstant('{commonpf64}\Docker\Docker\Docker Desktop.exe')));
  AddHelper(1, 'Git (keeps every version of your code)', 'Git.Git',
    'https://git-scm.com/download/win', OnPath('git'));
  AddHelper(2, 'Claude Code (signs in your Claude accounts)', 'Anthropic.ClaudeCode',
    'https://code.claude.com/docs/en/setup',
    OnPath('claude') or FileExists(ExpandConstant('{%USERPROFILE}\.local\bin\claude.exe')));
  AddHelper(3, 'Tailscale (lets your phone reach Decurion safely)', 'tailscale.tailscale',
    'https://tailscale.com/download/windows',
    OnPath('tailscale') or FileExists(ExpandConstant('{commonpf64}\Tailscale\tailscale.exe')));
  HasWinget := OnPath('winget');

  if not AnyMissing then
    exit;

  if HasWinget then
  begin
    HelperPage := CreateInputOptionPage(wpSelectTasks,
      'Helpers Decurion needs',
      'These are not on your PC yet.',
      'Decurion can install them for you right after it installs itself. ' +
      'Some of them may ask for your permission to make changes.',
      False, False);
    for I := 0 to 3 do
      if Helpers[I].Missing then
      begin
        OptionIndex[I] := HelperPage.Add(Helpers[I].Name);
        HelperPage.Values[OptionIndex[I]] := True;
      end;
  end
  else
  begin
    Manual := 'Decurion needs these, and they are not on your PC yet. ' +
      'Install them from these websites after this setup finishes:' + #13#10#13#10;
    for I := 0 to 3 do
      if Helpers[I].Missing then
        Manual := Manual + Helpers[I].Name + #13#10 + '    ' + Helpers[I].Site + #13#10#13#10;
    ManualPage := CreateOutputMsgPage(wpSelectTasks, 'Helpers Decurion needs',
      'These are not on your PC yet.', Manual);
  end;
end;

// 3010 means the helper installed and wants a restart, which the last page mentions.
procedure CurStepChanged(CurStep: TSetupStep);
var
  I, Code: Integer;
begin
  if (CurStep <> ssPostInstall) or (HelperPage = nil) or WizardSilent then
    exit;
  for I := 0 to 3 do
    if Helpers[I].Missing and HelperPage.Values[OptionIndex[I]] then
    begin
      WizardForm.StatusLabel.Caption := 'Installing ' + Helpers[I].Name + '...';
      if not Exec(ExpandConstant('{cmd}'),
        '/C winget install --exact --id ' + Helpers[I].WingetId +
        ' --accept-source-agreements --accept-package-agreements',
        '', SW_SHOW, ewWaitUntilTerminated, Code) or ((Code <> 0) and (Code <> 3010)) then
        MsgBox(Helpers[I].Name + ' did not install. You can get it from ' +
          Helpers[I].Site, mbInformation, MB_OK);
    end;
end;
