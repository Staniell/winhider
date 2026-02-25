; =============================================================================
; WinHider Application - Installer Script
; =============================================================================
;
; Filename: installer.iss
; Author: bigwiz
; Description: Inno Setup configuration for generating the WinHider installer.
;              This script packages the 64-bit binaries, handles file copying,
;              creates shortcuts with correct working directories, and manages
;              uninstallation cleanup.
;
; Key Operations:
; - Defines application metadata (Name, Version, Publisher)
; - Installs 64-bit executables and payload DLL
; - Configures Start Menu and Desktop shortcuts
; - Sets up modern wizard style with custom banner assets
;
; Designed At - Bitmutex Technologies
; =============================================================================


#define MyAppName "WinHider"
#define MyCLIAppName "WinHider CLI"
#define MyAppPublisher "Bitmutex Technologies"
#define MyAppURL "https://github.com/aamitn/winhider"

; Read version from appver.txt (generated at build time from git tag)
#define MyAppVersion ReadIni(SourcePath + "\..\appver.txt", "", "", "1.0.7")

[Code]
function GetAppVersion(Param: String): String;
var
  VersionFile: String;
  Lines: TArrayOfString;
begin
  VersionFile := ExpandConstant('{src}\appver.txt');
  if FileExists(VersionFile) then
  begin
    if LoadStringsFromFile(VersionFile, Lines) and (GetArrayLength(Lines) > 0) then
    begin
      Result := Trim(Lines[0]);
      Exit;
    end;
  end;
  Result := '{#MyAppVersion}';
end;

[Setup]
; Wizard Pages
DisableWelcomePage = no
LicenseFile=..\LICENSE
; Wizard Banner Images
WizardSmallImageFile=.\installer_assets\whicon-bitmap.bmp
WizardImageFile=.\installer_assets\banner.bmp
; NOTE: The value of AppId uniquely identifies this application. Do not use the same AppId value in installers for other applications.
; (To generate a new GUID, click Tools | Generate GUID inside the IDE.)
AppId={{4896775D-F364-4AF8-AD6C-946EE5F49D95}
;SignTool=winsdk_signtool
AppName={#MyAppName}
AppVersion={#MyAppVersion}
;AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DisableProgramGroupPage=yes
; Uncomment the following line to run in non administrative install mode (install for current user only.)
;PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputBaseFilename=WinhiderInstaller
Compression=lzma
SolidCompression=yes
WizardStyle=modern
; x64 only — no 32-bit support
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=whicon.ico
UninstallDisplayIcon={app}\winhider.exe
UninstallDisplayName={#MyAppName}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: checkedonce

[Files]
Source: "..\target\x86_64-pc-windows-msvc\release\*.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\x86_64-pc-windows-msvc\release\winhider_payload.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\appver.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; GUI APP
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\winhider.exe"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\winhider.exe"; Tasks: desktopicon
; CLI APP
Name: "{autoprograms}\{#MyCLIAppName}"; Filename: "{app}\winhider-cli.exe"
Name: "{autodesktop}\{#MyCLIAppName}"; Filename: "{app}\winhider-cli.exe"; Tasks: desktopicon

[Run]
;Run GUI APP
Filename: "{app}\winhider.exe"; \
Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')} GUI}"; \
Flags: nowait postinstall skipifsilent unchecked shellexec; \
WorkingDir: "{app}"

;Run CLI APP
Filename: "{app}\winhider-cli.exe"; \
Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')} CLI}"; \
Flags: nowait postinstall skipifsilent unchecked shellexec; \
WorkingDir: "{app}"

[UninstallDelete]
Type: dirifempty; Name: "{app}"
