use crate::{debug, warn};
use std::ffi::c_uint;

#[unsafe(no_mangle)]
pub extern "C" fn SteamAPI_RestartAppIfNecessary(un_own_app_id: c_uint) -> bool {
    debug!(
        Steam,
        "SteamAPI_RestartAppIfNecessary({un_own_app_id}) -> false"
    );
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn SteamAPI_Init() -> bool {
    warn!(Steam, "SteamAPI_Init unhandled");
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn SteamAPI_InitSafe() -> bool {
    warn!(Steam, "SteamAPI_InitSafe unhandled");
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn SteamAPI_IsSteamRunning() -> bool {
    warn!(Steam, "SteamAPI_IsSteamRunning unhandled");
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn SteamAPI_Shutdown() {
    warn!(Steam, "SteamAPI_Shutdown unhandled");
}
