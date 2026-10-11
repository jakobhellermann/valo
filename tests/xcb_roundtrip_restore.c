//# link_flags: -lxcb -pthread
//# max_restores: 8
// A raw xcb connection is not covered by the Xlib display quiesce: the
// checkpoint freezes it with request/reply traffic in flight. When the
// freeze catches the connection mid-message, the discarded timeline
// consumes the message's continuation, and the restored reader parks
// forever on bytes the server will never send again — the post-restore
// deadlock signature. After every restore the connection must stay
// serviceable: a fresh roundtrip completes within a bound, and the
// rewound worker keeps making progress.
//
// The worker fetches a property with a reply larger than the socket
// buffers, so the transfer spans many reads and a concurrent checkpoint
// regularly catches the connection mid-message.
//
// Restore rewinds the calling thread to the checkpoint, so each cycle
// runs two passes: the first (discarded) creates the marker and restores;
// the resumed pass sees the marker, skips the restore and asserts.
#define _GNU_SOURCE
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
#include <xcb/xcb.h>
#include <xcb/xcbext.h>
#include "testlib.h"

#define BIG_VALUE_LEN (1024 * 1024)
#define ROUNDTRIP_BOUND_MS 2000
#define CYCLES 16
#define MARKER_PATH "/tmp/valo-xcb-roundtrip-restore-marker"

static xcb_connection_t *conn;
static xcb_window_t window;
static atomic_int running = 1;
static atomic_ulong done_roundtrips = 0;

static void *worker(void *arg) {
    (void)arg;
    while (atomic_load_explicit(&running, memory_order_relaxed)) {
        xcb_get_property_cookie_t pc = xcb_get_property(
            conn, 0, window, XCB_GET_PROPERTY_TYPE_ANY, XCB_ATOM_STRING, 0,
            BIG_VALUE_LEN / 4);
        xcb_get_property_reply_t *preply =
            xcb_get_property_reply(conn, pc, NULL);
        if (preply)
            free(preply);

        xcb_generic_error_t *e = NULL;
        xcb_get_input_focus_cookie_t fc = xcb_get_input_focus(conn);
        free(xcb_get_input_focus_reply(conn, fc, &e));
        free(e);
        atomic_fetch_add_explicit(&done_roundtrips, 1, memory_order_relaxed);
    }
    return NULL;
}

// The main thread's libc sleeps pass virtual time without blocking; the worker needs real time.
static void real_sleep_us(long us) {
    struct timespec ts = {us / 1000000, us % 1000000 * 1000};
    syscall(SYS_nanosleep, &ts, NULL);
}

// A fresh roundtrip that must complete within the bound. A timeout means
// the connection's reader is parked waiting for bytes the server will
// never send — the post-restore deadlock signature.
static int roundtrip_dead(void) {
    xcb_get_input_focus_cookie_t cookie = xcb_get_input_focus(conn);
    xcb_generic_error_t *e = NULL;
    void *reply = NULL;
    int waited_ms = 0;
    while (!xcb_poll_for_reply64(conn, cookie.sequence, &reply, &e)) {
        struct pollfd pfd = {
            .fd = xcb_get_file_descriptor(conn),
            .events = POLLIN,
        };
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

    conn = xcb_connect(NULL, NULL);
    if (xcb_connection_has_error(conn)) {
        tprintf("SKIP: no X server\n");
        return 0;
    }
    xcb_screen_iterator_t screens =
        xcb_setup_roots_iterator(xcb_get_setup(conn));
    if (screens.rem < 1) {
        tprintf("SKIP: no X screen\n");
        return 0;
    }
    xcb_screen_t *screen = screens.data;
    window = xcb_generate_id(conn);
    xcb_create_window(conn, screen->root_depth, window, screen->root, 0, 0, 1,
                      1, 0, XCB_WINDOW_CLASS_INPUT_OUTPUT, screen->root_visual,
                      0, NULL);
    static uint8_t value[BIG_VALUE_LEN];
    memset(value, 'x', sizeof(value));
    xcb_change_property(conn, XCB_PROP_MODE_REPLACE, window, XCB_ATOM_STRING,
                        XCB_ATOM_STRING, 8, sizeof(value), value);
    xcb_generic_error_t *e = NULL;
    free(xcb_get_input_focus_reply(conn, xcb_get_input_focus(conn), &e));
    free(e);

    pthread_t tid;
    pthread_create(&tid, NULL, worker, NULL);
    real_sleep_us(200000);

    int cycles_done = 0;
    while (cycles_done < CYCLES) {
        valo_checkpoint_slot(1);
        // discarded timeline: the worker keeps consuming the stream
        real_sleep_us(30000);
        if (access(MARKER_PATH, F_OK) != 0) {
            int fd = open(MARKER_PATH, O_CREAT | O_WRONLY, 0644);
            if (fd >= 0)
                close(fd);
            valo_restore_slot(1);
        }
        // resumed pass: the connection must still be serviceable
        unsigned long before =
            atomic_load_explicit(&done_roundtrips, memory_order_relaxed);
        if (roundtrip_dead()) {
            tprintf("FAIL: cycle %d: post-restore roundtrip dead\n",
                    cycles_done);
            return 1;
        }
        real_sleep_us(100000);
        unsigned long after =
            atomic_load_explicit(&done_roundtrips, memory_order_relaxed);
        if (after <= before) {
            tprintf("FAIL: cycle %d: worker stalled (%lu)\n", cycles_done,
                    after);
            return 1;
        }
        unlink(MARKER_PATH);
        cycles_done++;
        tprintf("cycle %d ok\n", cycles_done);
    }

    atomic_store_explicit(&running, 0, memory_order_relaxed);
    pthread_join(tid, NULL);
    tprintf("all cycles ok\n");
    return 0;
}
