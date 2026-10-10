//# link_flags: -pthread
//# max_restores: 1
// TODO(ai-review): generated, not yet audited
// sigaltstack registration is per-thread kernel state, not memory. It
// survives restore through the signal-frame roundtrip: the kernel writes
// the registration into uc_stack of the suspend-signal frame, and the
// recreated thread re-registers it via rt_sigreturn (see
// docs/signal-state.md). A surviving thread must keep its registration
// through checkpoint and restore.
#include <assert.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>
#include "testlib.h"

static char survivor_altstack[16384];
static char recreated_altstack[16384];

static atomic_int workers_ready;
static atomic_int phase;
static atomic_int survivor_result;
static atomic_int recreated_result;
static atomic_int verify_lock;

static char *handler_sp;

static void onstack_handler(int sig, siginfo_t *info, void *uc) {
    (void)sig; (void)info; (void)uc;
    char probe;
    handler_sp = &probe;
}

static void register_altstack(char *buf, size_t len) {
    stack_t ss;
    memset(&ss, 0, sizeof(ss));
    ss.ss_sp = buf;
    ss.ss_size = len;
    assert(sigaltstack(&ss, NULL) == 0);

    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = SA_ONSTACK | SA_SIGINFO;
    sa.sa_sigaction = onstack_handler;
    assert(sigaction(SIGUSR2, &sa, NULL) == 0);
}

// 1 = registration intact and handler ran on the altstack, 0 otherwise.
static int verify_altstack(char *buf, size_t len) {
    // Serialize: the handler writes a shared slot.
    while (atomic_exchange(&verify_lock, 1)) {}
    handler_sp = NULL;

    stack_t cur;
    assert(sigaltstack(NULL, &cur) == 0);
    int reg_ok = !(cur.ss_flags & SS_DISABLE)
        && cur.ss_sp == buf
        && cur.ss_size == len;

    // SA_ONSTACK handlers run on the registered altstack; without the
    // kernel registration they run on the normal thread stack.
    raise(SIGUSR2);
    uintptr_t sp = (uintptr_t)handler_sp;
    int onstack_ok = handler_sp != NULL
        && sp >= (uintptr_t)buf
        && sp < (uintptr_t)buf + len;

    atomic_store(&verify_lock, 0);
    return reg_ok && onstack_ok;
}

static void *survivor_thread(void *arg) {
    (void)arg;
    register_altstack(survivor_altstack, sizeof(survivor_altstack));
    thread_ready(&workers_ready);
    while (atomic_load_explicit(&phase, memory_order_acquire) == 0) {}
    atomic_store_explicit(&survivor_result,
        verify_altstack(survivor_altstack, sizeof(survivor_altstack)) ? 1 : 2,
        memory_order_release);
    // Stays alive through restore: never recreated.
    for (;;) {}
}

static void *recreated_thread(void *arg) {
    (void)arg;
    register_altstack(recreated_altstack, sizeof(recreated_altstack));
    thread_ready(&workers_ready);
    while (atomic_load_explicit(&phase, memory_order_acquire) == 0) {}
    atomic_store_explicit(&recreated_result,
        verify_altstack(recreated_altstack, sizeof(recreated_altstack)) ? 1 : 2,
        memory_order_release);
    // Raw exit between checkpoint and restore, so the rewound pass has to
    // recreate this thread. No glibc exit path, no TLS access.
    syscall(SYS_exit, 0);
    return NULL;
}


int main(void) {
    pthread_t survivor;
    pthread_t recreated;
    assert(pthread_create(&survivor, NULL, survivor_thread, NULL) == 0);
    assert(pthread_create(&recreated, NULL, recreated_thread, NULL) == 0);
    wait_for_threads(&workers_ready, 2);

    valo_checkpoint();

    // Both timelines run this block: the first pass with the original
    // threads, the rewound pass with the surviving thread and the
    // recreated one.
    atomic_store_explicit(&phase, 1, memory_order_release);
    while (atomic_load_explicit(&survivor_result, memory_order_acquire) == 0) {}
    while (atomic_load_explicit(&recreated_result, memory_order_acquire) == 0) {}
    tprintf(survivor_result == 1 ? "survivor preserved\n" : "survivor lost\n");
    tprintf(recreated_result == 1 ? "recreated preserved\n" : "recreated lost\n");

    // The recreated thread must be gone before the restore.
    for (int i = 0; i < 1000 && task_count() != 2; ++i) {
        usleep(1000);
    }
    assert(task_count() == 2);

    valo_restore();
}
