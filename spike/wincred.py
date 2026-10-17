"""Store or read an agent credential in Windows Credential Manager.

Store (prompts with hidden input, prints nothing back):
    python spike/wincred.py set A       # Max profile A, from `claude setup-token`
    python spike/wincred.py set api     # pay-per-use Anthropic API key
    python spike/wincred.py set openai  # OpenAI API key, for ChatGPT work
    python spike/wincred.py set xai     # xAI API key, for Grok work
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


# Pay-per-use keys: where each is kept, what it is called, and how it starts.
KEYS = {
    "api": ("decurion/anthropic-api", "pay-per-use Anthropic API key", "sk-ant-"),
    "openai": ("decurion/openai-api", "OpenAI API key", "sk-"),
    "xai": ("decurion/xai-api", "xAI API key", "xai-"),
}


def target(profile):
    # A and B are Max profiles; the rest are pay-per-use keys.
    if profile.lower() in KEYS:
        return KEYS[profile.lower()][0]
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
    if len(sys.argv) != 3 or sys.argv[1] != "set" or sys.argv[2].lower() not in [*KEYS, "a", "b"]:
        sys.exit(__doc__)
    name = sys.argv[2].lower() if sys.argv[2].lower() in KEYS else sys.argv[2].upper()
    _, what, prefix = KEYS.get(name, (None, f"token for Claude profile {name}", "sk-ant-"))
    tok = getpass.getpass(f"Paste the {what} (hidden): ").strip()
    if not tok.startswith(prefix):
        sys.exit(f"That does not look like a {what}. Nothing saved.")
    write(name, tok)
    print(f"Saved to Windows Credential Manager as {target(name)}.")
