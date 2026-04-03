//# link_mode: preload
//# link_flags: -lSDL3
// Verify checkpoint/restore works with an SDL3 program.
// A box moves right each frame. Checkpoint at frame 5, restore at frame 20.
// After restore the box position should be back to frame 5.
#include <assert.h>
#include <dlfcn.h>
#include <stdlib.h>
#include <SDL3/SDL.h>
#include "testlib.h"

int main() {
    void (*checkpoint)(void) = dlsym(RTLD_DEFAULT, "valo_checkpoint");
    void (*restore)(void) = dlsym(RTLD_DEFAULT, "valo_restore");
    assert(checkpoint && "valo_checkpoint not found");
    assert(restore && "valo_restore not found");

    setenv("SDL_VIDEODRIVER", "dummy", 1);
    SDL_Init(SDL_INIT_VIDEO);

    SDL_Window *window = NULL;
    SDL_Renderer *renderer = NULL;
    SDL_CreateWindowAndRenderer("valo-test", 640, 480, 0, &window, &renderer);
    assert(window && renderer);

    SDL_FRect rect = {200, 200, 20, 20};
    float checkpoint_x = -1;

    for (int frame = 0; frame < 100; frame++) {
        rect.x += 2;

        SDL_SetRenderDrawColor(renderer, 0, 0, 0, 255);
        SDL_RenderClear(renderer);
        SDL_SetRenderDrawColor(renderer, 255, 0, 0, 255);
        SDL_RenderFillRect(renderer, &rect);
        SDL_RenderPresent(renderer);

        tprintf("frame=%d x=%f\n", frame, rect.x);

        if (frame == 5) {
            checkpoint_x = rect.x;
            checkpoint();
        }
        if (frame == 20) {
            assert(rect.x != checkpoint_x);
            restore();
        }

        SDL_Event event;
        while (SDL_PollEvent(&event)) {}
    }

    SDL_DestroyRenderer(renderer);
    SDL_DestroyWindow(window);
    SDL_Quit();
}
