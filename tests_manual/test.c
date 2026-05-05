// usr/bin/clang "$0" && exec ./a.out "$@"
#define _FILE_OFFSET_BITS 64

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include <fcntl.h>
#include <unistd.h>

#include <sys/mman.h>
#include <sys/syscall.h>

static uint8_t file_static_buffer[4096 * 2];

static void print_pageinfo(void *mmapped_space, int npageoffset) {
  FILE *pagemap = fopen("/proc/self/pagemap", "rb");
  assert(pagemap != NULL);
  fseeko(pagemap,
         (off_t)((((uintptr_t)mmapped_space) / 4096 + npageoffset) *
                 sizeof(uint64_t)),
         SEEK_SET);
  uint64_t pageinfo = UINT64_MAX;
  fread(&pageinfo, sizeof(uint64_t), 1, pagemap);
  fprintf(stderr, "pageinfo = %016lX\n", pageinfo);
  fclose(pagemap);
}

int main(void) {
  int fd = syscall(SYS_memfd_create, "test_file", 0);

  /*int fd = shm_open("/test_file", O_RDWR | O_CREAT | O_EXCL, 0600);
  if (fd == -1)
  {
          fprintf(stderr, "Failed to create test file\n");
          return -1;
  }
  shm_unlink("/test_file");*/

  FILE *f = fdopen(fd, "wb+");
  if (f == NULL) {
    fprintf(stderr, "Failed to open file\n");
    return -1;
  }

  memset(file_static_buffer, 0xFF, sizeof(file_static_buffer));
  fseek(f, 0, SEEK_SET);
  fwrite(file_static_buffer, sizeof(uint8_t), sizeof(file_static_buffer), f);

  void *mmapped_space = mmap(NULL, sizeof(file_static_buffer),
                             PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
  if (mmapped_space == MAP_FAILED) {
    fprintf(stderr, "Mapping failed");
    return -1;
  }

  for (int i = 0; i < sizeof(file_static_buffer); i++) {
    volatile int test;
    test = ((uint8_t *)mmapped_space)[i];
  }

  {
    fprintf(stderr, "Sleeping for 2 minutes, pid = %d, mapping = %p\n",
            getpid(), mmapped_space);
    print_pageinfo(mmapped_space, 0);
    print_pageinfo(mmapped_space, 1);
    // sleep(60 * 2);
  }

  for (int i = 0; i < sizeof(file_static_buffer) / 2; i++) {
    ((uint8_t *)mmapped_space)[i] = 0x00;
  }

  memset(file_static_buffer, 0x69, sizeof(file_static_buffer));
  fseek(f, 0, SEEK_SET);
  fwrite(file_static_buffer, sizeof(uint8_t), sizeof(file_static_buffer) / 2,
         f);

  for (int i = 0; i < sizeof(file_static_buffer); i++) {
    fprintf(stderr, "%02X", ((uint8_t *)mmapped_space)[i]);
    // volatile int test;
    // test = ((uint8_t*)mmapped_space)[i];
  }
  fprintf(stderr, "\n");

  {
    fprintf(stderr, "Sleeping for 2 minutes\n");
    print_pageinfo(mmapped_space, 0);
    print_pageinfo(mmapped_space, 1);
    // sleep(60 * 2);
  }
}
