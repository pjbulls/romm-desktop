//! Raw bindings to [argosy-sigil](https://github.com/rommapp/argosy-sigil), the library Argosy
//! uses to decide what a game's save is and to put one back.
//!
//! Only the part of `sigil.h` that save sync needs is declared: identification, the save-unit
//! request, and `sigil_collect` / `sigil_restore`. Every `#[repr(C)]` struct is a field-by-field
//! transcription of the header; the tests at the bottom pin their size and offsets against a C
//! compiler's output, so a header change that would break the layout fails loudly here instead
//! of corrupting memory.
//!
//! Safe wrappers live in `romm_core::unit`.

#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_int, c_void};

// ---- result codes ------------------------------------------------------------------------

pub const SIGIL_OK: c_int = 0;
pub const SIGIL_ERR_INVALID_ARG: c_int = -1;
pub const SIGIL_ERR_IO: c_int = -2;
pub const SIGIL_ERR_UNKNOWN_PLATFORM: c_int = -3;
pub const SIGIL_ERR_UNSUPPORTED_FORMAT: c_int = -4;
pub const SIGIL_ERR_NOT_FOUND: c_int = -5;
pub const SIGIL_ERR_NEEDS_KEY: c_int = -6;
pub const SIGIL_ERR_CRYPTO: c_int = -7;
pub const SIGIL_ERR_OOM: c_int = -8;
pub const SIGIL_ERR_CONFLICT: c_int = -9;
pub const SIGIL_ERR_EXISTS: c_int = -10;
pub const SIGIL_ERR_NO_SPACE: c_int = -11;
pub const SIGIL_ERR_UNCOLLECTED: c_int = -12;
pub const SIGIL_ERR_DAMAGED: c_int = -13;
pub const SIGIL_ERR_REGION: c_int = -14;
pub const SIGIL_ERR_NO_TARGET: c_int = -15;
pub const SIGIL_ERR_AMBIGUOUS: c_int = -16;
pub const SIGIL_ERR_KEYS_INCOMPATIBLE: c_int = -17;

// ---- struct versions ---------------------------------------------------------------------

pub const SIGIL_RESULT_V4: u32 = 4;
pub const SIGIL_SAVE_REQUEST_V1: u32 = 1;
pub const SIGIL_SYNC_REQUEST_V1: u32 = 1;

// ---- sizes -------------------------------------------------------------------------------

pub const SIGIL_SAVE_PATH_MAX: usize = 512;
pub const SIGIL_SAVE_ENTRY_MAX: usize = 256;
pub const SIGIL_PROFILE_ID_MAX: usize = 64;
pub const SIGIL_PROFILE_NAME_MAX: usize = 64;
pub const SIGIL_CARD_NAME_MAX: usize = 64;

// ---- enums (C `int`) ---------------------------------------------------------------------

pub type sigil_platform = c_int;
pub const SIGIL_PLATFORM_AUTO: sigil_platform = 0;

pub type sigil_source = c_int;
pub type sigil_usage = c_int;

pub const SIGIL_FEATURE_RTC: u32 = 1 << 0;
pub const SIGIL_FEATURE_MBC2: u32 = 1 << 1;

pub const SIGIL_SAVE_SHAPE_NONE: c_int = 0;
pub const SIGIL_SAVE_SHAPE_SINGLE: c_int = 1;
pub const SIGIL_SAVE_SHAPE_MULTI: c_int = 2;
pub const SIGIL_SAVE_SHAPE_FOLDER: c_int = 3;

pub const SIGIL_SYNC_MANAGED: c_int = 0;
pub const SIGIL_SYNC_UNMANAGED: c_int = 1;

// ---- identification ----------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct sigil_result {
    pub struct_version: u32,
    pub title_id: [c_char; 32],
    pub raw_serial: [c_char; 32],
    pub save_id: [c_char; 32],
    pub platform: sigil_platform,
    pub source: sigil_source,
    pub usage: sigil_usage,
    pub experimental: c_int,
    pub switch_content_type: c_int,
    pub title_version: u32,
    pub features: u32,
    pub n64_header: [c_char; 24],
    pub n64_md5: [c_char; 33],
    pub n64_md5_n64: [c_char; 33],
}

#[repr(C)]
pub struct sigil_io {
    pub read: Option<unsafe extern "C" fn(ctx: *mut c_void, off: u64, buf: *mut c_void, len: usize) -> c_int>,
    pub size: Option<unsafe extern "C" fn(ctx: *mut c_void) -> i64>,
    pub close: Option<unsafe extern "C" fn(ctx: *mut c_void)>,
    pub ctx: *mut c_void,
}

#[repr(C)]
pub struct sigil_support {
    pub struct_version: u32,
    pub switch_header_key: *const u8,
    pub switch_prod_keys_path: *const c_char,
    pub switch_prod_keys_text: *const c_char,
    pub switch_prod_keys_text_len: usize,
}

#[repr(C)]
pub struct sigil_options {
    pub struct_version: u32,
    pub support: *const sigil_support,
    pub flags: u32,
}

// ---- save units --------------------------------------------------------------------------

#[repr(C)]
pub struct sigil_save_member {
    pub path: [c_char; SIGIL_SAVE_PATH_MAX],
    pub entry: [c_char; SIGIL_SAVE_ENTRY_MAX],
    pub role: c_int,
    pub present: c_int,
    pub area: c_int,
}

#[repr(C)]
pub struct sigil_save_profile {
    pub id: [c_char; SIGIL_PROFILE_ID_MAX],
    pub name: [c_char; SIGIL_PROFILE_NAME_MAX],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct sigil_save_option {
    pub key: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
pub struct sigil_save_alternate {
    pub path: [c_char; SIGIL_SAVE_PATH_MAX],
    pub shared: c_int,
    pub options: [sigil_save_option; 2],
    pub option_count: usize,
}

pub type sigil_save_open_fn = Option<unsafe extern "C" fn(ctx: *mut c_void, relative_path: *const c_char) -> *mut sigil_io>;

#[repr(C)]
pub struct sigil_save_request {
    pub struct_version: u32,
    pub layout: *const c_char,
    pub platform: *const c_char,
    pub content_path: *const c_char,
    pub result: *const sigil_result,
    pub features: u32,
    pub options: *const sigil_save_option,
    pub option_count: usize,
    pub listing: *const *const c_char,
    pub listing_count: usize,
    pub open: sigil_save_open_fn,
    pub open_ctx: *mut c_void,
    pub root_path: *const c_char,
    pub profile: *const c_char,
}

#[repr(C)]
pub struct sigil_save_unit {
    pub struct_version: u32,
    pub key: [c_char; SIGIL_SAVE_ENTRY_MAX],
    pub shape: c_int,
    pub members: *mut sigil_save_member,
    pub member_count: usize,
    pub expected: *mut sigil_save_member,
    pub expected_count: usize,
    pub unkeyed: *mut [c_char; SIGIL_SAVE_PATH_MAX],
    pub unkeyed_count: usize,
    pub artifact: [c_char; SIGIL_SAVE_ENTRY_MAX],
    pub content_hash: [c_char; 33],
    pub identity_hash: [c_char; 33],
    pub alternates: *mut sigil_save_alternate,
    pub alternate_count: usize,
}

// ---- sync --------------------------------------------------------------------------------

pub type sigil_save_write_fn =
    Option<unsafe extern "C" fn(ctx: *mut c_void, relative_path: *const c_char, data: *const u8, len: usize) -> c_int>;
pub type sigil_save_remove_fn = Option<unsafe extern "C" fn(ctx: *mut c_void, relative_path: *const c_char) -> c_int>;

#[repr(C)]
pub struct sigil_sync_companion {
    pub game_ids: *const *const c_char,
    pub game_id_count: usize,
    pub unit: *const u8,
    pub unit_len: usize,
}

#[repr(C)]
pub struct sigil_sync_companion_result {
    pub data: *mut u8,
    pub len: usize,
    pub content_hash: [c_char; 33],
    pub identity_hash: [c_char; 33],
    pub changed: c_int,
}

#[repr(C)]
pub struct sigil_sync_request {
    pub struct_version: u32,
    pub save: sigil_save_request,
    pub game_ids: *const *const c_char,
    pub game_id_count: usize,
    pub mode: c_int,
    pub state: *const u8,
    pub state_len: usize,
    pub overwrite_local: c_int,
    pub write: sigil_save_write_fn,
    pub write_ctx: *mut c_void,
    pub claimed: *const *const c_char,
    pub claimed_count: usize,
    pub remove: sigil_save_remove_fn,
    pub companions: *const sigil_sync_companion,
    pub companion_count: usize,
    pub repair: c_int,
}

#[repr(C)]
pub struct sigil_sync_result {
    pub struct_version: u32,
    pub artifact: [c_char; SIGIL_SAVE_ENTRY_MAX],
    pub shape: c_int,
    pub data: *mut u8,
    pub len: usize,
    pub content_hash: [c_char; 33],
    pub identity_hash: [c_char; 33],
    pub changed: c_int,
    pub conflict: c_int,
    pub state: *mut u8,
    pub state_len: usize,
    pub holding: *mut u8,
    pub holding_len: usize,
    pub unowned: *mut [c_char; SIGIL_CARD_NAME_MAX],
    pub unowned_count: usize,
    pub restore_again: c_int,
    pub companions: *mut sigil_sync_companion_result,
    pub companion_count: usize,
    pub problem: [c_char; SIGIL_SAVE_PATH_MAX],
    pub blocks_short: u32,
    pub profiles: *mut sigil_save_profile,
    pub profile_count: usize,
    pub profile: [c_char; SIGIL_PROFILE_ID_MAX],
    pub alternates: *mut sigil_save_alternate,
    pub alternate_count: usize,
    pub hardcore_marker: c_int,
    pub unowned_changed: usize,
}

extern "C" {
    pub fn sigil_extract_from_path(
        path: *const c_char,
        hint: sigil_platform,
        opts: *const sigil_options,
        out: *mut sigil_result,
    ) -> c_int;
    pub fn sigil_platform_from_slug(slug: *const c_char) -> sigil_platform;
    pub fn sigil_platform_to_slug(p: sigil_platform) -> *const c_char;

    pub fn sigil_io_open_file(path: *const c_char) -> *mut sigil_io;
    pub fn sigil_io_close(io: *mut sigil_io);

    pub fn sigil_save_resolve(req: *const sigil_save_request, out: *mut *mut sigil_save_unit) -> c_int;
    pub fn sigil_save_unit_free(unit: *mut sigil_save_unit);
    pub fn sigil_save_layout_subdirs(layout: *const c_char, out: *mut *const c_char, cap: usize) -> usize;
    pub fn sigil_save_base(
        layout: *const c_char,
        path: *const c_char,
        base: *mut c_char,
        base_cap: usize,
        profile: *mut c_char,
        profile_cap: usize,
    ) -> c_int;
    pub fn sigil_save_layout_top(layout: *const c_char) -> *const c_char;
    pub fn sigil_content_stem(content_path: *const c_char, out: *mut c_char, cap: usize) -> *const c_char;

    pub fn sigil_collect(req: *const sigil_sync_request, out: *mut *mut sigil_sync_result) -> c_int;
    pub fn sigil_restore(
        req: *const sigil_sync_request,
        unit: *const u8,
        unit_len: usize,
        out: *mut *mut sigil_sync_result,
    ) -> c_int;
    pub fn sigil_sync_result_free(result: *mut sigil_sync_result);

    pub fn sigil_strerror(code: c_int) -> *const c_char;
    pub fn sigil_version() -> *const c_char;
}

// Generated from `tests/layout_probe.c` compiled against the vendored `sigil.h`
// (`scripts/check-sigil-layout.sh` prints these lines). If a Sigil bump changes any number, the
// header changed shape and the structs above must be updated to match before anything else. They
// hold on every 64-bit target Sigil supports (no `long` appears in the header).
#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn result_layout() {
        assert_eq!(size_of::<sigil_result>(), 220);
        assert_eq!(offset_of!(sigil_result, platform), 100);
        assert_eq!(offset_of!(sigil_result, features), 124);
        assert_eq!(offset_of!(sigil_result, n64_header), 128);
        assert_eq!(offset_of!(sigil_result, n64_md5_n64), 185);
    }

    #[test]
    fn save_request_layout() {
        assert_eq!(size_of::<sigil_save_request>(), 112);
        assert_eq!(offset_of!(sigil_save_request, result), 32);
        assert_eq!(offset_of!(sigil_save_request, options), 48);
        assert_eq!(offset_of!(sigil_save_request, listing), 64);
        assert_eq!(offset_of!(sigil_save_request, open), 80);
        assert_eq!(offset_of!(sigil_save_request, root_path), 96);
        assert_eq!(offset_of!(sigil_save_request, profile), 104);
    }

    #[test]
    fn save_unit_layout() {
        assert_eq!(size_of::<sigil_save_unit>(), 656);
        assert_eq!(offset_of!(sigil_save_unit, shape), 260);
        assert_eq!(offset_of!(sigil_save_unit, members), 264);
        assert_eq!(offset_of!(sigil_save_unit, unkeyed), 296);
        assert_eq!(offset_of!(sigil_save_unit, artifact), 312);
        assert_eq!(offset_of!(sigil_save_unit, content_hash), 568);
        assert_eq!(offset_of!(sigil_save_unit, identity_hash), 601);
        assert_eq!(offset_of!(sigil_save_unit, alternates), 640);
    }

    #[test]
    fn sync_request_layout() {
        assert_eq!(size_of::<sigil_sync_request>(), 232);
        assert_eq!(offset_of!(sigil_sync_request, save), 8);
        assert_eq!(offset_of!(sigil_sync_request, game_ids), 120);
        assert_eq!(offset_of!(sigil_sync_request, mode), 136);
        assert_eq!(offset_of!(sigil_sync_request, state), 144);
        assert_eq!(offset_of!(sigil_sync_request, overwrite_local), 160);
        assert_eq!(offset_of!(sigil_sync_request, write), 168);
        assert_eq!(offset_of!(sigil_sync_request, claimed), 184);
        assert_eq!(offset_of!(sigil_sync_request, remove), 200);
        assert_eq!(offset_of!(sigil_sync_request, companions), 208);
        assert_eq!(offset_of!(sigil_sync_request, repair), 224);
    }

    #[test]
    fn sync_result_layout() {
        assert_eq!(size_of::<sigil_sync_result>(), 1064);
        assert_eq!(offset_of!(sigil_sync_result, artifact), 4);
        assert_eq!(offset_of!(sigil_sync_result, shape), 260);
        assert_eq!(offset_of!(sigil_sync_result, data), 264);
        assert_eq!(offset_of!(sigil_sync_result, content_hash), 280);
        assert_eq!(offset_of!(sigil_sync_result, changed), 348);
        assert_eq!(offset_of!(sigil_sync_result, state), 360);
        assert_eq!(offset_of!(sigil_sync_result, holding), 376);
        assert_eq!(offset_of!(sigil_sync_result, unowned), 392);
        assert_eq!(offset_of!(sigil_sync_result, restore_again), 408);
        assert_eq!(offset_of!(sigil_sync_result, companions), 416);
        assert_eq!(offset_of!(sigil_sync_result, problem), 432);
        assert_eq!(offset_of!(sigil_sync_result, blocks_short), 944);
        assert_eq!(offset_of!(sigil_sync_result, profiles), 952);
        assert_eq!(offset_of!(sigil_sync_result, profile), 968);
        assert_eq!(offset_of!(sigil_sync_result, alternates), 1032);
        assert_eq!(offset_of!(sigil_sync_result, hardcore_marker), 1048);
        assert_eq!(offset_of!(sigil_sync_result, unowned_changed), 1056);
    }

    #[test]
    fn small_struct_sizes() {
        assert_eq!(size_of::<sigil_save_member>(), 780);
        assert_eq!(size_of::<sigil_save_profile>(), 128);
        assert_eq!(size_of::<sigil_save_option>(), 16);
        assert_eq!(size_of::<sigil_save_alternate>(), 560);
        assert_eq!(size_of::<sigil_sync_companion>(), 32);
        assert_eq!(size_of::<sigil_sync_companion_result>(), 88);
        assert_eq!(size_of::<sigil_io>(), 32);
        assert_eq!(size_of::<sigil_support>(), 40);
        assert_eq!(size_of::<sigil_options>(), 24);
    }
}
