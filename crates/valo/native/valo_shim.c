// TODO(ai-review): generated, not yet audited
#include <stddef.h>
#include <stdint.h>
#include <alloca.h>
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

static const size_t PARK_STACK_HEADROOM = 16 * 1024;

int valo_park_on_stack(uintptr_t stack_low, void (*park)(void *), void *arg)
{
    volatile char marker;
    uintptr_t sp = (uintptr_t)&marker;
    if (sp <= stack_low + 2 * PARK_STACK_HEADROOM)
        return -1;
    volatile char *padding = alloca(sp - stack_low - PARK_STACK_HEADROOM);
    padding[0] = 0;
    park(arg);
    return 0;
}

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

/* TODO(ai-review): generated, not yet audited — TEMP debug instrumentation:
 * read-only snapshot of Xlib/XCB sequence + queue bookkeeping. Never mutates
 * state; safe to call from the signal handler (no allocation, no syscalls). */
struct valo_xcb_counters {
    uint64_t has_error;
    uint64_t xlib_request;
    uint64_t xlib_last_request_read;
    uint64_t out_request;
    uint64_t out_request_written;
    uint64_t out_writing;
    uint64_t out_queue_len;
    uint64_t in_request_read;
    uint64_t in_request_completed;
    uint64_t in_request_expected;
    uint64_t in_reading;
    uint64_t in_queue_len;
    uint64_t in_events;
    uint64_t in_events_tail_target;
    uint64_t in_current_reply;
    uint64_t in_replies;
    uint64_t in_pending_replies;
    uint64_t in_readers;
    uint64_t in_special_waiters;
    uint64_t in_special_events;
    uint64_t iolock_state;
    uint64_t iolock_owner;
};

/* Requires a live Display without concurrent Xlib/XCB access and a writable
 * `out`. Never dereferences list nodes — reads only the connection struct. */
void valo_x11_debug_counters(Display *display, struct valo_xcb_counters *out)
{
    memset(out, 0, sizeof(*out));
    if (!display)
        return;

#ifdef X_DPY_GET_REQUEST
    out->xlib_request = X_DPY_GET_REQUEST(display);
    out->xlib_last_request_read = X_DPY_GET_LAST_REQUEST_READ(display);
#else
    out->xlib_request = display->request;
    out->xlib_last_request_read = display->last_request_read;
#endif

    xcb_connection_t *conn = XGetXCBConnection(display);
    if (!conn) {
        out->has_error = 2;
        return;
    }
    out->has_error = conn->has_error;
    out->out_request = conn->out.request;
    out->out_request_written = conn->out.request_written;
    out->out_writing = conn->out.writing;
    out->out_queue_len = conn->out.queue_len;
    out->in_request_read = conn->in.request_read;
    out->in_request_completed = conn->in.request_completed;
    out->in_request_expected = conn->in.request_expected;
    out->in_reading = conn->in.reading;
    out->in_queue_len = conn->in.queue_len;
    out->in_events = (uintptr_t)conn->in.events;
    out->in_events_tail_target = (uintptr_t)*conn->in.events_tail;
    out->in_current_reply = (uintptr_t)conn->in.current_reply;
    out->in_replies = (uintptr_t)conn->in.replies;
    out->in_pending_replies = (uintptr_t)conn->in.pending_replies;
    out->in_readers = (uintptr_t)conn->in.readers;
    out->in_special_waiters = (uintptr_t)conn->in.special_waiters;
    out->in_special_events = (uintptr_t)conn->in.special_events;
    out->iolock_state = conn->iolock.__data.__lock;
    out->iolock_owner = conn->iolock.__data.__owner;
}

/* Requires a live connection without concurrent Xlib/XCB access and dst_len
 * writable bytes at dst. The runtime libxcb layout must match xcbint.h.
 * Returns bytes copied, or zero if unavailable or too small. */
size_t valo_xcb_save(xcb_connection_t *conn, void *dst, size_t dst_len)
{
    if (!conn || dst_len < sizeof(xcb_connection_t))
        return 0;
    memcpy(dst, conn, sizeof(*conn));
    return sizeof(*conn);
}

/* Requires a live connection without concurrent Xlib/XCB access, and src_len
 * readable bytes at src. The runtime libxcb layout must match xcbint.h.
 * Returns zero on success. */
/* Apply restore-time I/O state onto a connection that process memory rewound
 * to checkpoint state. The rewound connection is internally consistent (list
 * nodes, mutexes and suspended threads all match checkpoint state); the only
 * divergence is toward the X server, which kept processing requests. So take
 * I/O progress (sequence counters + embedded queues, which encode the server's
 * request-sequence position) from `src`, and keep everything that references
 * rewound heap or resumed thread stacks (list heads, sync primitives, socket
 * ownership, the "I am the one doing I/O" flags) from the rewound connection.
 *
 * Requires: every thread is suspended at its checkpoint-time stack, and there
 * is no concurrent Xlib/XCB access. Returns zero on success. */
static int xcb_apply_restored_io_state(xcb_connection_t *conn, const xcb_connection_t *src)
{
    if (!conn || !src)
        return -1;

    /* Checkpoint-time state referencing rewound memory or resumed stacks. */
    pthread_mutex_t iolock = conn->iolock;
    _xcb_ext ext = conn->ext;
    _xcb_xid xid = conn->xid;
    struct {
        pthread_cond_t event_cond;
        int reading;
        struct reply_list *current_reply;
        struct reply_list **current_reply_tail;
        _xcb_map *replies;
        struct event_list *events;
        struct event_list **events_tail;
        struct reader_list *readers;
        struct special_list *special_waiters;
        struct pending_reply *pending_replies;
        struct pending_reply **pending_replies_tail;
        struct xcb_special_event *special_events;
    } in = {
        conn->in.event_cond,       conn->in.reading,
        conn->in.current_reply,    conn->in.current_reply_tail,
        conn->in.replies,          conn->in.events,
        conn->in.events_tail,      conn->in.readers,
        conn->in.special_waiters,  conn->in.pending_replies,
        conn->in.pending_replies_tail,
        conn->in.special_events,
    };
    struct {
        pthread_cond_t cond;
        pthread_cond_t socket_cond;
        void (*return_socket)(void *closure);
        void *socket_closure;
        int socket_moving;
        int writing;
        pthread_mutex_t reqlenlock;
    } out = {
        conn->out.cond,            conn->out.socket_cond,
        conn->out.return_socket,   conn->out.socket_closure,
        conn->out.socket_moving,   conn->out.writing,
        conn->out.reqlenlock,
    };

    memcpy(conn, src, sizeof(*conn));

    conn->iolock = iolock;
    conn->ext = ext;
    conn->xid = xid;
    conn->in.event_cond = in.event_cond;
    conn->in.reading = in.reading;
    conn->in.current_reply = in.current_reply;
    conn->in.current_reply_tail = in.current_reply_tail;
    conn->in.replies = in.replies;
    conn->in.events = in.events;
    conn->in.events_tail = in.events_tail;
    conn->in.readers = in.readers;
    conn->in.special_waiters = in.special_waiters;
    conn->in.pending_replies = in.pending_replies;
    conn->in.pending_replies_tail = in.pending_replies_tail;
    conn->in.special_events = in.special_events;
    conn->out.cond = out.cond;
    conn->out.socket_cond = out.socket_cond;
    conn->out.return_socket = out.return_socket;
    conn->out.socket_closure = out.socket_closure;
    conn->out.socket_moving = out.socket_moving;
    conn->out.writing = out.writing;
    conn->out.reqlenlock = out.reqlenlock;
    return 0;
}

/* Requires a live connection without concurrent Xlib/XCB access, and src_len
 * readable bytes at src. The runtime libxcb layout must match xcbint.h.
 * Returns zero on success. */
int valo_xcb_restore(xcb_connection_t *conn, const void *src, size_t src_len)
{
    if (!conn || src_len != sizeof(xcb_connection_t))
        return -1;
    return xcb_apply_restored_io_state(conn, (const xcb_connection_t *)src);
}

/* Requires a writable `out`. `conn` may be null (returns has_error=2). Never
 * dereferences list nodes — reads only the connection struct. No allocation,
 * no syscalls: safe from the signal handler. */
void valo_xcb_debug_counters(xcb_connection_t *conn, struct valo_xcb_counters *out)
{
    memset(out, 0, sizeof(*out));
    if (!conn) {
        out->has_error = 2;
        return;
    }
    out->has_error = conn->has_error;
    out->out_request = conn->out.request;
    out->out_request_written = conn->out.request_written;
    out->out_writing = conn->out.writing;
    out->out_queue_len = conn->out.queue_len;
    out->in_request_read = conn->in.request_read;
    out->in_request_completed = conn->in.request_completed;
    out->in_request_expected = conn->in.request_expected;
    out->in_reading = conn->in.reading;
    out->in_queue_len = conn->in.queue_len;
    out->in_events = (uintptr_t)conn->in.events;
    out->in_events_tail_target = (uintptr_t)*conn->in.events_tail;
    out->in_current_reply = (uintptr_t)conn->in.current_reply;
    out->in_replies = (uintptr_t)conn->in.replies;
    out->in_pending_replies = (uintptr_t)conn->in.pending_replies;
    out->in_readers = (uintptr_t)conn->in.readers;
    out->in_special_waiters = (uintptr_t)conn->in.special_waiters;
    out->in_special_events = (uintptr_t)conn->in.special_events;
    out->iolock_state = conn->iolock.__data.__lock;
    out->iolock_owner = conn->iolock.__data.__owner;
}

/* The xcb connection backing an Xlib Display (may equal a directly tracked
 * xcb_connect connection — callers must not restore it twice). */
xcb_connection_t *valo_x11_conn(Display *display)
{
    if (!display)
        return NULL;
    return XGetXCBConnection(display);
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
    return xcb_apply_restored_io_state(conn, (const xcb_connection_t *)src);
}

#endif
