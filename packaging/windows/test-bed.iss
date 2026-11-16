; Test Bed for Windows: installs the bundle the Windows workflow builds.
; Per user, no administrator rights: the share service runs as the signed-in
; user anyway. An HKCU Run entry starts it at sign-in or, with "Control
; administrator windows" on in Test Bed, a scheduled task with the user's
; highest rights; the installer can't kill that one, so it asks it to stop.
;
;   ISCC /DAppVersion=2.1.0 /DSourceDir=dist\TestBed /DOutputDir=. test-bed.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\dist\TestBed"
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

[Setup]
AppId={{6C1F6E0B-3B7E-4E52-9D0A-7F4C2B9E5A11}
AppName=Test Bed
AppVersion={#AppVersion}
AppPublisher=Pocket Studios LLC
AppPublisherURL=https://github.com/PocketStudiosLLC/Test-Bed
DefaultDirName={autopf}\Test Bed
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename=TestBed-Setup-{#AppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; The share service keeps its files open; close it to replace them.
CloseApplications=force
UninstallDisplayIcon={app}\bin\test-bed.exe
UninstallDisplayName=Test Bed

[Tasks]
Name: desktopicon; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: recursesubdirs createallsubdirs ignoreversion

[InstallDelete]
; Files from an older version that this one no longer ships.
Type: filesandordirs; Name: "{app}\bin"
Type: filesandordirs; Name: "{app}\lib"
Type: filesandordirs; Name: "{app}\share"

[Icons]
Name: "{autoprograms}\Test Bed"; Filename: "{app}\bin\test-bed.exe"; WorkingDir: "{app}\bin"
Name: "{autodesktop}\Test Bed"; Filename: "{app}\bin\test-bed.exe"; WorkingDir: "{app}\bin"; Tasks: desktopicon

[Run]
; Sharing was on before the upgrade: start it again from the new files.
Filename: "{app}\bin\test-bed-share.exe"; Parameters: "run"; Flags: nowait runhidden; Check: ShareWasOn
; It ran as administrator: start the task again (no prompt needed).
Filename: "{sys}\schtasks.exe"; Parameters: "/Run /TN ""Test Bed Share"""; Flags: runhidden; Check: AdminShareWasOn
Filename: "{app}\bin\test-bed.exe"; Description: "Open Test Bed"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Asked first: as administrator it can't be killed from here.
Filename: "{app}\bin\test-bed-share.exe"; Parameters: "stop"; Flags: runhidden; RunOnceId: "AskShareToStop"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM test-bed-share.exe"; Flags: runhidden; RunOnceId: "StopShare"
Filename: "{sys}\reg.exe"; Parameters: "delete HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v ""Test Bed Share"" /f"; Flags: runhidden; RunOnceId: "RemoveSignIn"

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  TaskName = 'Test Bed Share';
var
  WasSharing: Boolean;
  WasAdminSharing: Boolean;

function ShareWasOn: Boolean;
begin
  Result := WasSharing;
end;

function AdminShareWasOn: Boolean;
begin
  Result := WasAdminSharing;
end;

// The scheduled task "Control administrator windows" sets up.
function TaskExists: Boolean;
var
  Code: Integer;
begin
  Result := Exec(ExpandConstant('{sys}\schtasks.exe'), '/Query /TN "' + TaskName + '"', '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
end;

// Stops sharing however it runs: asked over its pipe (the only way when it
// runs as administrator), ended as a task, then killed (older versions).
procedure StopSharing;
var
  Code: Integer;
begin
  if FileExists(ExpandConstant('{app}\bin\test-bed-share.exe')) then
    Exec(ExpandConstant('{app}\bin\test-bed-share.exe'), 'stop', '', SW_HIDE, ewWaitUntilTerminated, Code);
  if WasAdminSharing then
    Exec(ExpandConstant('{sys}\schtasks.exe'), '/End /TN "' + TaskName + '"', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM test-bed-share.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(1000);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  WasSharing := RegValueExists(HKEY_CURRENT_USER, RunKey, 'Test Bed Share');
  WasAdminSharing := TaskExists;
  // Stop sharing so its files can be replaced.
  StopSharing;
  Result := '';
end;

// The task can only be removed as administrator: Windows asks.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Code: Integer;
begin
  if (CurUninstallStep = usUninstall) and TaskExists then
    ShellExec('runas', ExpandConstant('{sys}\schtasks.exe'), '/Delete /TN "' + TaskName + '" /F', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  // Point the sign-in entry at this install (it may name an older unzipped copy).
  if (CurStep = ssPostInstall) and WasSharing then
    RegWriteStringValue(HKEY_CURRENT_USER, RunKey, 'Test Bed Share',
      '"' + ExpandConstant('{app}\bin\test-bed-share.exe') + '" run');
end;
