; Inno Setup script for PolyRec. Compiled in CI via `iscc /DMyAppVersion=X.Y.Z installer\polyrec.iss`
; (Inno Setup 6 ships preinstalled on GitHub's windows-latest runner image).
;
; This produces an alternative to the portable zip release: a normal
; double-click installer with a Start Menu shortcut and an Add/Remove
; Programs entry, for users who don't want to unzip and locate an exe
; themselves. The portable zip keeps being published too -- it's what
; winget/Scoop-style package managers expect.

#define MyAppName "PolyRec"
#define MyAppPublisher "yusukensanta"
#define MyAppURL "https://github.com/yusukensanta/polyrec"
#define MyAppExeName "polyrec.exe"

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif

[Setup]
; Fixed GUID -- must never change across versions, it's how Windows/Inno Setup
; recognize "this is an upgrade of the same app" rather than a separate install.
AppId={{7F2E4C9A-3B1D-4E6F-8C2A-1D9E5F7B3A6C}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}/releases
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
OutputDir=..
OutputBaseFilename=polyrec-{#MyAppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
LicenseFile=..\LICENSE
; Unsigned binary -- see SECURITY.md's "out of scope" note on SmartScreen warnings.
; Nothing in this script can suppress that; only a real code-signing cert would.
; Supports the in-app self-updater (src/self_update.rs): it launches this
; installer silently while PolyRec is (about to be) still running, so the
; installer needs to be able to close/reopen it around the file replace
; rather than just failing on a locked polyrec.exe.
CloseApplications=yes
RestartApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\polyrec.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent
