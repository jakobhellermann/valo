//# build_lib: lib/loader_lock_ctor
//# link_flags: -rdynamic
//
// A hook's first call must not need the dynamic loader's lock: a thread that holds it can be
// waiting for the caller. Hollow Knight: Mono's GC suspended a thread inside libvalo's lazy
// dladdr, and the collecting thread's first sem_timedwait then blocked in dlsym.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <libgen.h>
#include <limits.h>
#include <pthread.h>
#include <semaphore.h>
#include <stdlib.h>
#include <time.h>

#include "testlib.h"

static atomic_int held;
static atomic_int released;

__attribute__((visibility("default"))) void loader_lock_held(void) {
  atomic_store(&held, 1);
  while (!atomic_load(&released)) {
  }
}

static void *load(void *path) {
  void *handle = dlopen(path, RTLD_NOW);
  assert(handle);
  return NULL;
}

int main() {
  char exe[PATH_MAX];
  ssize_t len = readlink("/proc/self/exe", exe, sizeof(exe) - 1);
  assert(len > 0);
  exe[len] = 0;
  char path[PATH_MAX];
  snprintf(path, sizeof(path), "%s/lib/loader_lock_ctor.so", dirname(exe));
  pthread_t loader;
  assert(pthread_create(&loader, NULL, load, path) == 0);
  while (!atomic_load(&held)) {
  }

  sem_t sem;
  assert(sem_init(&sem, 0, 1) == 0);
  struct timespec deadline = {0};
  assert(sem_timedwait(&sem, &deadline) == 0);
  tprintf("hook linked while the loader lock is held\n");

  atomic_store(&released, 1);
  assert(pthread_join(loader, NULL) == 0);
}
