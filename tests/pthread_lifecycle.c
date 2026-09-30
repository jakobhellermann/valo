//# link_mode: preload
//# link_flags: -pthread
// Pthread interposition must preserve return values, join, detach and exit.
#include <assert.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include "testlib.h"

static atomic_int release_detached;
static atomic_int detached_done;

static void *returns_value(void *arg) {
    return arg;
}

static void *exits_with_value(void *arg) {
    pthread_exit(arg);
}

static void *detached_thread(void *arg) {
    (void)arg;
    while (!atomic_load(&release_detached)) {}
    atomic_store(&detached_done, 1);
    return NULL;
}

int main(void) {
    pthread_t thread;
    void *value;

    // Repeated joins exercise pthread handle reuse without depending on it.
    for (intptr_t i = 1; i <= 64; ++i) {
        assert(pthread_create(&thread, NULL, returns_value, (void *)i) == 0);
        assert(pthread_join(thread, &value) == 0);
        assert(value == (void *)i);
    }

    assert(pthread_create(&thread, NULL, exits_with_value, (void *)123) == 0);
    assert(pthread_join(thread, &value) == 0);
    assert(value == (void *)123);

    assert(pthread_create(&thread, NULL, detached_thread, NULL) == 0);
    assert(pthread_detach(thread) == 0);
    atomic_store(&release_detached, 1);
    while (!atomic_load(&detached_done)) {}

    // Creation with detached attributes exercises the other detach path.
    atomic_store(&release_detached, 0);
    atomic_store(&detached_done, 0);
    pthread_attr_t attr;
    assert(pthread_attr_init(&attr) == 0);
    assert(pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED) == 0);
    assert(pthread_create(&thread, &attr, detached_thread, NULL) == 0);
    assert(pthread_attr_destroy(&attr) == 0);
    atomic_store(&release_detached, 1);
    while (!atomic_load(&detached_done)) {}

    tprintf("pthread lifecycle ok\n");
}
