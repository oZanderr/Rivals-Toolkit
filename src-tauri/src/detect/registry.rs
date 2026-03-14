use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE},
};

/// Read a REG_SZ string value from HKLM.
pub(super) fn hklm_str(subkey: &str, value: &str) -> Option<String> {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(subkey)
        .ok()?
        .get_value::<String, _>(value)
        .ok()
}

/// Read a REG_SZ string value from HKCU.
pub(super) fn hkcu_str(subkey: &str, value: &str) -> Option<String> {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(subkey)
        .ok()?
        .get_value::<String, _>(value)
        .ok()
}
