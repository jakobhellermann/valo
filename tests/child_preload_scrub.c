//# link_mode: preload
#include <assert.h>
#include <dlfcn.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>
#include "testlib.h"

static int child_main(void) {
    void *sym = dlsym(RTLD_DEFAULT, "valo_checkpoint");
    tprintf("child: valo %sloaded\n", sym ? "" : "not ");
    return sym ? 1 : 0;
}

int main(int argc, char **argv) {
    if (argc > 1) {
        return child_main();
    }

    const char *lp = getenv("LD_PRELOAD");
    if (lp && strstr(lp, "libvalo.so")) {
        tprintf("FAIL: LD_PRELOAD still contains libvalo: %s\n", lp);
        return 1;
    }
    tprintf("parent: LD_PRELOAD=%s\n", lp ? lp : "(unset)");

    pid_t pid = fork();
    assert(pid >= 0);
    if (pid == 0) {
        execl("/proc/self/exe", argv[0], "child", (char *)NULL);
        _exit(127);
    }

    int status = 0;
    assert(waitpid(pid, &status, 0) == pid);
    int code = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    tprintf("child exit=%d\n", code);
    return code == 0 ? 0 : 1;
}
