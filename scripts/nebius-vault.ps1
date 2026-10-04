param([ValidateSet('Import', 'ImportStdin', 'Status', 'Remove', 'Library')][string]$Action = 'Status')
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Security

function Get-NebiusVaultPath {
    Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Kyro\secrets\nebius.dpapi'
}

function Read-NebiusVault {
    $path = Get-NebiusVaultPath
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw 'Coffre Nebius absent : lancer nebius-vault.ps1 -Action Import.' }
    $item = Get-Item -LiteralPath $path
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or $item.Length -gt 16384) { throw 'Coffre Nebius non conforme.' }
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    foreach ($candidate in @((Split-Path -Parent $path), $path)) {
        if ((Get-Item -LiteralPath $candidate).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Coffre Nebius non conforme.' }
        $acl=Get-Acl -LiteralPath $candidate
        if (-not $acl.AreAccessRulesProtected -or $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $sid) { throw 'Coffre Nebius permissions invalides.' }
        foreach ($rule in $acl.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier])) {
            if ($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference.Value -ne $sid) { throw 'Coffre Nebius permissions trop larges.' }
        }
    }
    $plain = [Security.Cryptography.ProtectedData]::Unprotect([IO.File]::ReadAllBytes($path), $null, [Security.Cryptography.DataProtectionScope]::CurrentUser)
    if ($plain.Length -eq 0 -or $plain.Length -gt 8192) { [Array]::Clear($plain,0,$plain.Length); throw 'Coffre Nebius non conforme.' }
    return ,$plain
}

function Save-NebiusVault([byte[]]$Plain) {
    if ($Plain.Length -lt 16 -or $Plain.Length -gt 8192 -or ($Plain | Where-Object { $_ -lt 33 -or $_ -gt 126 })) { throw 'Format de clé invalide ; rien enregistré.' }
    $encrypted = [Security.Cryptography.ProtectedData]::Protect($Plain, $null, [Security.Cryptography.DataProtectionScope]::CurrentUser)
    $path = Get-NebiusVaultPath
    $directory = Split-Path -Parent $path
    [IO.Directory]::CreateDirectory($directory) | Out-Null
    foreach ($candidate in @($directory, $path)) {
        if ((Test-Path -LiteralPath $candidate) -and ((Get-Item -LiteralPath $candidate).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Coffre non conforme.' }
    }
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    & icacls $directory /inheritance:r /grant:r ('*'+$sid.Value+':(OI)(CI)F') >$null
    if ($LASTEXITCODE -ne 0) { throw 'Coffre Nebius permissions invalides.' }
    [IO.File]::WriteAllBytes($path, $encrypted)
    & icacls $path /inheritance:r /grant:r ('*'+$sid.Value+':F') >$null
    if ($LASTEXITCODE -ne 0) { throw 'Coffre Nebius permissions invalides.' }
}

if ($Action -eq 'Library') { return }
if ($Action -eq 'Status') {
    Write-Output ([pscustomobject]@{ present = (Test-Path -LiteralPath (Get-NebiusVaultPath) -PathType Leaf); protection = 'DPAPI CurrentUser'; plaintext_export = $false })
    return
}
if ($Action -eq 'Remove') {
    $path = Get-NebiusVaultPath
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path }
    Write-Output 'Coffre local supprimé. La clé distante reste inchangée.'
    return
}
if ($Action -eq 'ImportStdin') {
    # Non-interactive import only through a pipe, never a command argument or environment value.
    if ([Console]::IsInputRedirected) { $line = [Console]::ReadLine() }
    else {
        $secure = Read-Host 'Clé Nebius (saisie masquée)' -AsSecureString
        $pointer = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
        try { $line = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($pointer) }
        finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($pointer); $secure.Dispose() }
    }
    $plain = [Text.Encoding]::UTF8.GetBytes($line)
    try { Save-NebiusVault $plain; Write-Output 'Coffre DPAPI enregistré.' }
    finally { [Array]::Clear($plain, 0, $plain.Length); $line = $null }
    return
}

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$form = New-Object Windows.Forms.Form
$form.Text = 'Kyro — clé Nebius, import local chiffré'
$form.Width = 570
$form.Height = 210
$form.StartPosition = 'CenterScreen'
$form.TopMost = $true
$label = New-Object Windows.Forms.Label
$label.Text = 'Collez la clé Nebius ici. Elle sera chiffrée pour votre compte Windows, hors du dépôt.'
$label.SetBounds(18, 18, 520, 44)
$inputBox = New-Object Windows.Forms.TextBox
$inputBox.UseSystemPasswordChar = $true
$inputBox.SetBounds(18, 70, 520, 25)
$saveButton = New-Object Windows.Forms.Button
$saveButton.Text = 'Chiffrer et enregistrer'
$saveButton.SetBounds(330, 115, 205, 30)
$saveButton.DialogResult = [Windows.Forms.DialogResult]::OK
$form.AcceptButton = $saveButton
$form.Controls.AddRange(@($label, $inputBox, $saveButton))
try {
    if ($form.ShowDialog() -ne [Windows.Forms.DialogResult]::OK) { return }
    $value = $inputBox.Text.Trim()
    if ($value.Length -lt 16 -or $value.Length -gt 8192 -or $value -match '\s') { throw 'Format de clé invalide ; rien enregistré.' }
    $plain = [Text.Encoding]::UTF8.GetBytes($value)
    try {
        Save-NebiusVault $plain
    } finally { if ($null -ne $plain) { [Array]::Clear($plain, 0, $plain.Length) }; $value = $null; $inputBox.Clear() }
} finally { $form.Dispose() }
