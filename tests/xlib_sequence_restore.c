//# link_flags: -lX11 -lX11-xcb -lxcb
//# max_restores: 1
// An Xlib display whose xcb connection is also used directly, like a
// Vulkan WSI does. Xlib keeps the socket after its requests, and its
// flush bookkeeping lives in rewound memory: the restore must keep it
// consistent with the live sequence numbers, or Xlib's next flush tells
// xcb about requests that were never sent and the connection's replies
// stop matching their requests.
#define _GNU_SOURCE
#include <fcntl.h>
#include <poll.h>
#include <stdlib.h>
#include <unistd.h>
#include <X11/Xlib.h>
#include <X11/Xlib-xcb.h>
#include <xcb/xcb.h>
#include <xcb/xcbext.h>
#include "testlib.h"

#define ADVANCE 5000
#define ROUNDTRIP_BOUND_MS 2000
#define MARKER_PATH "/tmp/valo-xlib-sequence-restore-marker"

static int roundtrip_dead(xcb_connection_t *conn) {
    xcb_get_input_focus_cookie_t cookie = xcb_get_input_focus(conn);
    xcb_flush(conn);
    xcb_generic_error_t *e = NULL;
    void *reply = NULL;
    int waited_ms = 0;
    while (!xcb_poll_for_reply64(conn, cookie.sequence, &reply, &e)) {
        struct pollfd pfd = {.fd = xcb_get_file_descriptor(conn), .events = POLLIN};
        int r = poll(&pfd, 1, 10);
        if (r < 0)
            return 1;
        if (r == 0) {
            waited_ms += 10;
            if (waited_ms >= ROUNDTRIP_BOUND_MS)
                return 1;
        }
    }
    free(reply);
    free(e);
    return 0;
}

int main(void) {
    unlink(MARKER_PATH);
    Display *dpy = XOpenDisplay(NULL);
    if (!dpy) {
        tprintf("SKIP: no X server\n");
        return 0;
    }
    xcb_connection_t *conn = XGetXCBConnection(dpy);

    XNoOp(dpy);
    XFlush(dpy);

    valo_checkpoint();
    if (access(MARKER_PATH, F_OK) != 0) {
        int fd = open(MARKER_PATH, O_CREAT | O_WRONLY, 0644);
        if (fd >= 0)
            close(fd);
        for (int i = 0; i < ADVANCE; i++)
            XNoOp(dpy);
        XSync(dpy, False);
        valo_restore();
    }
    unlink(MARKER_PATH);

    XNoOp(dpy);
    XFlush(dpy);
    if (roundtrip_dead(conn)) {
        tprintf("FAIL: xcb roundtrip after an Xlib flush timed out\n");
        return 1;
    }
    tprintf("roundtrip ok\n");
    return 0;
}
