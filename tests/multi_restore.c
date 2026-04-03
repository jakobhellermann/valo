//# max_restores: 5
// Verify multiple restores to the same checkpoint work.
#include <assert.h>
#include "testlib.h"

int main() {
    for (int i = 0; i < 10; i++) {
        tprintf("%d\n", i);
        if (i == 2) valo_checkpoint();
        if (i == 5) valo_restore();
    }
}
