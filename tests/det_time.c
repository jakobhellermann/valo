//# link_mode: preload
//# link_flags: -lSDL2
// Virtual time advances by exactly one frame per present, from 1s, and rewinds with a
// restore. Real time passes through raw syscalls, which valo never sees.
#include <assert.h>
#include <dlfcn.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>
#include <SDL2/SDL.h>
#include "testlib.h"

#define NS 1000000000LL
#define START (1 * NS)

static const clockid_t CLOCKS[] = {
    CLOCK_MONOTONIC, CLOCK_MONOTONIC_RAW, CLOCK_MONOTONIC_COARSE,
    CLOCK_REALTIME,  CLOCK_REALTIME_COARSE,  CLOCK_BOOTTIME,
};
#define CLOCK_COUNT (sizeof(CLOCKS) / sizeof(CLOCKS[0]))

static long long clock_ns(clockid_t clock) {
    struct timespec ts;
    clock_gettime(clock, &ts);
    return ts.tv_sec * NS + ts.tv_nsec;
}

static long long real_ns(void) {
    struct timespec ts;
    syscall(SYS_clock_gettime, CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * NS + ts.tv_nsec;
}

static void real_sleep_ms(long ms) {
    struct timespec ts = {0, ms * 1000000};
    syscall(SYS_nanosleep, &ts, NULL);
}

static SDL_Renderer *renderer;

static void present_frames(int count) {
    for (int i = 0; i < count; i++) {
        SDL_RenderClear(renderer);
        SDL_RenderPresent(renderer);
        real_sleep_ms(20);
    }
}

static void read_all(long long out[CLOCK_COUNT]) {
    for (size_t i = 0; i < CLOCK_COUNT; i++) out[i] = clock_ns(CLOCKS[i]);
}

static void assert_all_advanced(const long long before[CLOCK_COUNT], long long expected) {
    long long after[CLOCK_COUNT];
    read_all(after);
    for (size_t i = 0; i < CLOCK_COUNT; i++) assert(after[i] - before[i] == expected);
}

int main() {
    void (*checkpoint)(void) = dlsym(RTLD_DEFAULT, "valo_checkpoint");
    void (*restore)(void) = dlsym(RTLD_DEFAULT, "valo_restore");
    assert(checkpoint && restore);

    assert(clock_ns(CLOCK_MONOTONIC) == START);
    assert(clock_ns(CLOCK_BOOTTIME) == START);
    assert(clock_ns(CLOCK_REALTIME) == START);
    struct timeval tv;
    gettimeofday(&tv, NULL);
    assert(tv.tv_sec * NS + tv.tv_usec * 1000LL == START);
    assert(time(NULL) * NS == START);

    SDL_setenv("SDL_VIDEODRIVER", "dummy", 1);
    SDL_Init(SDL_INIT_VIDEO);
    SDL_Window *window = SDL_CreateWindow("valo-test", SDL_WINDOWPOS_UNDEFINED,
                                          SDL_WINDOWPOS_UNDEFINED, 320, 240, SDL_WINDOW_SHOWN);
    renderer = SDL_CreateRenderer(window, -1, SDL_RENDERER_SOFTWARE);

    long long before[CLOCK_COUNT];
    read_all(before);
    long long real_before = real_ns();
    present_frames(30);
    // 30 frames at 60 fps sum to exactly 500ms; real time took at least 30 x 20ms
    assert_all_advanced(before, NS / 2);
    assert(real_ns() - real_before >= 600000000LL);
    tprintf("30 frames: 500ms on every clock\n");

    long long at_checkpoint = clock_ns(CLOCK_MONOTONIC);
    checkpoint();
    assert(clock_ns(CLOCK_MONOTONIC) == at_checkpoint);
    read_all(before);
    present_frames(10);
    assert_all_advanced(before, 10 * NS / 60);
    tprintf("10 frames after the checkpoint: %lldns\n", 10 * NS / 60);

    restore();
    return 1;
}
