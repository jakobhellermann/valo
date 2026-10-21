//# link_mode: preload
//# link_flags: -pthread
// Absolute deadlines computed from the virtual clock wait as long in real time as they lie
// ahead in virtual time. When the frame thread times out, its clock reaches the deadline.
#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <pthread.h>
#include <semaphore.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
#include "testlib.h"

#define NS 1000000000LL
#define WAIT_NS 50000000LL

static long long real_ns(void) {
    struct timespec ts;
    syscall(SYS_clock_gettime, CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * NS + ts.tv_nsec;
}

static struct timespec deadline(clockid_t clock) {
    struct timespec ts;
    clock_gettime(clock, &ts);
    long long ns = ts.tv_sec * NS + ts.tv_nsec + WAIT_NS;
    return (struct timespec){ns / NS, ns % NS};
}

static void check(const char *name, long long real_start, int ret) {
    long long waited = real_ns() - real_start;
    assert(ret == ETIMEDOUT);
    assert(waited >= WAIT_NS * 9 / 10);
    assert(waited < NS);
    tprintf("%s: timed out after the deadline\n", name);
}

static long long ns_of(struct timespec ts) {
    return ts.tv_sec * NS + ts.tv_nsec;
}

static void at_deadline(clockid_t clock, struct timespec until) {
    struct timespec now;
    clock_gettime(clock, &now);
    assert(ns_of(now) == ns_of(until));
}

static void frame_thread_waits(void) {
    pthread_cond_t cond = PTHREAD_COND_INITIALIZER;
    pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;
    pthread_mutex_lock(&mutex);
    struct timespec until = deadline(CLOCK_REALTIME);
    long long start = real_ns();
    check("frame cond", start, pthread_cond_timedwait(&cond, &mutex, &until));
    at_deadline(CLOCK_REALTIME, until);

    until = deadline(CLOCK_MONOTONIC);
    start = real_ns();
    check("frame cond clockwait", start, pthread_cond_clockwait(&cond, &mutex, CLOCK_MONOTONIC, &until));
    at_deadline(CLOCK_MONOTONIC, until);
    pthread_mutex_unlock(&mutex);

    sem_t sem;
    sem_init(&sem, 0, 0);
    until = deadline(CLOCK_REALTIME);
    start = real_ns();
    int ret = sem_timedwait(&sem, &until) == -1 ? errno : 0;
    check("frame sem", start, ret);
    at_deadline(CLOCK_REALTIME, until);

    until = deadline(CLOCK_MONOTONIC);
    start = real_ns();
    ret = sem_clockwait(&sem, CLOCK_MONOTONIC, &until) == -1 ? errno : 0;
    check("frame sem clockwait", start, ret);
    at_deadline(CLOCK_MONOTONIC, until);
}

static void cond_wait(const char *name, clockid_t clock) {
    pthread_condattr_t attr;
    pthread_condattr_init(&attr);
    pthread_condattr_setclock(&attr, clock);
    pthread_cond_t cond;
    pthread_cond_init(&cond, &attr);
    pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;

    pthread_mutex_lock(&mutex);
    struct timespec until = deadline(clock);
    long long start = real_ns();
    check(name, start, pthread_cond_timedwait(&cond, &mutex, &until));
    pthread_mutex_unlock(&mutex);
}

static void *worker(void *arg) {
    (void)arg;
    cond_wait("cond realtime", CLOCK_REALTIME);
    cond_wait("cond monotonic", CLOCK_MONOTONIC);

    sem_t sem;
    sem_init(&sem, 0, 0);
    struct timespec until = deadline(CLOCK_REALTIME);
    long long start = real_ns();
    int ret = sem_timedwait(&sem, &until) == -1 ? errno : 0;
    check("sem", start, ret);

    until = deadline(CLOCK_MONOTONIC);
    start = real_ns();
    ret = sem_clockwait(&sem, CLOCK_MONOTONIC, &until) == -1 ? errno : 0;
    check("sem clockwait", start, ret);

    pthread_cond_t cond = PTHREAD_COND_INITIALIZER;
    pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;
    pthread_mutex_lock(&mutex);
    until = deadline(CLOCK_MONOTONIC);
    start = real_ns();
    check("cond clockwait", start, pthread_cond_clockwait(&cond, &mutex, CLOCK_MONOTONIC, &until));
    pthread_mutex_unlock(&mutex);

    until = deadline(CLOCK_MONOTONIC);
    start = real_ns();
    assert(clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &until, NULL) == 0);
    check("clock_nanosleep", start, ETIMEDOUT);

    until = deadline(CLOCK_BOOTTIME);
    start = real_ns();
    assert(clock_nanosleep(CLOCK_BOOTTIME, TIMER_ABSTIME, &until, NULL) == 0);
    check("clock_nanosleep boottime", start, ETIMEDOUT);
    return NULL;
}

int main() {
    pthread_t thread;
    pthread_create(&thread, NULL, worker, NULL);
    pthread_join(thread, NULL);
    frame_thread_waits();
    return 0;
}
