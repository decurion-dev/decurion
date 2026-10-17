# Saves a Claude sign-in for Decurion in Windows Credential Manager, where the
# runner reads it. Does the same job as spike/wincred.py without needing Python.
#
# Max profiles A and B are stored as decurion/claude-profile-A or -B, and the
# pay-per-use key as decurion/anthropic-api, encoded as UTF-16 like the keyring
# crate expects. The token is never shown or written anywhere else.

$ErrorActionPreference = 'Stop'
$Host.UI.RawUI.WindowTitle = 'Decurion: save a Claude sign-in'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class DecurionCred {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct CREDENTIAL {
        public uint Flags; public uint Type; public string TargetName; public string Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public uint CredentialBlobSize; public IntPtr CredentialBlob; public uint Persist;
        public uint AttributeCount; public IntPtr Attributes; public string TargetAlias; public string UserName;
    }
    [DllImport("advapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool CredWriteW(ref CREDENTIAL c, uint flags);
    public static void Write(string target, string secret) {
        byte[] blob = System.Text.Encoding.Unicode.GetBytes(secret);
        IntPtr p = Marshal.AllocHGlobal(blob.Length);
        try {
            Marshal.Copy(blob, 0, p, blob.Length);
            var c = new CREDENTIAL {
                Type = 1, TargetName = target, UserName = "claude", Persist = 2,
                CredentialBlobSize = (uint)blob.Length, CredentialBlob = p
            };
            if (!CredWriteW(ref c, 0)) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        } finally {
            for (int i = 0; i < blob.Length; i++) Marshal.WriteByte(p, i, 0);
            Marshal.FreeHGlobal(p);
            Array.Clear(blob, 0, blob.Length);
        }
    }
}
'@

Write-Host 'Which sign-in do you want to save?' -ForegroundColor Cyan
Write-Host '  A    Claude Max account A'
Write-Host '  B    Claude Max account B'
Write-Host '  api  Pay-per-use Anthropic API key'
$choice = (Read-Host 'Type A, B or api').Trim()
switch ($choice.ToLower()) {
    'a' { $target = 'decurion/claude-profile-A'; $what = 'Claude account A' }
    'b' { $target = 'decurion/claude-profile-B'; $what = 'Claude account B' }
    'api' { $target = 'decurion/anthropic-api'; $what = 'the pay-per-use API key' }
    default { Write-Host 'Nothing saved. Please type A, B or api.' -ForegroundColor Yellow; Read-Host 'Press Enter to close'; exit 1 }
}

if ($target -ne 'decurion/anthropic-api') {
    if (Get-Command claude -ErrorAction SilentlyContinue) {
        Write-Host ''
        Write-Host "Claude will open a sign-in page. Sign in with $what." -ForegroundColor Cyan
        Write-Host 'When it shows a long code that starts with sk-ant-, copy it, then come back here.'
        Read-Host 'Press Enter to open the sign-in page'
        claude setup-token
    } else {
        Write-Host 'Claude Code is not installed, so you need a token from another PC: run "claude setup-token" there.' -ForegroundColor Yellow
    }
}

Write-Host ''
$secure = Read-Host "Paste the code for $what (it stays hidden)" -AsSecureString
$bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
try {
    $token = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr).Trim()
} finally {
    [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr)
}
if (-not $token.StartsWith('sk-ant-')) {
    Write-Host 'That does not look like a Claude code. Nothing saved.' -ForegroundColor Yellow
    Read-Host 'Press Enter to close'
    exit 1
}
[DecurionCred]::Write($target, $token)
$token = $null
Write-Host "Saved. Decurion will use it for $what." -ForegroundColor Green
Read-Host 'Press Enter to close'
