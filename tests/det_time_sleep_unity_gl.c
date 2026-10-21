//# link_mode: preload
//# link_flags: -lSDL2
//# unity_version: 5.4.3f1
// In Unity, sleeps of the frame thread return at once without passing virtual time: only frame
// boundaries advance the clock.
#include <assert.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
#include <SDL2/SDL.h>
#include "testlib.h"

#define NS 1000000000LL

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
        struct timespec ts = to_timespec(10000000);
        for (int j = 0; j < 100; j++)
            assert(nanosleep(&ts, NULL) == 0);
        assert(usleep(10000) == 0);
        struct timespec deadline = to_timespec(clock_ns() + NS);
        assert(clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &deadline, NULL) == 0);
        assert(clock_ns() == before);
        present();
    }
    assert(clock_ns() - start == NS / 4);
    assert(real_ns() - real_start < NS / 10);
    tprintf("15 frames with 1s of sleeps each: 250ms virtual\n");
    return 0;
}
