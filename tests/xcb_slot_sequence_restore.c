//# link_flags: -lxcb -pthread
//# max_restores: 16
// A raw xcb connection through a sequence of checkpoints and restores that
// jumps between slots, including slots taken in timelines that themselves
// came from a restore. After every resume a fresh roundtrip must complete
// within a bound while a worker keeps request/reply traffic in flight.
//
// The step index lives in a file, so it survives the rewinds: each resume
// reads which operation comes next.
#define _GNU_SOURCE
#include <poll.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <xcb/xcb.h>
#include <xcb/xcbext.h>
#include "testlib.h"

#define BIG_VALUE_LEN (256 * 1024)
#define ROUNDTRIP_BOUND_MS 2000

static const char *const OPS[] = {"C1", "R1", "R1", "C2", "R1", "R2",
                                  "R1", "R2", "C3", "R2", "R3"};
#define OP_COUNT (sizeof(OPS) / sizeof(OPS[0]))

static char step_path[64];
static xcb_connection_t *conn;
static xcb_window_t window;
static atomic_ulong done_roundtrips = 0;

static void *worker(void *arg) {
    (void)arg;
    for (;;) {
        xcb_get_property_cookie_t pc = xcb_get_property(
            conn, 0, window, XCB_GET_PROPERTY_TYPE_ANY, XCB_ATOM_STRING, 0,
            BIG_VALUE_LEN / 4);
        free(xcb_get_property_reply(conn, pc, NULL));
        atomic_fetch_add_explicit(&done_roundtrips, 1, memory_order_relaxed);
    }
    return NULL;
}

static unsigned read_step(void) {
    FILE *f = fopen(step_path, "r");
    unsigned step = 0;
    if (f) {
        if (fscanf(f, "%u", &step) != 1)
            step = 0;
        fclose(f);
    }
    return step;
}

static void write_step(unsigned step) {
    FILE *f = fopen(step_path, "w");
    fprintf(f, "%u\n", step);
    fclose(f);
}

static int roundtrip_dead(void) {
    xcb_get_input_focus_cookie_t cookie = xcb_get_input_focus(conn);
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

// Resumed or live: the connection must still be serviceable and the worker
// must keep making progress.
static int check(unsigned step) {
    unsigned long before = atomic_load_explicit(&done_roundtrips, memory_order_relaxed);
    if (roundtrip_dead()) {
        tprintf("FAIL: step %u: roundtrip dead\n", step);
        return 1;
    }
    usleep(50000);
    if (atomic_load_explicit(&done_roundtrips, memory_order_relaxed) <= before) {
        tprintf("FAIL: step %u: worker stalled\n", step);
        return 1;
    }
    return 0;
}

int main(void) {
    snprintf(step_path, sizeof(step_path), "/tmp/valo-xcb-slot-sequence-%d", (int)getpid());
    unlink(step_path);

    conn = xcb_connect(NULL, NULL);
    if (xcb_connection_has_error(conn)) {
        tprintf("SKIP: no X server\n");
        return 0;
    }
    xcb_screen_t *screen = xcb_setup_roots_iterator(xcb_get_setup(conn)).data;
    window = xcb_generate_id(conn);
    xcb_create_window(conn, screen->root_depth, window, screen->root, 0, 0, 1, 1, 0,
                      XCB_WINDOW_CLASS_INPUT_OUTPUT, screen->root_visual, 0, NULL);
    static uint8_t value[BIG_VALUE_LEN];
    memset(value, 'x', sizeof(value));
    xcb_change_property(conn, XCB_PROP_MODE_REPLACE, window, XCB_ATOM_STRING,
                        XCB_ATOM_STRING, 8, sizeof(value), value);
    free(xcb_get_input_focus_reply(conn, xcb_get_input_focus(conn), NULL));

    pthread_t tid;
    pthread_create(&tid, NULL, worker, NULL);
    usleep(100000);

    for (unsigned step = read_step(); step < OP_COUNT; step = read_step()) {
        const char *op = OPS[step];
        unsigned slot = (unsigned)(op[1] - '0');
        write_step(step + 1);
        if (op[0] == 'C') {
            valo_checkpoint_slot(slot);
        } else {
            valo_restore_slot(slot);
        }
        unsigned resumed = read_step();
        if (check(resumed - 1))
            return 1;
        tprintf("after %s: ok\n", OPS[resumed - 1]);
        usleep(30000);
    }

    unlink(step_path);
    tprintf("sequence done\n");
    _exit(0);
}
