// Spike: can a CPU-visible D3D12 heap be shared with a second process on WSL,
// and do both map the same pages?
#include <directx/d3d12.h>
#include <directx/dxcore.h>
#include <dxguids/dxguids.h>
#include <wsl/winadapter.h>

#include <dlfcn.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cstdio>
#include <cstdlib>
#include <cstring>

static const UINT64 SIZE = 1 << 20;

#define CHECK(expr)                                                            \
  do {                                                                         \
    HRESULT hr_ = (expr);                                                      \
    if (FAILED(hr_)) {                                                         \
      fprintf(stderr, "[%d] FAILED %s: 0x%08x\n", getpid(), #expr,             \
              (unsigned)hr_);                                                  \
      exit(1);                                                                 \
    }                                                                          \
  } while (0)

static ID3D12Device *create_device() {
  void *lib = dlopen("/usr/lib/wsl/lib/libd3d12.so", RTLD_NOW);
  void *dxcore = dlopen("/usr/lib/wsl/lib/libdxcore.so", RTLD_NOW);
  if (!lib || !dxcore) {
    fprintf(stderr, "dlopen: %s\n", dlerror());
    exit(1);
  }
  auto create = (PFN_D3D12_CREATE_DEVICE)dlsym(lib, "D3D12CreateDevice");
  auto create_factory =
      (HRESULT (*)(REFIID, void **))dlsym(dxcore, "DXCoreCreateAdapterFactory");
  IDXCoreAdapterFactory *factory = nullptr;
  CHECK(create_factory(IID_PPV_ARGS(&factory)));
  IDXCoreAdapterList *adapters = nullptr;
  CHECK(factory->CreateAdapterList(1, &DXCORE_ADAPTER_ATTRIBUTE_D3D12_GRAPHICS,
                                   IID_PPV_ARGS(&adapters)));
  IDXCoreAdapter *adapter = nullptr;
  CHECK(adapters->GetAdapter(0, IID_PPV_ARGS(&adapter)));
  ID3D12Device *dev = nullptr;
  CHECK(create(adapter, D3D_FEATURE_LEVEL_11_0, IID_PPV_ARGS(&dev)));
  return dev;
}

static D3D12_RESOURCE_DESC buffer_desc(D3D12_RESOURCE_FLAGS flags) {
  D3D12_RESOURCE_DESC rd = {};
  rd.Dimension = D3D12_RESOURCE_DIMENSION_BUFFER;
  rd.Width = SIZE;
  rd.Height = 1;
  rd.DepthOrArraySize = 1;
  rd.MipLevels = 1;
  rd.SampleDesc.Count = 1;
  rd.Layout = D3D12_TEXTURE_LAYOUT_ROW_MAJOR;
  rd.Flags = flags;
  return rd;
}

static void send_fd(int sock, int fd) {
  char c = 0;
  iovec iov = {&c, 1};
  char ctrl[CMSG_SPACE(sizeof(int))] = {};
  msghdr msg = {};
  msg.msg_iov = &iov;
  msg.msg_iovlen = 1;
  msg.msg_control = ctrl;
  msg.msg_controllen = sizeof(ctrl);
  cmsghdr *cm = CMSG_FIRSTHDR(&msg);
  cm->cmsg_level = SOL_SOCKET;
  cm->cmsg_type = SCM_RIGHTS;
  cm->cmsg_len = CMSG_LEN(sizeof(int));
  memcpy(CMSG_DATA(cm), &fd, sizeof(int));
  if (sendmsg(sock, &msg, 0) != 1) {
    perror("sendmsg");
    exit(1);
  }
}

static int recv_fd(int sock) {
  char c;
  iovec iov = {&c, 1};
  char ctrl[CMSG_SPACE(sizeof(int))] = {};
  msghdr msg = {};
  msg.msg_iov = &iov;
  msg.msg_iovlen = 1;
  msg.msg_control = ctrl;
  msg.msg_controllen = sizeof(ctrl);
  if (recvmsg(sock, &msg, 0) != 1) {
    perror("recvmsg");
    exit(1);
  }
  int fd;
  memcpy(&fd, CMSG_DATA(CMSG_FIRSTHDR(&msg)), sizeof(int));
  return fd;
}

static void wait_byte(int sock) {
  char c;
  if (read(sock, &c, 1) != 1) {
    perror("read");
    exit(1);
  }
}

static void child(int sock, D3D12_RESOURCE_FLAGS res_flags) {
  int fd = recv_fd(sock);
  ID3D12Device *dev = create_device();
  ID3D12Heap *heap = nullptr;
  HRESULT hr = dev->OpenSharedHandle((HANDLE)(intptr_t)fd, IID_PPV_ARGS(&heap));
  printf("  child: OpenSharedHandle: 0x%08x\n", (unsigned)hr);
  if (FAILED(hr))
    exit(1);
  D3D12_RESOURCE_DESC rd = buffer_desc(res_flags);
  ID3D12Resource *buf = nullptr;
  CHECK(dev->CreatePlacedResource(heap, 0, &rd, D3D12_RESOURCE_STATE_COMMON,
                                  nullptr, IID_PPV_ARGS(&buf)));
  uint32_t *p = nullptr;
  CHECK(buf->Map(0, nullptr, (void **)&p));
  for (UINT64 i = 0; i < SIZE / 4; i++)
    p[i] = 0xC0DE0000u | (uint32_t)(i & 0xffff);
  printf("  child: wrote pattern via %p\n", (void *)p);
  if (write(sock, "w", 1) != 1) {
    perror("write");
    exit(1);
  }
  // Keep the mapping alive until the parent has checked it.
  wait_byte(sock);
  exit(0);
}

struct Variant {
  const char *name;
  D3D12_HEAP_PROPERTIES props;
  D3D12_HEAP_FLAGS flags;
  D3D12_RESOURCE_FLAGS res_flags;
};

static void run(const Variant &v) {
  printf("== %s\n", v.name);
  int sv[2];
  if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) {
    perror("socketpair");
    exit(1);
  }
  fflush(stdout);
  pid_t pid = fork();
  if (pid == 0) {
    close(sv[0]);
    child(sv[1], v.res_flags);
  }
  close(sv[1]);

  ID3D12Device *dev = create_device();
  D3D12_HEAP_DESC hd = {};
  hd.SizeInBytes = SIZE;
  hd.Properties = v.props;
  hd.Flags = v.flags;
  ID3D12Heap *heap = nullptr;
  HRESULT hr = dev->CreateHeap(&hd, IID_PPV_ARGS(&heap));
  printf("  parent: CreateHeap: 0x%08x\n", (unsigned)hr);
  HANDLE handle = nullptr;
  if (SUCCEEDED(hr)) {
    hr = dev->CreateSharedHandle(heap, nullptr, GENERIC_ALL, nullptr, &handle);
    printf("  parent: CreateSharedHandle: 0x%08x\n", (unsigned)hr);
  }
  if (FAILED(hr)) {
    kill(pid, SIGKILL);
    waitpid(pid, nullptr, 0);
    close(sv[0]);
    return;
  }

  D3D12_RESOURCE_DESC rd = buffer_desc(v.res_flags);
  ID3D12Resource *buf = nullptr;
  CHECK(dev->CreatePlacedResource(heap, 0, &rd, D3D12_RESOURCE_STATE_COMMON,
                                  nullptr, IID_PPV_ARGS(&buf)));
  uint32_t *p = nullptr;
  hr = buf->Map(0, nullptr, (void **)&p);
  printf("  parent: Map: 0x%08x\n", (unsigned)hr);
  if (FAILED(hr)) {
    kill(pid, SIGKILL);
    waitpid(pid, nullptr, 0);
    close(sv[0]);
    return;
  }
  memset(p, 0, SIZE);

  send_fd(sv[0], (int)(intptr_t)handle);
  wait_byte(sv[0]);

  UINT64 bad = 0;
  for (UINT64 i = 0; i < SIZE / 4; i++) {
    uint32_t want = 0xC0DE0000u | (uint32_t)(i & 0xffff);
    if (p[i] != want && bad++ < 3)
      printf("  mismatch at word %llu: 0x%08x\n", (unsigned long long)i, p[i]);
  }
  printf("  parent: %s, %llu/%llu words differ from the child's writes\n",
         bad ? "NOT SHARED" : "SHARED", (unsigned long long)bad,
         (unsigned long long)(SIZE / 4));
  if (write(sv[0], "d", 1) != 1) {
    perror("write");
    exit(1);
  }
  int status;
  waitpid(pid, &status, 0);
  close(sv[0]);
}

int main() {
  D3D12_HEAP_PROPERTIES upload = {};
  upload.Type = D3D12_HEAP_TYPE_UPLOAD;
  D3D12_HEAP_PROPERTIES readback = {};
  readback.Type = D3D12_HEAP_TYPE_READBACK;
  D3D12_HEAP_PROPERTIES wc_l0 = {D3D12_HEAP_TYPE_CUSTOM,
                                 D3D12_CPU_PAGE_PROPERTY_WRITE_COMBINE,
                                 D3D12_MEMORY_POOL_L0};
  D3D12_HEAP_PROPERTIES wb_l0 = {D3D12_HEAP_TYPE_CUSTOM,
                                 D3D12_CPU_PAGE_PROPERTY_WRITE_BACK,
                                 D3D12_MEMORY_POOL_L0};
  D3D12_HEAP_PROPERTIES wc_l1 = {D3D12_HEAP_TYPE_CUSTOM,
                                 D3D12_CPU_PAGE_PROPERTY_WRITE_COMBINE,
                                 D3D12_MEMORY_POOL_L1};
  D3D12_HEAP_PROPERTIES gpu_upload = {};
  gpu_upload.Type = D3D12_HEAP_TYPE_GPU_UPLOAD;

  const D3D12_HEAP_FLAGS shared =
      D3D12_HEAP_FLAG_SHARED | D3D12_HEAP_FLAG_ALLOW_ONLY_BUFFERS;
  const D3D12_HEAP_FLAGS cross = shared | D3D12_HEAP_FLAG_SHARED_CROSS_ADAPTER;
  const D3D12_RESOURCE_FLAGS xa = D3D12_RESOURCE_FLAG_ALLOW_CROSS_ADAPTER;

  D3D12_HEAP_PROPERTIES def = {};
  def.Type = D3D12_HEAP_TYPE_DEFAULT;
  const D3D12_HEAP_FLAGS shared_only = D3D12_HEAP_FLAG_SHARED;
  const D3D12_HEAP_FLAGS cross_only =
      D3D12_HEAP_FLAG_SHARED | D3D12_HEAP_FLAG_SHARED_CROSS_ADAPTER;

  Variant variants[] = {
      {"control: DEFAULT + SHARED (not CPU-visible)", def, shared,
       D3D12_RESOURCE_FLAG_NONE},
      {"control: UPLOAD, not shared", upload,
       D3D12_HEAP_FLAG_ALLOW_ONLY_BUFFERS, D3D12_RESOURCE_FLAG_NONE},
      {"UPLOAD + SHARED (no ALLOW_ONLY_BUFFERS)", upload, shared_only,
       D3D12_RESOURCE_FLAG_NONE},
      {"CUSTOM WC/L0 + SHARED_CROSS_ADAPTER (no ALLOW_ONLY_BUFFERS)", wc_l0,
       cross_only, xa},
      {"CUSTOM NA/L0 + SHARED_CROSS_ADAPTER",
       {D3D12_HEAP_TYPE_CUSTOM, D3D12_CPU_PAGE_PROPERTY_NOT_AVAILABLE,
        D3D12_MEMORY_POOL_L0},
       cross_only,
       xa},
      {"UPLOAD + SHARED", upload, shared, D3D12_RESOURCE_FLAG_NONE},
      {"READBACK + SHARED", readback, shared, D3D12_RESOURCE_FLAG_NONE},
      {"CUSTOM WC/L0 + SHARED", wc_l0, shared, D3D12_RESOURCE_FLAG_NONE},
      {"CUSTOM WB/L0 + SHARED", wb_l0, shared, D3D12_RESOURCE_FLAG_NONE},
      {"CUSTOM WC/L1 + SHARED", wc_l1, shared, D3D12_RESOURCE_FLAG_NONE},
      {"CUSTOM WC/L0 + SHARED_CROSS_ADAPTER", wc_l0, cross, xa},
      {"CUSTOM WB/L0 + SHARED_CROSS_ADAPTER", wb_l0, cross, xa},
      {"GPU_UPLOAD + SHARED", gpu_upload, shared, D3D12_RESOURCE_FLAG_NONE},
  };
  for (const Variant &v : variants)
    run(v);
  return 0;
}
