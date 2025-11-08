#[unsafe(no_mangle)]
pub extern "C" fn SDL_RenderPresent(renderer: *const ()) {
    let ptr = crate::sym!(c"SDL_RenderPresent");
    let fn_ptr = unsafe { std::mem::transmute::<*mut (), extern "C" fn(*const ())>(ptr) };
    crate::frame::frame_boundary(move || fn_ptr(renderer));
}
