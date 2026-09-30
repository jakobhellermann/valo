// Shared test utilities. Use tprintf() instead of printf() for
// unbuffered output that survives checkpoint/restore.
#ifndef TESTLIB_H
#define TESTLIB_H

#include <stdarg.h>
#include <stdio.h>
#include <unistd.h>

void valo_checkpoint();
void valo_restore();
void valo_checkpoint_slot(unsigned slot);
void valo_restore_slot(unsigned slot);

// Unbuffered printf to stdout via write(). Necessary because stdio
// buffers are part of process memory and get restored along with
// everything else, losing buffered output.
__attribute__((format(printf, 1, 2)))
static void tprintf(const char *fmt, ...) {
    char buf[256];
    va_list ap;
    va_start(ap, fmt);
    int len = vsnprintf(buf, sizeof(buf), fmt, ap);
    va_end(ap);
    write(1, buf, len);
}

#endif
