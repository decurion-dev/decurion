"""Store or read a Claude profile token in Windows Credential Manager.

Store (prompts with hidden input, prints nothing back):
    python spike/wincred.py set A      # Max profile A, from `claude setup-token`
    python spike/wincred.py set api    # pay-per-use Anthropic API key
"""

import ctypes
import ctypes.wintypes as wt
import getpass
import sys

CRED_TYPE_GENERIC = 1
CRED_PERSIST_LOCAL_MACHINE = 2


class CREDENTIAL(ctypes.Structure):
    _fields_ = [("Flags", wt.DWORD), ("Type", wt.DWORD), ("TargetName", wt.LPWSTR),
                ("Comment", wt.LPWSTR), ("LastWritten", wt.FILETIME),
                ("CredentialBlobSize", wt.DWORD), ("CredentialBlob", ctypes.POINTER(ctypes.c_char)),
                ("Persist", wt.DWORD), ("AttributeCount", wt.DWORD), ("Attributes", ctypes.c_void_p),
                ("TargetAlias", wt.LPWSTR), ("UserName", wt.LPWSTR)]


advapi = ctypes.WinDLL("advapi32", use_last_error=True)


def target(profile):
    # "api" is the pay-per-use Anthropic API key; A and B are Max profiles.
    if profile.lower() == "api":
        return "decurion/anthropic-api"
    return f"decurion/claude-profile-{profile}"


def write(profile, secret):
    blob = secret.encode("utf-16-le")  # same encoding the keyring crate uses
    c = CREDENTIAL(Type=CRED_TYPE_GENERIC, TargetName=target(profile), Persist=CRED_PERSIST_LOCAL_MACHINE,
                   CredentialBlobSize=len(blob), UserName="claude",
                   CredentialBlob=ctypes.cast(ctypes.create_string_buffer(blob, len(blob)),
                                              ctypes.POINTER(ctypes.c_char)))
    if not advapi.CredWriteW(ctypes.byref(c), 0):
        raise OSError(ctypes.get_last_error(), "CredWriteW failed")


def read(profile):
    p = ctypes.POINTER(CREDENTIAL)()
    if not advapi.CredReadW(target(profile), CRED_TYPE_GENERIC, 0, ctypes.byref(p)):
        raise LookupError(f"no credential {target(profile)}; run: python spike/wincred.py set {profile}")
    try:
        return ctypes.string_at(p.contents.CredentialBlob, p.contents.CredentialBlobSize).decode("utf-16-le")
    finally:
        advapi.CredFree(p)


if __name__ == "__main__":
    if len(sys.argv) != 3 or sys.argv[1] != "set":
        sys.exit(__doc__)
    what = "pay-per-use API key" if sys.argv[2].lower() == "api" else f"token for Claude profile {sys.argv[2]}"
    tok = getpass.getpass(f"Paste the {what} (hidden): ").strip()
    if not tok.startswith("sk-ant-"):
        sys.exit("That does not look like a Claude token. Nothing saved.")
    write(sys.argv[2], tok)
    print(f"Saved to Windows Credential Manager as {target(sys.argv[2])}.")
