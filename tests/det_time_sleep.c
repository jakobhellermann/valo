//# link_mode: preload
//# link_flags: -lSDL2 -pthread
// Sleeps of the frame thread advance virtual time instead of blocking: a frame limiter that
// sleeps until the next frame's deadline ends every frame on the grid. Other threads sleep for
// real without advancing it.
#include <assert.h>
#include <pthread.h>
#include <sys/syscall.h>
#include <time.h>
#include <SDL2/SDL.h>
#include "testlib.h"

#define NS 1000000000LL
#define FRAME_NS (NS / 60)

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

static struct timespec to_timespec(long long ns) {
    return (struct timespec){ns / NS, ns % NS};
}

static SDL_Renderer *renderer;

static void present(void) {
    SDL_RenderClear(renderer);
    SDL_RenderPresent(renderer);
}

static void *sleeping_worker(void *arg) {
    long long *real_slept = arg;
    long long virtual_before = clock_ns();
    long long real_before = real_ns();
    struct timespec ts = to_timespec(50000000);
    nanosleep(&ts, NULL);
    *real_slept = real_ns() - real_before;
    assert(clock_ns() == virtual_before);
    return NULL;
}

int main() {
    SDL_setenv("SDL_VIDEODRIVER", "dummy", 1);
    SDL_Init(SDL_INIT_VIDEO);
    SDL_Window *window = SDL_CreateWindow("valo-test", SDL_WINDOWPOS_UNDEFINED,
                                          SDL_WINDOWPOS_UNDEFINED, 320, 240, SDL_WINDOW_SHOWN);
    renderer = SDL_CreateRenderer(window, -1, SDL_RENDERER_SOFTWARE);
    present();

    long long start = clock_ns();
    long long real_start = real_ns();
    for (int i = 0; i < 15; i++) {
        long long before = clock_ns();
        struct timespec ts = to_timespec(FRAME_NS);
        assert(nanosleep(&ts, NULL) == 0);
        assert(clock_ns() - before == FRAME_NS);
        present();
    }
    for (int i = 0; i < 15; i++) {
        struct timespec deadline = to_timespec(clock_ns() + FRAME_NS);
        assert(clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &deadline, NULL) == 0);
        present();
    }
    assert(clock_ns() - start == NS / 2);
    assert(real_ns() - real_start < NS / 10);
    tprintf("30 limited frames: 500ms virtual\n");

    long long real_slept = 0;
    pthread_t worker;
    pthread_create(&worker, NULL, sleeping_worker, &real_slept);
    pthread_join(worker, NULL);
    assert(real_slept >= 50000000);
    tprintf("worker sleep: real, clock unchanged\n");
    return 0;
}
