//! Counts the target's RNG draws by interposing Mono's internal-call registry.
//!
//! Unity implements `UnityEngine.Random`'s hot members (`value`, `Range(float, float)`,
//! `RandomRangeInt`) as internal calls: extern methods with no managed body, which neither
//! MonoMod nor Harmony can detour. They are, however, registered by name through
//! `mono_add_internal_call`, so substituting the registered pointer catches every draw without
//! naming a single game method — and works for any Unity game, not one version of one title.
//!
//! The registered signatures are unknown, so a thunk must not touch the arguments: it spills the
//! six integer and eight float argument registers, calls the counter, restores them and tail-jumps
//! to the original.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use rustix::mm::{MapFlags, ProtFlags};

use crate::{info, warn};

/// Registrations whose name starts with this are counted. Everything else passes through.
const WATCHED: &[u8] = b"UnityEngine.Random::";

/// A draw counter per watched registration. Fixed so the thunks can index it without locking;
/// Unity registers a handful of Random icalls.
const SLOTS: usize = 32;

static COUNTS: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
static NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static REAL_ADD_INTERNAL_CALL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

type AddInternalCallFn = unsafe extern "C" fn(*const c_char, *const c_void);

/// How many managed frames above the draw identify its origin. Deeper is rarely more telling and
/// multiplies the distinct stacks to aggregate.
const STACK_DEPTH: usize = 4;

/// Walking the managed stack costs far more than the draw itself, so it is off unless asked for:
/// `VALO_RNG_STACKS=1`. A run that wants attribution accepts being slow.
static WALK_STACKS: AtomicBool = AtomicBool::new(false);
static STACKS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);
static MONO_STACK_WALK: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static MONO_METHOD_FULL_NAME: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static MONO_FREE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

type StackFrameFn = unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, *mut c_void) -> c_int;
type StackWalkFn = unsafe extern "C" fn(StackFrameFn, *mut c_void);
type MethodFullNameFn = unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_char;
type FreeFn = unsafe extern "C" fn(*mut c_void);

/// Remembers the symbols a stack walk needs, as the target resolves them.
pub(crate) fn note_dlsym(name: &[u8], resolved: *mut c_void) {
    let slot = match name {
        b"mono_stack_walk" => &MONO_STACK_WALK,
        b"mono_method_full_name" => &MONO_METHOD_FULL_NAME,
        b"mono_free" => &MONO_FREE,
        _ => return,
    };
    slot.store(resolved, Ordering::SeqCst);
}

/// Replacement for a `dlsym` of `mono_add_internal_call`. `None` leaves the lookup alone.
pub(crate) fn dlsym_add_internal_call(resolved: *mut c_void) -> Option<*mut c_void> {
    let previous = REAL_ADD_INTERNAL_CALL.swap(resolved, Ordering::SeqCst);
    if previous.is_null() {
        let stacks = std::env::var_os("VALO_RNG_STACKS").is_some_and(|v| v != "0");
        WALK_STACKS.store(stacks, Ordering::SeqCst);
        if stacks {
            *STACKS.lock().unwrap_or_else(|e| e.into_inner()) = Some(HashMap::new());
        }
        info!(
            Hooks,
            "counting UnityEngine.Random draws through mono_add_internal_call (stacks: {stacks})"
        );
    }
    Some(mono_add_internal_call as *mut c_void)
}

/// # Safety
/// Called by the Mono embedder with a nul-terminated name and the icall's implementation.
unsafe extern "C" fn mono_add_internal_call(name: *const c_char, method: *const c_void) {
    let real = REAL_ADD_INTERNAL_CALL.load(Ordering::SeqCst);
    debug_assert!(
        !real.is_null(),
        "registration before the dlsym that found it"
    );
    // SAFETY: the stored pointer is the real `mono_add_internal_call` resolved by the target.
    let real = unsafe { std::mem::transmute::<*mut c_void, AddInternalCallFn>(real) };

    if name.is_null() || method.is_null() {
        // SAFETY: forwarding the target's own arguments.
        return unsafe { real(name, method) };
    }
    // SAFETY: `name` is the embedder's nul-terminated registration name.
    let bytes = unsafe { CStr::from_ptr(name) }.to_bytes();
    if !bytes.starts_with(WATCHED) {
        // SAFETY: forwarding the target's own arguments.
        return unsafe { real(name, method) };
    }

    let substitute = match counted(bytes, method) {
        Some(thunk) => thunk,
        None => method,
    };
    // SAFETY: forwarding the target's own name with a thunk that tail-jumps to `method`.
    unsafe { real(name, substitute) }
}

/// Assigns the registration a counter slot and builds its thunk. `None` when the slots or the
/// thunk page are exhausted, which leaves the icall uncounted rather than unregistered.
fn counted(name: &[u8], method: *const c_void) -> Option<*const c_void> {
    let mut names = NAMES.lock().ok()?;
    if names.len() >= SLOTS {
        warn!(
            Hooks,
            "more than {SLOTS} UnityEngine.Random icalls: {} stays uncounted",
            String::from_utf8_lossy(name)
        );
        return None;
    }
    let slot = names.len();
    names.push(String::from_utf8_lossy(name).into_owned());
    drop(names);
    thunk(slot as u64, method)
}

extern "C" fn count(slot: u64) {
    if let Some(counter) = COUNTS.get(slot as usize) {
        counter.fetch_add(1, Ordering::Relaxed);
    }
    if WALK_STACKS.load(Ordering::Relaxed) {
        record_stack(slot);
    }
}

// The frames collected for the draw being counted. The walk callback runs to completion before
// the next draw on this thread, so one buffer per thread holds it.
thread_local! {
    static FRAMES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Attributes one draw to the managed methods above it.
fn record_stack(slot: u64) {
    let walk = MONO_STACK_WALK.load(Ordering::Relaxed);
    if walk.is_null() {
        return;
    }
    // SAFETY: the pointer is `mono_stack_walk` as the target resolved it.
    let walk = unsafe { std::mem::transmute::<*mut c_void, StackWalkFn>(walk) };
    // SAFETY: `frame` has the callback signature and ignores its user data.
    unsafe { walk(frame, std::ptr::null_mut()) };

    let caller = FRAMES.with_borrow_mut(|frames| {
        let joined = frames.join(" <- ");
        frames.clear();
        joined
    });
    let name = NAMES
        .lock()
        .ok()
        .and_then(|names| names.get(slot as usize).cloned())
        .unwrap_or_default();
    let mut stacks = STACKS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(stacks) = stacks.as_mut() {
        *stacks.entry(format!("{name} <- {caller}")).or_insert(0) += 1;
    }
}

/// `mono_stack_walk`'s per-frame callback. Returns non-zero to stop the walk.
unsafe extern "C" fn frame(
    method: *mut c_void,
    _native_offset: c_int,
    _il_offset: c_int,
    managed: c_int,
    _data: *mut c_void,
) -> c_int {
    if managed == 0 || method.is_null() {
        return 0;
    }
    let full_name = MONO_METHOD_FULL_NAME.load(Ordering::Relaxed);
    if full_name.is_null() {
        return 1;
    }
    // SAFETY: the pointer is `mono_method_full_name` as the target resolved it, called with the
    // method the walk handed us.
    let name = unsafe {
        let full_name = std::mem::transmute::<*mut c_void, MethodFullNameFn>(full_name);
        let raw = full_name(method, 0);
        if raw.is_null() {
            return 1;
        }
        let owned = CStr::from_ptr(raw).to_string_lossy().into_owned();
        let free = MONO_FREE.load(Ordering::Relaxed);
        if !free.is_null() {
            std::mem::transmute::<*mut c_void, FreeFn>(free)(raw.cast());
        }
        owned
    };
    FRAMES.with_borrow_mut(|frames| {
        frames.push(name);
        c_int::from(frames.len() >= STACK_DEPTH)
    })
}

/// The draws attributed since the last read, as `count stack` entries separated by `;`, most
/// drawn first. Read and cleared like `valo_rng_draws`, so an agent reporting per frame sees
/// which call sites drew in that frame — the comparison that locates a divergence.
///
/// # Safety
/// `buf` is valid for writes of `cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn valo_rng_draw_stacks(buf: *mut u8, cap: usize) -> isize {
    let mut stacks = STACKS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(stacks) = stacks.as_mut() else {
        return 0;
    };
    let mut sorted: Vec<_> = stacks.drain().collect();
    sorted.sort_by(|(left_stack, left), (right_stack, right)| {
        right.cmp(left).then_with(|| left_stack.cmp(right_stack))
    });
    let mut text = String::new();
    for (stack, count) in sorted {
        if !text.is_empty() {
            text.push(';');
        }
        text.push_str(&format!("{count} {stack}"));
    }
    if text.len() > cap || buf.is_null() {
        return text.len() as isize;
    }
    // SAFETY: `buf` is valid for `cap` bytes and `text` is no longer than that.
    unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), buf, text.len()) };
    text.len() as isize
}

/// The draws counted so far as `name=count` pairs, and resets them. Returns the length written,
/// or the length needed when it exceeds `cap`, in which case nothing is written.
///
/// # Safety
/// `buf` is valid for writes of `cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn valo_rng_draws(buf: *mut u8, cap: usize) -> isize {
    let Ok(names) = NAMES.lock() else {
        return 0;
    };
    let mut text = String::new();
    for (slot, name) in names.iter().enumerate() {
        let count = COUNTS[slot].swap(0, Ordering::Relaxed);
        if count == 0 {
            continue;
        }
        if !text.is_empty() {
            text.push(',');
        }
        text.push_str(name);
        text.push('=');
        text.push_str(&count.to_string());
    }
    if text.len() > cap || buf.is_null() {
        return text.len() as isize;
    }
    // SAFETY: `buf` is valid for `cap` bytes and `text` is no longer than that.
    unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), buf, text.len()) };
    text.len() as isize
}

/// Executable memory for the thunks, bump-allocated: they live as long as the registrations do.
static THUNKS: Mutex<Option<Page>> = Mutex::new(None);

struct Page {
    base: *mut u8,
    used: usize,
}

// SAFETY: the pointer is a private anonymous mapping owned by this module and only reached
// through the mutex.
unsafe impl Send for Page {}

const PAGE_LEN: usize = 4096;

/// Builds a thunk that counts `slot` and tail-jumps to `method`. Argument registers are spilled
/// across the call, so a thunk is valid for any signature.
fn thunk(slot: u64, method: *const c_void) -> Option<*const c_void> {
    // The spill area holds xmm0-7; 136 keeps rsp 16-byte aligned for the call.
    const SPILL: u32 = 136;
    let mut code: Vec<u8> = Vec::with_capacity(160);

    // push rdi, rsi, rdx, rcx, r8, r9
    code.extend_from_slice(&[0x57, 0x56, 0x52, 0x51, 0x41, 0x50, 0x41, 0x51]);
    // sub rsp, SPILL
    code.extend_from_slice(&[0x48, 0x81, 0xec]);
    code.extend_from_slice(&SPILL.to_le_bytes());
    // movups [rsp + 16*i], xmm<i>
    for i in 0..8u8 {
        code.extend_from_slice(&[0x0f, 0x11, 0x44 | (i << 3), 0x24, i * 16]);
    }
    // mov rdi, slot
    code.extend_from_slice(&[0x48, 0xbf]);
    code.extend_from_slice(&slot.to_le_bytes());
    // mov r11, count; call r11
    code.extend_from_slice(&[0x49, 0xbb]);
    code.extend_from_slice(&(count as *const () as usize as u64).to_le_bytes());
    code.extend_from_slice(&[0x41, 0xff, 0xd3]);
    // movups xmm<i>, [rsp + 16*i]
    for i in 0..8u8 {
        code.extend_from_slice(&[0x0f, 0x10, 0x44 | (i << 3), 0x24, i * 16]);
    }
    // add rsp, SPILL
    code.extend_from_slice(&[0x48, 0x81, 0xc4]);
    code.extend_from_slice(&SPILL.to_le_bytes());
    // pop r9, r8, rcx, rdx, rsi, rdi
    code.extend_from_slice(&[0x41, 0x59, 0x41, 0x58, 0x59, 0x5a, 0x5e, 0x5f]);
    // mov r11, method; jmp r11
    code.extend_from_slice(&[0x49, 0xbb]);
    code.extend_from_slice(&(method as usize as u64).to_le_bytes());
    code.extend_from_slice(&[0x41, 0xff, 0xe3]);

    let mut page = THUNKS.lock().ok()?;
    if page.is_none() {
        // SAFETY: an anonymous mapping, so the null hint and absent fd are the documented use.
        let base = unsafe {
            rustix::mm::mmap_anonymous(
                std::ptr::null_mut(),
                PAGE_LEN,
                ProtFlags::READ | ProtFlags::WRITE | ProtFlags::EXEC,
                MapFlags::PRIVATE,
            )
        }
        .ok()?;
        *page = Some(Page {
            base: base.cast(),
            used: 0,
        });
    }
    let page = page.as_mut()?;
    if page.used + code.len() > PAGE_LEN {
        warn!(Hooks, "thunk page full: an icall stays uncounted");
        return None;
    }
    // SAFETY: `used + len` is within the mapping, which is writable and executable.
    let at = unsafe { page.base.add(page.used) };
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), at, code.len()) };
    page.used += code.len();
    Some(at.cast())
}
