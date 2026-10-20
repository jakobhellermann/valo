// TODO(ai-review): generated, not yet audited
//# unity_version: 2019.4.40f1
//# args: -force-gfx-direct -custom
// Engine arguments already on the command line are not appended again.
#include "testlib.h"

int main(int argc, char **argv) {
    for (int i = 1; i < argc; i++)
        tprintf("argv[%d]=%s\n", i, argv[i]);
    return 0;
}
