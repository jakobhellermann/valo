// TODO(ai-review): generated, not yet audited
//# link_mode: preload
//# link_flags: -lSDL2
// The coarse clocks follow the deterministic virtual clock like their precise
// counterparts: 30 presents advance CLOCK_MONOTONIC_COARSE and
// CLOCK_REALTIME_COARSE by exactly 30 x 1e9/60 = 500ms, independent of wall
// time (Unity reads MONOTONIC_COARSE every frame alongside MONOTONIC).
#include <assert.h>
#include <time.h>
#include <SDL2/SDL.h>
#include "testlib.h"

#define FRAMES 30
#define EXPECT_30 (500000000LL)

static long long clock_ns(clockid_t clock) {
    struct timespec ts;
    clock_gettime(clock, &ts);
    return ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

int main() {
    SDL_setenv("SDL_VIDEODRIVER", "dummy", 1);
    SDL_Init(SDL_INIT_VIDEO);
    SDL_Window *window = SDL_CreateWindow("valo-det-coarse",
        SDL_WINDOWPOS_UNDEFINED, SDL_WINDOWPOS_UNDEFINED,
        320, 240, SDL_WINDOW_SHOWN);
    SDL_Renderer *renderer = SDL_CreateRenderer(window, -1, SDL_RENDERER_SOFTWARE);

    long long m0 = clock_ns(CLOCK_MONOTONIC_COARSE);
    long long r0 = clock_ns(CLOCK_REALTIME_COARSE);
    for (int i = 0; i < FRAMES; i++) {
        SDL_RenderClear(renderer);
        SDL_RenderPresent(renderer);
        SDL_Delay(20);
    }
    long long mdelta = clock_ns(CLOCK_MONOTONIC_COARSE) - m0;
    long long rdelta = clock_ns(CLOCK_REALTIME_COARSE) - r0;
    tprintf("monotonic coarse: delta=%lldns (expect %lldns)\n", mdelta, EXPECT_30);
    tprintf("realtime coarse: delta=%lldns (expect %lldns)\n", rdelta, EXPECT_30);
    assert(mdelta == EXPECT_30);
    assert(rdelta == EXPECT_30);
    tprintf("det-coarse: PASS\n");
    return 0;
}
