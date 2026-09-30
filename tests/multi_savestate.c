//# max_restores: 1
// Loading A after saving B must replay from A's checkpoint.
#include "testlib.h"

int main(void) {
    volatile int value = 10;
    valo_checkpoint_slot(3);
    tprintf("A: %d\n", value);

    value = 20;
    valo_checkpoint_slot(17);
    tprintf("B: %d\n", value);

    valo_restore_slot(3);
    return 1;
}
