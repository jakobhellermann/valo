# Langsame CPU-Reads aus host-visible GPU-Speicher (anv)

Stand: 2026-10-21. Zurückgestellt; betrifft gfxstream+kumquat auf anv
generell, unabhängig von valo und vom Restore.

## Befund

Gemessen auf Intel Arc B390 (PTL, xe, integriert) mit einem Benchmark direkt
auf anv (64 MiB, `volatile`-Schleife, best-of-n):

| Memory-Type                          | Schreiben | Lesen     |
| ------------------------------------ | --------- | --------- |
| `HOST_CACHED`, ohne Export           | 19,5 GB/s | 27,9 GB/s |
| `HOST_CACHED`, exportierbar (dmabuf) | 65 GB/s   | 0,10 GB/s |
| nicht cached, egal ob exportierbar   | 65 GB/s   | 0,10 GB/s |

`vkMapMemory` und `mmap` der exportierten dmabuf verhalten sich gleich
(gleicher DRM-Fake-Offset, dieselben Seiten, gleicher Durchsatz).

gfxstream legt in kumquat jede host-visible Allokation exportierbar an, damit
das Spiel eine fd bekommt. Damit liest das Spiel auch aus `HOST_CACHED`-Typen
uncached, rund 280-mal langsamer als nativ.

## Ursache

anv wählt für exportierbare BOs den Scanout-PAT-Eintrag
(`src/intel/vulkan/anv_device.c`, `anv_device_get_pat_entry`):

```c
if (alloc_flags & (ANV_BO_ALLOC_EXTERNAL | ANV_BO_ALLOC_SCANOUT))
   return &device->info->pat.scanout;
```

Auf xe wird daraus `DRM_XE_GEM_CPU_CACHING_WC`
(`src/intel/vulkan/xe/anv_kmd_backend.c`). Importierte BOs bekommen dagegen
`pat.cached_coherent`.

## Optionen

1. **SystemBlob (Host-Pointer-Import).** kumquat legt host-visible Speicher
   als memfd an, gfxstream importiert ihn per `VK_EXT_external_memory_host`,
   das Spiel mappt den memfd direkt. anv behandelt das als Import, also
   cached. Die Schalter existieren: `RutabagaBuilder::set_use_system_blob`
   (in kumquat nur für Apple an), gfxstream-Feature `SystemBlob`
   (`STREAM_RENDERER_FLAGS_USE_SYSTEM_BLOB`). Nebeneffekte: kein Treiber im
   Spielprozess, `/proc/maps` zeigt die memfd-Inode.
   - gfxstream warnt „USE_SYSTEM_BLOB has only been tested on Windows"; die
     Memory-Type-Logik ist auf Apple zugeschnitten
     (`mGuestAppleSystemBlobMemoryTypeIndex` in
     `vk_emulated_physical_device_memory.cpp`).
   - Auf dGPUs läge host-visible Speicher im System-RAM statt in ReBAR-VRAM;
     GPU-Zugriffe darauf könnten langsamer werden. Ungemessen.
2. **anv-Patch:** externe, nicht für Scanout bestimmte BOs auf integrierten
   GPUs cached anlegen. Greift in anvs Policy für alle Importeure (Display,
   Video) ein.
3. **Hinnehmen:** Spiele schreiben meist WC und lesen selten zurück. Ob HK
   Readback aus gemapptem Speicher macht, ist ungemessen.

## Nächster Schritt

Den Benchmark um einen per `VK_EXT_external_memory_host` importierten memfd
erweitern und Lese-/Schreibrate auf anv messen, bevor gfxstreams `SystemBlob`
unter Linux angefasst wird. Auf AMD prüfen, ob RADV exportierbaren Speicher
ebenfalls uncached anlegt.

## AMD/RADV: nachgemessen (2026-10-21, RX 6700 XT, kein ReBAR)

Die offene Frage ist beantwortet: Auf RADV liegt es nicht am Export, sondern
am Memory-Type selbst. Mit einem direkten Benchmark über `vkMapMemory` und
`mmap` des exportierten dma-buf (64 MiB, `volatile`-Schleife, best-of-5):

| Memory-Type (heap 0 = GTT)              | Lesen     | Schreiben |
| --------------------------------------- | --------- | --------- |
| `HOST_VISIBLE\|HOST_COHERENT`           | 0,60 GB/s | 8,1 GB/s  |
| `…\|HOST_CACHED`                        | 63 GB/s   | 30,8 GB/s |

Plain, exportiert und fd-mmap sind identisch — anv's PAT-Falle (Export ⇒
Scanout-Eintrag) existiert so auf RADV nicht. Nicht-cached heißt hier, dass
RADV das Mapping write-combined anlegt; Reads können nicht kombiniert
werden und laufen auf hunderte MB/s.

Das war der Grund für den langsamen GPU-Snapshot: die mapped-memory-Loop in
`VkDecoderGlobalState::Impl::save` liest den gesamten Inhalt aller
gemappten `VkDeviceMemory` direkt aus dem Mapping. HK bei Frame 3600:
1,17 GB × 0,6 GB/s ≈ 2 s GPU-Wait (Renderer-Snapshot, tmpfs-Write-Kalibrierung
3,2 GB/s — die Write-Seite war unschuldig). Bild- und Buffer-Content liefen
bereits über das cached Staging aus „read back saved content through cached
memory".

Gefixt in gfxstream (`vulkan: allocate guest host-visible memory from the
cached host type`): die Gast-Typ-Tabelle zeigt die Properties unverändert,
wählt für die Host-Allokation aber den `HOST_CACHED`-Sibling desselben Heaps,
wenn es einen gibt. Suchkriterium ist gleicher Heap plus Obermenge der Flags,
also No-op auf Hosts ohne coherent-cached host-visible Typ. Gemessen: GPU-Wait
2,23 s → 0,52 s (Rest: tmpfs-Write-Pfad des Full-Readbacks, ~0,37 s bei
1,17 GB), Restore-Roundtrip und `gpu_host_visible_*` grün. Da dieselbe
Allokation auch das Guest-Mapping ist, lesen und schreiben zink-Uploads jetzt
ebenfalls cached.
