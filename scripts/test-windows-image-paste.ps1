param([switch]$Native, [string]$RustTestExe, [string]$RustTestName)
$ErrorActionPreference = 'Stop'
$framework = [System.Runtime.InteropServices.RuntimeEnvironment]::GetRuntimeDirectory()
$references = @('System.dll', "$framework/WPF/UIAutomationClient.dll", "$framework/WPF/UIAutomationTypes.dll", "$framework/WPF/WindowsBase.dll")
$source = [IO.File]::ReadAllText("$PSScriptRoot/windows-image-paste.cs") + [IO.File]::ReadAllText("$PSScriptRoot/windows-image-paste.tests.cs")
Add-Type -TypeDefinition $source -ReferencedAssemblies $references

function Assert-True([bool]$Value, [string]$Message) {
    if (-not $Value) { throw $Message }
}

[ Bingo.ImagePaste.ContractTests ]::Policy()
if ($Native) {
    [Bingo.ImagePaste.ContractTests]::NativeConsole()
    [Bingo.ImagePaste.ContractTests]::Lifecycle("$PSScriptRoot/windows-image-paste.cs")
}
if ($RustTestExe) { [Bingo.ImagePaste.ContractTests]::Crossterm($RustTestExe, $RustTestName) }
Assert-True ([Bingo.ImagePaste.Protocol]::Ready -ceq 'BINGO_IMAGE_PASTE_READY_V1') 'readiness protocol changed'
Write-Output 'Windows image paste policy tests passed.'
if ($Native) { Write-Output 'Isolated Windows console native tests passed.' }
