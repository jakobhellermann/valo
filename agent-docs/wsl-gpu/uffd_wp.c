// Cost of dirty tracking a memfd mapping with async uffd-wp + PAGEMAP_SCAN.
#define _GNU_SOURCE
#include <fcntl.h>
#include <linux/fs.h>
#include <linux/userfaultfd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define SIZE ((size_t)256 << 20)
#define PAGE 4096
#define NPAGES (SIZE / PAGE)

#define CHECK(cond, what)                                    \
    do {                                                     \
        if (!(cond)) {                                       \
            perror(what);                                    \
            exit(1);                                         \
        }                                                    \
    } while (0)

static double now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec + ts.tv_nsec * 1e-9;
}

static int pagemap;
static struct page_region regions[NPAGES];

// Returns the number of written pages in the mapping and re-protects them.
static size_t scan_and_reprotect(char *base) {
    struct pm_scan_arg arg = {
        .size = sizeof(arg),
        .flags = PM_SCAN_WP_MATCHING | PM_SCAN_CHECK_WPASYNC,
        .start = (unsigned long)base,
        .end = (unsigned long)(base + SIZE),
        .vec = (unsigned long)regions,
        .vec_len = NPAGES,
        .category_mask = PAGE_IS_WRITTEN,
        .return_mask = PAGE_IS_WRITTEN,
    };
    long n = ioctl(pagemap, PAGEMAP_SCAN, &arg);
    CHECK(n >= 0, "PAGEMAP_SCAN");
    size_t pages = 0;
    for (long i = 0; i < n; i++) pages += (regions[i].end - regions[i].start) / PAGE;
    return pages;
}

static void touch(char *base, size_t stride) {
    for (size_t p = 0; p < NPAGES; p += stride) base[p * PAGE] += 1;
}

int main(void) {
    int memfd = memfd_create("shadow", MFD_CLOEXEC);
    CHECK(memfd >= 0 && ftruncate(memfd, SIZE) == 0, "memfd");
    char *base = mmap(NULL, SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, memfd, 0);
    CHECK(base != MAP_FAILED, "mmap");
    memset(base, 0, SIZE);

    // Baseline: same touches with no tracking.
    double t = now();
    touch(base, 1);
    double base_ns = (now() - t) / NPAGES * 1e9;

    int uffd = syscall(SYS_userfaultfd, O_CLOEXEC | UFFD_USER_MODE_ONLY);
    CHECK(uffd >= 0, "userfaultfd");
    struct uffdio_api api = {.api = UFFD_API,
                             .features = UFFD_FEATURE_WP_ASYNC | UFFD_FEATURE_WP_HUGETLBFS_SHMEM};
    CHECK(ioctl(uffd, UFFDIO_API, &api) == 0, "UFFDIO_API");
    struct uffdio_register reg = {.range = {(unsigned long)base, SIZE}, .mode = UFFDIO_REGISTER_MODE_WP};
    CHECK(ioctl(uffd, UFFDIO_REGISTER, &reg) == 0, "UFFDIO_REGISTER");
    pagemap = open("/proc/self/pagemap", O_RDONLY);
    CHECK(pagemap >= 0, "open pagemap");

    t = now();
    struct uffdio_writeprotect wp = {.range = {(unsigned long)base, SIZE}, .mode = UFFDIO_WRITEPROTECT_MODE_WP};
    CHECK(ioctl(uffd, UFFDIO_WRITEPROTECT, &wp) == 0, "UFFDIO_WRITEPROTECT");
    printf("initial protect of %zu MiB: %.2f ms\n", SIZE >> 20, (now() - t) * 1e3);
    printf("untracked page touch: %.0f ns/page\n\n", base_ns);

    size_t strides[] = {1, 4, 16, 256};
    for (size_t i = 0; i < sizeof(strides) / sizeof(*strides); i++) {
        size_t stride = strides[i];
        size_t expect = (NPAGES + stride - 1) / stride;
        for (int run = 0; run < 3; run++) {
            t = now();
            touch(base, stride);
            double touch_s = now() - t;
            t = now();
            size_t dirty = scan_and_reprotect(base);
            double scan_s = now() - t;
            if (dirty != expect) {
                fprintf(stderr, "dirty mismatch: got %zu, want %zu\n", dirty, expect);
                return 1;
            }
            if (run == 2)
                printf("dirty %6zu/%zu pages (%4zu MiB): first-write %5.0f ns/page, scan+reprotect %6.2f ms\n",
                       dirty, NPAGES, dirty * PAGE >> 20, touch_s / dirty * 1e9, scan_s * 1e3);
        }
    }

    // Scan cost when nothing changed (lower bound per sync point).
    t = now();
    size_t dirty = scan_and_reprotect(base);
    printf("clean scan of %zu MiB: %.3f ms (dirty=%zu)\n", SIZE >> 20, (now() - t) * 1e3, dirty);
    return 0;
}
