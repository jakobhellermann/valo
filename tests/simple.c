#include <stdint.h>
#include <stdio.h>
#include <sys/select.h>
#include <sys/types.h>
#include <unistd.h>

#define VALO

void valo_init();
void valo_checkpoint();
void valo_restore();

int main() {
  pid_t pid = getpid();

#ifdef VALO
  valo_init();
#endif

  for (int i = 0; i < 15; i++) {
    usleep(1000 * 20);

    printf("%d\n", i);

    if (i == 5) {
#ifdef VALO
      valo_checkpoint();
#endif
    }
    if (i == 10) {
      // printf("%jd\n", (intmax_t)pid);
      // select(0, NULL, NULL, NULL, NULL);

#ifdef VALO
      valo_restore();
#endif
    }
  }

  return 0;
}
