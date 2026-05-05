// Simple X11 window test. Links to X11 directly.
// Run with: make x11-test
#include <X11/Xlib.h>
#include <stdio.h>
#include <unistd.h>

int main() {
    Display *display = XOpenDisplay(NULL);
    if (!display) {
        fprintf(stderr, "Cannot open display\n");
        return 1;
    }

    int screen = DefaultScreen(display);
    Window window = XCreateSimpleWindow(display, RootWindow(display, screen),
        10, 10, 640, 480, 1,
        BlackPixel(display, screen), WhitePixel(display, screen));

    XSelectInput(display, window, ExposureMask | KeyPressMask);
    XMapWindow(display, window);

    GC gc = XCreateGC(display, window, 0, NULL);

    int frame = 0;
    int run = 1;
    while (run) {
        // Draw
        XSetForeground(display, gc, BlackPixel(display, screen));
        XFillRectangle(display, window, gc, 0, 0, 640, 480);
        XSetForeground(display, gc, WhitePixel(display, screen));
        int x = 200 + frame * 2;
        XFillRectangle(display, window, gc, x, 200, 20, 20);
        XFlush(display);

        usleep(16000); // ~60fps

        // Events
        while (XPending(display)) {
            XEvent event;
            XNextEvent(display, &event);
            if (event.type == KeyPress)
                run = 0;
        }

        // fprintf(stderr, "frame=%d x=%d\n", frame, x);
        frame++;
    }

    XFreeGC(display, gc);
    XDestroyWindow(display, window);
    XCloseDisplay(display);
    return 0;
}
