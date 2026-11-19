; Inno Setup script for gitcrafted's Windows installer. Built by the release workflow:
;   iscc /DAppVersion=0.2.0 /DOutputName=gitcrafted-v0.2.0-windows-x86_64-setup packaging\windows\gitcrafted.iss
; Installs per user (no administrator rights needed) unless the user picks "all users".

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef OutputName
  #define OutputName "gitcrafted-setup"
#endif
#define Root "..\.."

[Setup]
AppId={{86F6EE51-215B-489E-BB29-402DC3170FC5}
AppName=gitcrafted
AppVersion={#AppVersion}
AppVerName=gitcrafted {#AppVersion}
AppPublisher=Towhidul I Chowdhury
AppPublisherURL=https://github.com/itowhid06/gitcrafted
AppSupportURL=https://github.com/itowhid06/gitcrafted/issues
AppUpdatesURL=https://github.com/itowhid06/gitcrafted/releases
DefaultDirName={autopf}\gitcrafted
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#Root}\dist
OutputBaseFilename={#OutputName}
SetupIconFile={#Root}\assets\app-icon\gitcrafted.ico
UninstallDisplayIcon={app}\gitcrafted.exe
UninstallDisplayName=gitcrafted
LicenseFile={#Root}\LICENSE-MIT
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ChangesAssociations=no

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "foldermenu"; Description: "Add ""Open in gitcrafted"" to the right-click menu of folders"; GroupDescription: "Explorer:"

[Files]
Source: "{#Root}\target\release\gitcrafted.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\ATTRIBUTION.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\gitcrafted"; Filename: "{app}\gitcrafted.exe"
Name: "{autodesktop}\gitcrafted"; Filename: "{app}\gitcrafted.exe"; Tasks: desktopicon

[Registry]
; "Open in gitcrafted" on a folder and on a folder's background (HKCU or HKLM, per install mode).
Root: HKA; Subkey: "Software\Classes\Directory\shell\gitcrafted"; ValueType: string; ValueName: ""; ValueData: "Open in gitcrafted"; Tasks: foldermenu; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Directory\shell\gitcrafted"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\gitcrafted.exe"""; Tasks: foldermenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\gitcrafted\command"; ValueType: string; ValueName: ""; ValueData: """{app}\gitcrafted.exe"" ""%1"""; Tasks: foldermenu
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\gitcrafted"; ValueType: string; ValueName: ""; ValueData: "Open in gitcrafted"; Tasks: foldermenu; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\gitcrafted"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\gitcrafted.exe"""; Tasks: foldermenu
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\gitcrafted\command"; ValueType: string; ValueName: ""; ValueData: """{app}\gitcrafted.exe"" ""%V"""; Tasks: foldermenu

[Run]
Filename: "{app}\gitcrafted.exe"; Description: "{cm:LaunchProgram,gitcrafted}"; Flags: nowait postinstall skipifsilent
