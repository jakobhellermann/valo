// Verify deep stack frames survive checkpoint/restore.
#include <assert.h>
#include "testlib.h"

void recurse(int depth, int max_depth) {
    int local = depth * 111;
    if (depth == max_depth) {
        tprintf("bottom: %d %d\n", depth, local);
        valo_checkpoint();
        tprintf("restored: %d %d\n", depth, local);
        assert(depth == max_depth);
        assert(local == depth * 111);
        valo_restore();
    } else {
        recurse(depth + 1, max_depth);
    }
}

int main() {
    recurse(0, 50);
}
