//# link_mode: preload
//# link_flags: -lSDL2
// Deterministic virtual time: the clock advances exactly one frame duration
// (the config framerate, default 60/1) per frame boundary, decoupled
// from wall time, and rewinds with a restore (the tick state is a plain
// static in snapshotted memory — the libTAS savestates-the-timer
// semantics, for free). Active by default: the harness sends
// Config::default() at the init handshake, whose `time.uncontrolled_time`
// is off.
//
// Checks:
//  1. frame lock: 30 presents advance CLOCK_MONOTONIC by exactly
//     30 x 1e9/60 = 500ms, while the real (raw-syscall) clock advances
//     by the true wall time (>= 30 x SDL_Delay(20)).
//  2. restore rewind: reading the clock right after the checkpoint and
//     after the 10-frame advance must match in both the live and the
//     restored timeline — the restored pass re-runs the exact same
//     asserts against the rewound tick state.
//
// The second valo_restore() exits via VALO_MAX_RESTORES.
#include <assert.h>
#include <dlfcn.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <SDL2/SDL.h>
#include "testlib.h"

#define FRAMES 30
#define ADVANCE_FRAMES 10
// 1e9/60 ns per frame; 30 frames sum to exactly 500ms (fractional carry
// included), 10 frames to 166,666,666 ns.
#define EXPECT_30 (500000000LL)
#define EXPECT_10 (10000000000LL / 60)

static long long virt_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static long long real_ns(void) {
    struct timespec ts;
    syscall(SYS_clock_gettime, CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

int main() {
    void (*checkpoint)(void) = dlsym(RTLD_DEFAULT, "valo_checkpoint");
    void (*restore)(void) = dlsym(RTLD_DEFAULT, "valo_restore");
    assert(checkpoint && "valo_checkpoint not found");
    assert(restore && "valo_restore not found");

    SDL_setenv("SDL_VIDEODRIVER", "dummy", 1);
    SDL_Init(SDL_INIT_VIDEO);
    SDL_Window *window = SDL_CreateWindow("valo-det-time",
        SDL_WINDOWPOS_UNDEFINED, SDL_WINDOWPOS_UNDEFINED,
        320, 240, SDL_WINDOW_SHOWN);
    SDL_Renderer *renderer = SDL_CreateRenderer(window, -1, SDL_RENDERER_SOFTWARE);

    // 1. Frame lock: the virtual delta over 30 presents is exactly
    //    30 x 1e9/60 = 500ms (the fractional carry sums cleanly over 30
    //    frames regardless of accumulator phase); the real (raw-syscall)
    //    delta is true wall time (>= 30 x SDL_Delay(20)) and must exceed
    //    the virtual one. No warm-up needed: the epoch is hardcoded, every
    //    read — before or after boundaries — shares it.
    long long v0 = virt_ns();
    long long r0 = real_ns();
    for (int i = 0; i < FRAMES; i++) {
        SDL_RenderClear(renderer);
        SDL_RenderPresent(renderer);
        SDL_Delay(20);
    }
    long long v1 = virt_ns();
    long long r1 = real_ns();
    long long vdelta = v1 - v0;
    long long rdelta = r1 - r0;
    tprintf("frame-lock: virtual delta=%lldns (expect %lldns), real slower than virtual: %s\n",
            vdelta, EXPECT_30, rdelta > vdelta ? "yes" : "no");
    assert(vdelta == EXPECT_30);
    assert(rdelta > vdelta + FRAMES * 1000000LL);

    // 2. Restore rewind: everything below re-runs after the restore with
    //    the rewound tick state. Identical asserts must hold both times.
    long long v_cp = virt_ns();
    checkpoint();
    long long v_post = virt_ns();
    assert(v_post == v_cp);
    for (int i = 0; i < ADVANCE_FRAMES; i++) {
        SDL_RenderClear(renderer);
        SDL_RenderPresent(renderer);
        SDL_Delay(20);
    }
    long long v_adv = virt_ns();
    tprintf("restore-rewind: advance=%lldns (expect %lld)\n",
            v_adv - v_post, EXPECT_10);
    assert(v_adv - v_post > EXPECT_10 - 2000 && v_adv - v_post < EXPECT_10 + 2000);

    tprintf("det-time: PASS\n");
    restore();
    return 1;
}
