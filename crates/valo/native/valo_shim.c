// TODO(ai-review): generated, not yet audited
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <pthread.h>
#include <sys/uio.h>
#include <X11/Xlibint.h>
#include <X11/Xlib-xcb.h>

/* These are libxcb 1.17.0's default build-time layout options. */
#define HAVE_SENDMSG 1
#define XCB_QUEUE_BUFFER_SIZE 16384
/* Needed for sizeof(xcb_connection_t); the public XCB header keeps it opaque. */
#include "xcbint.h" // IWYU pragma: keep

#ifdef VALO_XCB_SIZE_PROBE
#include <stdio.h>

int main(void)
{
    printf("%zu\n", sizeof(xcb_connection_t));
    return 0;
}
#else

/* Requires a live Display without concurrent Xlib/XCB access, writable output
 * pointers, and dst_len writable bytes at dst. The runtime libxcb layout must
 * match xcbint.h. Returns bytes copied, or zero if unavailable or too small. */
size_t valo_x11_save(Display *display, uint64_t *last_request_read,
                     uint64_t *request, void *dst, size_t dst_len)
{
    if (!display || dst_len < sizeof(xcb_connection_t))
        return 0;

    xcb_connection_t *conn = XGetXCBConnection(display);
    if (!conn)
        return 0;

#ifdef X_DPY_GET_LAST_REQUEST_READ
    *last_request_read = X_DPY_GET_LAST_REQUEST_READ(display);
    *request = X_DPY_GET_REQUEST(display);
#else
    *last_request_read = display->last_request_read;
    *request = display->request;
#endif
    memcpy(dst, conn, sizeof(*conn));
    return sizeof(*conn);
}

/* Requires a live Display without concurrent Xlib/XCB access, and src_len
 * readable bytes at src. The runtime libxcb layout must match xcbint.h.
 * Returns zero on success. */
int valo_x11_restore(Display *display, uint64_t last_request_read,
                     uint64_t request, const void *src, size_t src_len)
{
    if (!display || src_len != sizeof(xcb_connection_t))
        return -1;

    xcb_connection_t *conn = XGetXCBConnection(display);
    if (!conn)
        return -1;

#ifdef X_DPY_SET_LAST_REQUEST_READ
    X_DPY_SET_LAST_REQUEST_READ(display, last_request_read);
    X_DPY_SET_REQUEST(display, request);
#else
    display->last_request_read = last_request_read;
    display->request = request;
#endif
    memcpy(conn, src, sizeof(*conn));
    return 0;
}

#endif
