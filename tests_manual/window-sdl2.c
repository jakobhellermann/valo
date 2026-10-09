// SDL2 test for manual checkpoint/restore testing.
// Run with: make sdl2-test
// Control with: valo run --checkpoint-frame N --restore-frame M
#include <SDL2/SDL.h>
#include <stdio.h>

int main() {
    SDL_Init(SDL_INIT_VIDEO);
    fprintf(stderr, "SDL video driver: %s\n", SDL_GetCurrentVideoDriver());

    SDL_Window *window = SDL_CreateWindow("valo-sdl2-test",
        SDL_WINDOWPOS_UNDEFINED, SDL_WINDOWPOS_UNDEFINED,
        640, 480, SDL_WINDOW_SHOWN);
    SDL_Renderer *renderer = SDL_CreateRenderer(window, -1, SDL_RENDERER_SOFTWARE);
    SDL_RendererInfo info;
    if (SDL_GetRendererInfo(renderer, &info) == 0) {
        fprintf(stderr, "SDL renderer: %s\n", info.name);
    }

    int run = 1;
    int frame = 0;
    SDL_Rect rect = {200, 200, 20, 20};
    while (run) {
        SDL_SetRenderDrawColor(renderer, 0, 0, 0, 255);
        SDL_RenderClear(renderer);
        SDL_SetRenderDrawColor(renderer, 255 - frame % 256, frame % 256, frame % 256, 255);
        SDL_RenderFillRect(renderer, &rect);
        SDL_RenderPresent(renderer);
        SDL_Delay(10);

        SDL_Event event;
        while (SDL_PollEvent(&event)) {
            switch (event.type) {
                case SDL_QUIT:
                    run = 0;
                    break;
                case SDL_KEYDOWN:
                    switch (event.key.keysym.sym) {
                        case SDLK_ESCAPE: run = 0; break;
                        case SDLK_UP:    rect.y -= 10; break;
                        case SDLK_DOWN:  rect.y += 10; break;
                        case SDLK_LEFT:  rect.x -= 10; break;
                        case SDLK_RIGHT: rect.x += 10; break;
                        default: break;
                    }
                    break;
            }
        }

        frame++;
    }

    SDL_DestroyRenderer(renderer);
    SDL_DestroyWindow(window);
    SDL_Quit();
}
