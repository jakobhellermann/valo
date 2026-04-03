// Basic checkpoint + restore: verify loop counter rewinds correctly.
#include <assert.h>
#include "testlib.h"

int main() {
    for (int i = 0;; i++) {
        tprintf("%d\n", i);
        if (i == 2) valo_checkpoint();
        if (i == 4) valo_restore();
    }
}
