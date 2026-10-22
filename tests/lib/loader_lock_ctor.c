// Runs under the dynamic loader's lock: constructors of a dlopen'd library do.
void loader_lock_held(void);

__attribute__((constructor)) static void ctor(void) { loader_lock_held(); }
