//# link_mode: preload
//# link_flags: -lSDL2
// Verify checkpoint/restore works with an SDL2 program.
// A box moves right each frame. Checkpoint at frame 5, restore at frame 20.
// After restore the box position should be back to frame 5.
#include <assert.h>
#include <dlfcn.h>
#include <SDL2/SDL.h>
#include "testlib.h"

int main() {
    void (*checkpoint)(void) = dlsym(RTLD_DEFAULT, "valo_checkpoint");
    void (*restore)(void) = dlsym(RTLD_DEFAULT, "valo_restore");
    assert(checkpoint && "valo_checkpoint not found");
    assert(restore && "valo_restore not found");

    SDL_setenv("SDL_VIDEODRIVER", "dummy", 1);
    SDL_Init(SDL_INIT_VIDEO);

    SDL_Window *window = SDL_CreateWindow("valo-test",
        SDL_WINDOWPOS_UNDEFINED, SDL_WINDOWPOS_UNDEFINED,
        640, 480, SDL_WINDOW_SHOWN);
    SDL_Renderer *renderer = SDL_CreateRenderer(window, -1, SDL_RENDERER_SOFTWARE);

    SDL_Rect rect = {200, 200, 20, 20};
    int checkpoint_x = -1;

    for (int frame = 0; frame < 100; frame++) {
        rect.x += 2;

        SDL_SetRenderDrawColor(renderer, 0, 0, 0, 255);
        SDL_RenderClear(renderer);
        SDL_SetRenderDrawColor(renderer, 255, 0, 0, 255);
        SDL_RenderFillRect(renderer, &rect);
        SDL_RenderPresent(renderer);

        tprintf("frame=%d x=%d\n", frame, rect.x);

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
