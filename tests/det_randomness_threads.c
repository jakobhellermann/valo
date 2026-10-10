//# link_mode: preload
//# link_flags: -pthread
// Threads drawing from the deterministic stream at once never get the same bytes.
#include <assert.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/random.h>
#include "testlib.h"

#define THREADS 4
#define DRAWS 200000

static uint64_t values[THREADS * DRAWS];

static void *draw(void *arg) {
    uint64_t *out = arg;
    for (int i = 0; i < DRAWS; i++) assert(getrandom(&out[i], sizeof(out[i]), 0) == sizeof(out[i]));
    return NULL;
}

static int compare(const void *a, const void *b) {
    uint64_t x = *(const uint64_t *)a, y = *(const uint64_t *)b;
    return (x > y) - (x < y);
}

int main() {
    pthread_t threads[THREADS];
    for (int t = 0; t < THREADS; t++) pthread_create(&threads[t], NULL, draw, &values[t * DRAWS]);
    for (int t = 0; t < THREADS; t++) pthread_join(threads[t], NULL);

    qsort(values, THREADS * DRAWS, sizeof(values[0]), compare);
    int duplicates = 0;
    for (int i = 1; i < THREADS * DRAWS; i++) duplicates += values[i] == values[i - 1];
    tprintf("duplicates: %d\n", duplicates);
    assert(duplicates == 0);
    return 0;
}
