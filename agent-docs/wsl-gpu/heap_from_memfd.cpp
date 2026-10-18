// Spike: can WSL's D3D12 wrap a memfd mapping as a GPU heap, and do GPU writes
// land in the memfd pages?
#include <directx/d3d12.h>
#include <directx/dxcore.h>
#include <dxguids/dxguids.h>
#include <wsl/winadapter.h>

#include <dlfcn.h>
#include <fcntl.h>
#include <sys/mman.h>
#include <unistd.h>

#include <cstdio>
#include <cstdlib>
#include <cstring>

#define CHECK(expr)                                                            \
  do {                                                                         \
    HRESULT hr_ = (expr);                                                      \
    if (FAILED(hr_)) {                                                         \
      fprintf(stderr, "FAILED %s: 0x%08x (line %d)\n", #expr, (unsigned)hr_,   \
              __LINE__);                                                       \
      exit(1);                                                                 \
    }                                                                          \
  } while (0)

static const size_t SIZE = 1 << 20;

int main() {
  void *lib = dlopen("/usr/lib/wsl/lib/libd3d12.so", RTLD_NOW);
  if (!lib) {
    fprintf(stderr, "dlopen: %s\n", dlerror());
    return 1;
  }
  auto create = (PFN_D3D12_CREATE_DEVICE)dlsym(lib, "D3D12CreateDevice");

  void *dxcore = dlopen("/usr/lib/wsl/lib/libdxcore.so", RTLD_NOW);
  if (!dxcore) {
    fprintf(stderr, "dlopen dxcore: %s\n", dlerror());
    return 1;
  }
  auto create_factory =
      (HRESULT (*)(REFIID, void **))dlsym(dxcore, "DXCoreCreateAdapterFactory");
  IDXCoreAdapterFactory *factory = nullptr;
  CHECK(create_factory(IID_PPV_ARGS(&factory)));
  IDXCoreAdapterList *adapters = nullptr;
  CHECK(factory->CreateAdapterList(1, &DXCORE_ADAPTER_ATTRIBUTE_D3D12_GRAPHICS,
                                   IID_PPV_ARGS(&adapters)));
  IDXCoreAdapter *adapter = nullptr;
  CHECK(adapters->GetAdapter(0, IID_PPV_ARGS(&adapter)));
  char name[256] = {};
  CHECK(adapter->GetProperty(DXCoreAdapterProperty::DriverDescription,
                             sizeof(name), name));
  printf("adapter: %s\n", name);

  ID3D12Device *dev = nullptr;
  CHECK(create(adapter, D3D_FEATURE_LEVEL_11_0, IID_PPV_ARGS(&dev)));

  ID3D12Device3 *dev3 = nullptr;
  ID3D12Device13 *dev13 = nullptr;
  HRESULT hr3 = dev->QueryInterface(IID_PPV_ARGS(&dev3));
  HRESULT hr13 = dev->QueryInterface(IID_PPV_ARGS(&dev13));
  printf("QI Device3:  0x%08x\nQI Device13: 0x%08x\n", (unsigned)hr3,
         (unsigned)hr13);
  if (!dev3)
    return 1;

  int memfd = memfd_create("heap", MFD_CLOEXEC);
  if (memfd < 0 || ftruncate(memfd, SIZE) != 0) {
    perror("memfd");
    return 1;
  }
  void *ptr = getenv("ANON") ? aligned_alloc(1 << 16, SIZE)
                             : mmap(nullptr, SIZE, PROT_READ | PROT_WRITE,
                                    MAP_SHARED, memfd, 0);
  if (ptr == MAP_FAILED) {
    perror("mmap");
    return 1;
  }
  memset(ptr, 0, SIZE);

  ID3D12Heap *heap = nullptr;
  HRESULT hr = dev3->OpenExistingHeapFromAddress(ptr, IID_PPV_ARGS(&heap));
  printf("Device3::OpenExistingHeapFromAddress(memfd):   0x%08x\n",
         (unsigned)hr);
  if (dev13 && !heap) {
    hr = dev13->OpenExistingHeapFromAddress1(ptr, SIZE, IID_PPV_ARGS(&heap));
    printf("Device13::OpenExistingHeapFromAddress1(memfd): 0x%08x\n",
           (unsigned)hr);
  }
  if (!heap)
    return 1;
  D3D12_HEAP_DESC hd = heap->GetDesc();
  printf("heap size=%llu flags=0x%x cpupage=%d\n",
         (unsigned long long)hd.SizeInBytes, hd.Flags,
         hd.Properties.CPUPageProperty);

  // Placed buffer on the imported heap; the GPU clears it via a copy from a
  // default-heap source.
  D3D12_RESOURCE_DESC rd = {};
  rd.Dimension = D3D12_RESOURCE_DIMENSION_BUFFER;
  rd.Width = SIZE;
  rd.Height = 1;
  rd.DepthOrArraySize = 1;
  rd.MipLevels = 1;
  rd.SampleDesc.Count = 1;
  rd.Layout = D3D12_TEXTURE_LAYOUT_ROW_MAJOR;
  rd.Flags = D3D12_RESOURCE_FLAG_ALLOW_CROSS_ADAPTER;
  ID3D12Resource *dst = nullptr;
  CHECK(dev->CreatePlacedResource(heap, 0, &rd, D3D12_RESOURCE_STATE_COMMON,
                                  nullptr, IID_PPV_ARGS(&dst)));

  // Source: an ordinary upload heap filled by the CPU with a pattern.
  D3D12_HEAP_PROPERTIES up = {};
  up.Type = D3D12_HEAP_TYPE_UPLOAD;
  rd.Flags = D3D12_RESOURCE_FLAG_NONE;
  ID3D12Resource *src = nullptr;
  CHECK(dev->CreateCommittedResource(&up, D3D12_HEAP_FLAG_NONE, &rd,
                                     D3D12_RESOURCE_STATE_GENERIC_READ, nullptr,
                                     IID_PPV_ARGS(&src)));
  void *srcp = nullptr;
  CHECK(src->Map(0, nullptr, &srcp));
  for (size_t i = 0; i < SIZE / 4; i++)
    ((uint32_t *)srcp)[i] = 0xC0DE0000u | (uint32_t)(i & 0xffff);
  src->Unmap(0, nullptr);

  D3D12_COMMAND_QUEUE_DESC qd = {};
  qd.Type = D3D12_COMMAND_LIST_TYPE_DIRECT;
  ID3D12CommandQueue *q = nullptr;
  CHECK(dev->CreateCommandQueue(&qd, IID_PPV_ARGS(&q)));
  ID3D12CommandAllocator *alloc = nullptr;
  CHECK(dev->CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT,
                                    IID_PPV_ARGS(&alloc)));
  ID3D12GraphicsCommandList *cl = nullptr;
  CHECK(dev->CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, alloc,
                               nullptr, IID_PPV_ARGS(&cl)));
  cl->CopyBufferRegion(dst, 0, src, 0, SIZE);
  CHECK(cl->Close());
  ID3D12CommandList *lists[] = {cl};
  q->ExecuteCommandLists(1, lists);

  ID3D12Fence *fence = nullptr;
  CHECK(dev->CreateFence(0, D3D12_FENCE_FLAG_NONE, IID_PPV_ARGS(&fence)));
  CHECK(q->Signal(fence, 1));
  while (fence->GetCompletedValue() < 1)
    usleep(1000);
  printf("device removed reason: 0x%08x\n",
         (unsigned)dev->GetDeviceRemovedReason());

  // Read through an independent path (pread on the memfd), as a second process
  // would see it.
  uint32_t buf[4];
  size_t bad = 0;
  for (size_t off = 0; off < SIZE; off += sizeof(buf)) {
    if (pread(memfd, buf, sizeof(buf), off) != sizeof(buf)) {
      perror("pread");
      return 1;
    }
    for (int j = 0; j < 4; j++) {
      uint32_t want = 0xC0DE0000u | (uint32_t)(((off / 4) + j) & 0xffff);
      if (buf[j] != want && bad++ < 4)
        printf("mismatch at %zu: got 0x%08x want 0x%08x\n", off + j * 4, buf[j],
               want);
    }
  }
  printf("%s: %zu/%zu words mismatched\n", bad ? "FAIL" : "OK", bad, SIZE / 4);
  return bad ? 1 : 0;
}
