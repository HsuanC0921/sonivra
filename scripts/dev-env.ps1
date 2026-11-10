param([string]$RustRoot, [string]$SdkRoot)
$ErrorActionPreference = 'Stop'
if ($RustRoot) {
    $env:RUSTUP_HOME = Join-Path $RustRoot 'rustup'
    $env:CARGO_HOME = Join-Path $RustRoot 'cargo'
    $env:PATH = (Join-Path $env:CARGO_HOME 'bin') + ';' + $env:PATH
}
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if ($SdkRoot) {
    $cl = & $vswhere -products '*' -find 'VC\Tools\MSVC\*\bin\Hostx64\x64\cl.exe' | Select-Object -First 1
    if (-not $cl) { throw 'A native MSVC compiler is required.' }
    $bin = Split-Path -Parent $cl
    $env:PATH = $bin + ';' + $env:PATH
    $env:LIB = "$SdkRoot\crt\lib\x86_64;$SdkRoot\sdk\lib\um\x86_64;$SdkRoot\sdk\lib\ucrt\x86_64"
    $env:INCLUDE = "$SdkRoot\crt\include;$SdkRoot\sdk\include\ucrt;$SdkRoot\sdk\include\shared;$SdkRoot\sdk\include\um;$SdkRoot\sdk\include\winrt"
    $env:CC = $cl
    $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = Join-Path $bin 'link.exe'
    return
}
$vcvars = & $vswhere -products '*' -find 'VC\Auxiliary\Build\vcvars64.bat' | Select-Object -First 1
if (-not $vcvars) { throw 'Install Visual Studio C++ Desktop Build Tools and the Windows SDK.' }
$lines = & $env:ComSpec /c "`"$vcvars`" >nul && set"
if ($LASTEXITCODE -ne 0) { throw 'Could not load MSVC build environment.' }
foreach ($line in $lines) {
    $name, $value = $line -split '=', 2
    if ($name -in @('Path','INCLUDE','LIB','LIBPATH','VCToolsInstallDir','WindowsSdkDir','WindowsSDKVersion')) {
        [Environment]::SetEnvironmentVariable($name, $value, 'Process')
    }
}
