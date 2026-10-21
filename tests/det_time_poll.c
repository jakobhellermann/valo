//# link_mode: preload
//# link_flags: -pthread
// The frame thread's waits on no fds are sleeps that pass only virtual time. Its waits on fds
// block for real, and a timeout also passes in virtual time. Other threads only wait for real.
#define _GNU_SOURCE
#include <assert.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <sys/epoll.h>
#include <sys/select.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
#include "testlib.h"

#define NS 1000000000LL
#define TIMEOUT_MS 10
#define TIMEOUT_NS (TIMEOUT_MS * 1000000LL)

static long long clock_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * NS + ts.tv_nsec;
}

static long long real_ns(void) {
    struct timespec ts;
    syscall(SYS_clock_gettime, CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * NS + ts.tv_nsec;
}

static long long virtual_start, real_start;

static void begin(void) {
    virtual_start = clock_ns();
    real_start = real_ns();
}

static void slept(const char *name, int ret) {
    assert(ret == 0);
    assert(clock_ns() - virtual_start == TIMEOUT_NS);
    assert(real_ns() - real_start < TIMEOUT_NS / 2);
    tprintf("%s: virtual sleep\n", name);
}

static void timed_out(const char *name, int ret) {
    assert(ret == 0);
    assert(clock_ns() - virtual_start == TIMEOUT_NS);
    assert(real_ns() - real_start >= TIMEOUT_NS * 9 / 10);
    tprintf("%s: real wait, timeout in virtual time\n", name);
}

static void *worker(void *arg) {
    (void)arg;
    begin();
    assert(poll(NULL, 0, TIMEOUT_MS) == 0);
    assert(clock_ns() == virtual_start);
    assert(real_ns() - real_start >= TIMEOUT_NS * 9 / 10);
    return NULL;
}

int main() {
    int fds[2];
    assert(pipe(fds) == 0);
    struct pollfd pfd = {fds[0], POLLIN, 0};
    struct timespec ts = {0, TIMEOUT_NS};

    begin();
    slept("poll", poll(NULL, 0, TIMEOUT_MS));
    begin();
    slept("ppoll", ppoll(NULL, 0, &ts, NULL));
    begin();
    timed_out("poll pipe", poll(&pfd, 1, TIMEOUT_MS));
    begin();
    timed_out("ppoll pipe", ppoll(&pfd, 1, &ts, NULL));

    struct timeval tv = {0, TIMEOUT_MS * 1000};
    begin();
    slept("select", select(0, NULL, NULL, NULL, &tv));
    assert(tv.tv_sec == 0 && tv.tv_usec == 0);
    begin();
    slept("pselect", pselect(0, NULL, NULL, NULL, &ts, NULL));

    fd_set readfds;
    FD_ZERO(&readfds);
    FD_SET(fds[0], &readfds);
    tv = (struct timeval){0, TIMEOUT_MS * 1000};
    begin();
    timed_out("select pipe", select(fds[0] + 1, &readfds, NULL, NULL, &tv));
    FD_ZERO(&readfds);
    FD_SET(fds[0], &readfds);
    begin();
    timed_out("pselect pipe", pselect(fds[0] + 1, &readfds, NULL, NULL, &ts, NULL));

    int epfd = epoll_create1(0);
    struct epoll_event ev = {.events = EPOLLIN, .data.fd = fds[0]};
    assert(epoll_ctl(epfd, EPOLL_CTL_ADD, fds[0], &ev) == 0);
    struct epoll_event out;
    begin();
    timed_out("epoll_wait", epoll_wait(epfd, &out, 1, TIMEOUT_MS));
    begin();
    timed_out("epoll_pwait", epoll_pwait(epfd, &out, 1, TIMEOUT_MS, NULL));

    assert(write(fds[1], "x", 1) == 1);
    begin();
    assert(poll(&pfd, 1, 100) == 1);
    assert(clock_ns() == virtual_start);
    tprintf("ready pipe: returns at once\n");

    pthread_t thread;
    pthread_create(&thread, NULL, worker, NULL);
    pthread_join(thread, NULL);
    tprintf("worker: waits for real\n");
    return 0;
}
