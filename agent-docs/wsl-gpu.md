<!-- // TODO(ai-review): generated, not yet audited -->

# valo unter WSL2/WSLg: GPU-Pfad

Stand: Oktober 2026. **Der GPU-Pfad (gfxstream/kumquat) läuft unter WSLg mit
echter GPU über dzn**, mit kopierbasierter Kohärenz für host-sichtbaren
Speicher. Hollow Knight läuft gesteuert inklusive Checkpoint/Restore,
`just profile` läuft durch. Etwas langsamer als nativ. Die Testprogramme der
Untersuchung liegen in `agent-docs/wsl-gpu/`.

Getestete Umgebung: Windows 10.0.26200, WSL 3.0.3, WSLg 1.0.79, WSL-Kernel
6.18.54.1, „Direct3D for WSL“ 1.611.1, Mesa 26.2.5 (dzn), RX 6700 XT + iGPU,
Ryzen 7 7700X.

## Benutzen

Aus dem valo-Repo, mit kumquat und dem gfxstream-ICD aus den lokalen Builds
(`VALO_KUMQUAT_BIN`, `VALO_GFXSTREAM_ICD`, z. B. in einer lokalen `.env`):

```sh
just run-hollowknight-gpu --tui --uncontrolled-input   # oder: just profile 60 120
```

valo-launch schaltet den Kopiermodus selbst ein, wenn `/dev/dxg` existiert
(WSL-GPU) oder `/dev/udmabuf` nicht lesbar ist, und setzt dann
`--renderer-features=VirtioGpuNext:disabled` für kumquat. Unter WSL bekommt
das Spiel zusätzlich `MESA_VK_WSI_DEBUG=sw`. gfxstream wählt dzn ohne
`VK_ICD_FILENAMES` selbst (diskrete GPU).

## Was nötig war

### 1. Kopiermodus für host-sichtbaren Speicher (der Kern)

dzn kann unter WSL weder Guest-Pages importieren noch mappbaren Host-Speicher
exportieren (siehe Sackgassen). Host-sichtbarer Speicher ist deshalb ein
Guest-Schattenpuffer; `vkFlushMappedMemoryRanges`/`vkInvalidateMappedMemoryRanges`
tragen die Bytes durch den Command-Stream.

- gfxstream: Capset-Feld `hostVisibleMemoryCopy` statt `deferredMapping`, wenn
  `VirtioGpuNext` aus ist. Der Host mappt host-sichtbaren Speicher selbst
  (`needToMap`), legt ihn auf einen gecachten Host-Type (dzn: 0,3 vs. ~10 GB/s),
  exportiert ihn nicht (dzn lehnt exportierbaren host-sichtbaren Speicher ab)
  und überspringt `VulkanEnsureCachedCoherentMemoryAvailable` (kein Type ist
  coherent).
- Mesa: Kopiermodus im `ResourceTracker` (dedizierte Allokation +
  Schattenpuffer, `usingDirectMapping()` aus dem Capset); Encoder mit
  konsistenten Größen bei `VK_WHOLE_SIZE`, Invalidate folgt der Längenangabe des
  Hosts; WSI ohne coherent Memory (Fallback auf host-sichtbar + Invalidate vor
  dem CPU-Readback im Software-Pfad).
- valo: die gpu-Tests flushen/invalidieren explizit statt `HOST_COHERENT`
  vorauszusetzen.
- Umgebung: `MESA_VK_WSI_DEBUG=sw`, weil XWayland unter WSLg kein DRI3 hat.

### 2. gfxstream-Fehler, die nur dzn nicht toleriert

Echte Fehler, auch nativ korrekt zu beheben, sichtbar nur auf dzn:

- 1.1-Instance-Capabilities (Properties2, External Memory/Semaphore/Fence) als
  Kern behandeln: Mit dzn als einzigem ICD listet der Loader die
  Extension-Namen nicht (Absturz an der Device-UUID, keine exportierten
  Handles). Die Guest-Instance aktiviert nur tatsächlich gelistete Namen.
- Snapshot-Barrieren mit `MEMORY_READ|MEMORY_WRITE` statt `~VK_ACCESS_NONE`
  (reservierte Bits → `vkEndCommandBuffer` scheitert).
- Mip-Extents auf mindestens 1 klemmen: 2048×1024 Mip 11 ergab `1x0`, eine
  Kopie mit Höhe 0 ist ungültiges Vulkan, D3D12 lehnt die Kommandoliste ab.

### 3. Leistung

- Flush-Decoder: erst alle Ranges abwarten, dann einmal kopieren. Ein
  Flush-Paket kommt in vielen Ring-Chunks und wird nach jedem neu dekodiert;
  vorher wurden die vollständigen vorderen Ranges jedes Mal erneut kopiert
  (quadratisch, ~87 GiB beim Laden von Hollow Knight, Lade-Frames > 10 s).
- Der Client-Hang-Timeout (2 s) braucht nach dem Decoder-Fix keine Erhöhung:
  in vier `just profile`-Läufen mit 2 s schlug der Detektor nie an.

Belegt durch einen beobachteten Fehler oder einen Gegentest ist jede Änderung
aus 1–3, mit zwei Ausnahmen:

- Gecachter Host-Type: belegt ist nur die Kopierbandbreite (0,3 vs.
  ~10 GB/s, `wsl-gpu/copy_bw.c`). Ein Gegentest mit `just profile` war wegen
  der Ausreißer im GPU-Restore (siehe „Stand“) nicht aussagekräftig.
- Die neu erzeugten Bounds-Checks im Flush/Invalidate-Decoder sind funktional
  nicht nötig (Gegentest grün); sie verhindern Lesen über das Host-Mapping
  hinaus bei `VK_WHOLE_SIZE` mit Offset.

`tests/gpu_mapped_ranges.c` deckt die Größenkonsistenz ab (mit dem alten
Encoder gerät der Stream außer Takt).

### 4. Nicht nötig für die Änderung selbst

- `UnityPlayer_s.debug`: nur für den gesteuerten Modus (ohne Symbole hängt
  Unity mit deterministischer Zeit beim Boot). Die Steam-Installation hat sie
  nicht; Quelle: Unitys Paket „Linux Mono Support“ zur Editor-Version
  (6000.0.61f1, `linux64_player_nondevelopment_mono`, Build-ID muss zum
  `UnityPlayer.so` passen).
- `kernel.perf_event_mlock_kb` hochsetzen: nur für `just profile`
  (`perf record -m 1024`).

## Stand

Testsuite unter WSL: 49/53 grün, alle drei Vulkan-gpu-Tests eingeschlossen.
Rot:

- `fd_dmabuf_freed`, `fd_dmabuf_rewind`: kein `/dev/udmabuf` im WSL-Kernel.
- `gpu_glx_submit_restore`, `gpu_glx_swapchain_resize_restore`: zink startet
  auf dzn gar nicht, auch ohne valo/gfxstream. Wahrscheinlich fehlt
  robustness2/`nullDescriptor`; außerdem setzt zink einen coherent
  Memory-Type voraus, den der Kopiermodus nicht anbietet.

`just profile 60 120` (Hollow Knight, gesteuert, GPU):

| | gesamt | Speicher | Warten auf GPU |
|---|---|---|---|
| Checkpoint | 978 ms | 933 ms | parallel |
| Restore | 1181 ms | 449 ms | 718 ms |

Nativ ohne GPU laut `snapshot-performance-ideas.md`: 708 / 500 ms. Beim
Restore ist der GPU-Restore in kumquat der kritische Pfad.

Offen:

- Gegentest unter nativem Linux für die Teile aus Abschnitt 2 (betreffen
  jeden Lauf). Der Kopiermodus selbst lässt sich nativ mit dem Feature-String
  testen; lavapipe als Host startet unter WSL gar nicht (auch ohne
  Kopiermodus), taugt hier also nicht als zweiter Treiber.
- **GPU-Snapshot/-Restore mit Ausreißern über 6 s** (`GPU_WAIT_TIMEOUT`),
  sonst ~1 s; in 25–75 % der `just profile`-Läufe. gdb-Stichproben eines
  langsamen Restores: der Restore wartet in `RendererImpl::load` →
  `waitForProcessCleanup`, das Aufräumen läuft in
  `VkDecoderSnapshot::vkDestroyInstance` → `VkReconstruction::removeHandles` →
  tief rekursivem `DependencyGraph::removeNodeAndDescendants` (> 5 s).
  CPU-gebunden im Snapshot-Abhängigkeitsgraphen; ob der Kopiermodus ihn
  vergrößert (dedizierte Allokationen), ist ungeprüft. Das WSL-`perf` hat kein
  libunwind/libdw; gdb-Stichproben gehen.
- kumquat versucht beim Restore exportierte device-local Blobs zu mmappen;
  unter dzn ist das `anon_inode:dxgresource` → `EACCES`, nur geloggt.

## Welcher Speicher betroffen ist

Mit `CREATE_GUEST_HANDLE` (kumquat meldet es) wird jede host-sichtbare
`vkAllocateMemory` im Guest-Treiber zu einem Guest-Blob
(`ResourceTracker.cpp`, Zweig `mCaps.params[kParamCreateGuestHandle]`):

1. kumquat legt einen memfd an (`create_guest_blob_backing` in
   `kumquat/server/src/kumquat_gpu/mod.rs`).
2. Der Spielprozess mmappt den memfd, das ist der `vkMapMemory`-Pointer.
3. kumquat macht per udmabuf einen dma-buf daraus und importiert ihn in den
   Host-Treiber (`VK_EXT_external_memory_dma_buf`).

Schritt 3 ist unter WSL unmöglich. Nicht betroffen sind:

| Speicher | Warum unkritisch |
|---|---|
| Command-Ring (ASG, Blob-ID 0) | reiner memfd ohne GPU |
| Device-local Speicher (Texturen, Render-Targets) | bleibt in kumquat/dzn, Snapshot über gfxstream |

Der Guest-Treiber kann den alten Kopiermodus von gfxstream nicht mehr:
`usingDirectMapping()` liefert hart `true`. Der gfxstream-Host kann ihn
noch (`!usingDirectMapping()` in `vk_decoder.cpp`), der Guest nicht.

## Sackgassen (alle getestet)

| Weg | Ergebnis | Beleg |
|---|---|---|
| dma-buf in dzn importieren | dzn hat kein `VK_EXT_external_memory_dma_buf` | `vulkaninfo` |
| udmabuf in dzn einbauen | löst das falsche Problem: Unter WSL gehört die GPU Windows, Pages müssen über dxgkrnl an den Host gemeldet werden. dzn spricht nur D3D12 | Architektur |
| `VK_EXT_external_memory_host` / `OpenExistingHeapFromAddress(1)` | dzn meldet die Extension nicht. `ID3D12Device3` und `ID3D12Device13` sind vorhanden, beide Aufrufe liefern **`E_NOTIMPL`** (memfd und anonym) | `wsl-gpu/heap_from_memfd.cpp` |
| exportierten dzn-fd im Spielprozess mmappen | `vkGetMemoryFdKHR` liefert ein D3D12-Shared-Handle als fd, ohne Pages dahinter | `dzn_GetMemoryFdKHR` in Mesa |
| CPU-sichtbaren D3D12-Heap mit `SHARED` teilen, im zweiten Prozess öffnen und mappen | jede Kombination aus CPU-sichtbar und `SHARED`/`SHARED_CROSS_ADAPTER` liefert bei `CreateHeap` `E_INVALIDARG`. Teilen geht nur ohne CPU-Zugriff | `wsl-gpu/shared_heap.cpp` |
| kumquat auf der Windows-Seite | kein geteilter Speicher und keine Unix-Sockets zwischen WSL2-VM und Windows | Architektur |

Die Regel „kein `SHARED` auf CPU-sichtbaren Heaps“ ist D3D12-Design und gilt
auch unter Windows. `libd3d12core.so` enthält dazu den Text: *„some shared CPU
accessible heaps can be accomplished through OpenExistingHeap“*. Genau dieser
Ausweg ist unter WSL nicht implementiert. dzn spiegelt die Regel in
`dzn_device.c` (`if (shared && ... HOST_VISIBLE_BIT) continue;`), siehe auch
[Mesa #11267](https://gitlab.freedesktop.org/mesa/mesa/-/work_items/11267).

### Der Kernel könnte es

dxgkrnl (GPL, im WSL-Kernel) hat `create_existing_sysmem` in
`drivers/hv/dxgkrnl/dxgvmbus.c`: Es pinnt User-Pages
(`pin_user_pages_fast(FOLL_LONGTERM)`) und meldet sie per GPADL oder PFN-Liste
an den Host. Es fehlt eine D3D12-Laufzeit-API, die das unter WSL aufruft. Die
Laufzeit per D3DKMT-ioctl zu umgehen scheitert an den privaten Daten des
Herstellertreibers pro Allocation (undokumentiert, pro Hersteller und
Treiberversion).

Einordnung der beiden Laufzeit-APIs:

- `ID3D12Device3::OpenExistingHeapFromAddress` ist laut Microsoft ein
  „special-purpose diagnostic heap“, der GPU-Faults und Device-Removal
  überlebt, und umschließt „the entire VirtualAlloc region“. Das ist an den
  Windows-Speichermanager gebunden; unter WSL gibt es keine
  VirtualAlloc-Regionen, was das `E_NOTIMPL` plausibel erklärt.
- `ID3D12Device13::OpenExistingHeapFromAddress1` nimmt eine explizite Größe
  und ist damit nicht an eine VirtualAlloc-Region gebunden. dzn nutzt sie für
  `VK_EXT_external_memory_host`. Keine Learn-Seite; ob Microsoft sie als
  allgemeinen Import-Pfad versteht, ist unklar. Unter WSL ebenfalls
  `E_NOTIMPL`.

Öffentliche Issues dazu unter WSL: keine gefunden (microsoft/WSL,
microsoft/DirectX-Specs, Websuche). Ein Issue müsste gezielt nach
`OpenExistingHeapFromAddress1` mit expliziter Range fragen
(`heap_from_memfd.cpp` als Reproduktion); ob Microsoft diesen Anwendungsfall
unterstützen will, ist offen. dzn hätte den Pfad dann schon.

## Der Weg mit Kopien

kumquat gibt dem Spielprozess für host-sichtbare Blobs einen memfd als
Schattenkopie und legt die echte Allocation selbst bei dzn an.

### Memory-Type: gecacht ist Pflicht

| dzn-Type (RX 6700 XT) | memfd → GPU | GPU → memfd |
|---|---|---|
| Vergleich: memfd ↔ RAM | ~10 GB/s | ~10 GB/s |
| Type 1, `VIS COH` (nicht gecacht) | 0,3 GB/s | 0,2 GB/s |
| Type 2, `VIS COH CACHED` | ~11 GB/s | ~10,5 GB/s |

Die iGPU zeigt dasselbe Muster (`wsl-gpu/copy_bw.c`). Die Werte gelten für die
Mappings von dzn unter WSL. kumquat muss host-sichtbaren Speicher immer aus
dem gecachten Type anlegen, unabhängig davon, welchen Type der Guest dem
Spiel anzeigt.

### Was pro Frame passieren muss

| Wann | Was |
|---|---|
| vor jedem `vkQueueSubmit` | Schattenkopie → dzn, nur Dirty-Pages, ohne Bereiche mit noch laufenden GPU-Schreibzugriffen |
| bevor der Spielprozess eine Fertigmeldung sieht (`vkWaitForFences`, `vkGetFenceStatus`, `vkWaitSemaphores`, `vkQueueWaitIdle`, `vkDeviceWaitIdle`, Event-Status) | dzn → Schattenkopie, nur Bereiche, die die GPU geschrieben haben könnte |
| `vkFlushMappedMemoryRanges` / `vkInvalidateMappedMemoryRanges` | nur für nicht-coherent Types, dort genau (die Anwendung nennt die Bereiche) |

Flush und Invalidate allein reichen nicht. Die Spec verlangt mindestens einen
`HOST_VISIBLE | HOST_COHERENT`-Type, und bei coherent memory ruft die
Anwendung beides nicht auf. Unity schreibt in coherent memory.

**Dirty-Tracking auf der CPU-Seite:** async uffd-WP
(`UFFD_FEATURE_WP_ASYNC | UFFD_FEATURE_WP_HUGETLBFS_SHMEM`, unprivilegiert
mit `UFFD_USER_MODE_ONLY`) plus `PAGEMAP_SCAN` mit
`PM_SCAN_WP_MATCHING | PM_SCAN_CHECK_WPASYNC`, das in einem Aufruf liest und
neu schützt. Ein Handler-Thread ist nicht nötig. Offen: das Zusammenspiel mit
valos eigenem Soft-Dirty/`clear_refs` im selben Prozess.

**GPU-Seite:** Page-Tracking ist unmöglich, die GPU schreibt am MMU vorbei.
kumquat ist aber der Decoder und sieht alle Befehle: Kopierziele
(`vkCmdCopy*ToBuffer`, `vkCmdFill/UpdateBuffer`,
`vkCmdCopyQueryPoolResults`) sind genau bekannt. Shader-Schreibzugriffe auf
Storage-Buffer und Storage-Images nur konservativ über die Bindings,
eingrenzbar per SPIR-V `NonWritable`. Fehler hier zeigen sich als subtile
Grafikfehler und nicht als Absturz.

**Konflikte innerhalb einer Page:** Schreiben CPU und GPU in dieselbe Page
(verschiedene Bereiche), darf die Kopie CPU→GPU keine ganze Page übertragen,
sonst überschreibt sie GPU-Daten mit veralteten Daten aus der Schattenkopie.

### Snapshot und Restore

- **Snapshot:** An der Frame-Grenze bei ruhender GPU ist die Schattenkopie
  vollständig. valo sichert den memfd wie bisher. Die dzn-Kopie muss nicht in
  den gfxstream-Snapshot. Prüfen, dass keine Dirty-Pages oder offenen
  GPU-Bereiche übrig sind, deckt Lücken im Tracking auf.
- **Restore:** valo stellt den memfd wieder her, gfxstream legt die
  Allocations neu an, dann **einmal komplett Schattenkopie → dzn** (ersetzt
  das dma-buf-Reattach in kumquat) und das Tracking zurücksetzen. Bei Hollow
  Knight sind das 70–90 MB, also ~8–10 ms pro Restore.
- Die Snapshot-Seite wird eher einfacher als heute: kein udmabuf-Dup, kein
  Reattach, der memfd ist die einzige Quelle der Wahrheit.

### Variante: den Kopierpfad von gfxstream im Stream wiederbeleben

Umgesetzt, siehe „Was nötig war“. Die folgende Analyse ist der Stand davor.

Statt kumquat-eigener Schattenkopie mit uffd-WP lässt sich der alte Modus
von gfxstream ohne Direct-Mapping nutzen: Der Guest schickt bei
`vkFlushMappedMemoryRanges` die Bytes im Command-Stream mit, der Host
kopiert sie in seinen Mapping-Pointer und schickt sie bei
`vkInvalidateMappedMemoryRanges` zurück. Vorteil: Die Anwendung nennt die
Bereiche selbst. Es braucht weder Dirty-Tracking noch eine Befehlsanalyse
für die GPU-Seite.

Was vorhanden ist:

- Host: Alles hängt an `usingDirectMapping()`, also `GlDirectMem ||
  VirtioGpuNext`. `VirtioGpuNext` wird in `virtio_gpu_gfxstream_renderer.cpp`
  fest gesetzt, lässt sich aber danach per Feature-String überschreiben
  (`VirtioGpuNext:disabled`). kumquat reicht `--renderer-features` durch.
  Flush/Invalidate-Kopien in `vk_decoder.cpp` und das Aufrunden der
  Allocation auf Seitengröße existieren. `HOST_COHERENT` wird dann aus allen
  Guest-Types entfernt (`vk_emulated_physical_device_memory.cpp`).
- Guest: Der Encoder-Code für den Modus ist vorhanden (`VkEncoder.cpp`,
  `!usingDirectMapping()`-Zweige).

Was fehlt bzw. bricht:

1. Guest: `usingDirectMapping()` liefert hart `true`. Es braucht einen
   Laufzeitwert und in `allocateCoherentMemory` einen Zweig, der statt
   Guest-Handle-Blob (udmabuf) oder host3d-Blob (vom Host exportierbarer
   Speicher) nur einen seitenausgerichteten Schattenpuffer anlegt.
   Signalisieren über ein neues `vulkanCapset`-Feld, nicht per
   Umgebungsvariable: Laufen Guest und Host in verschiedenen Modi, gerät der
   Stream sofort aus dem Takt.
2. **Der Host mappt unter kumquat nicht.** `needToMap` ist an
   `!ExternalBlob` geknüpft, kumquat setzt `set_use_external_blob(true)`. Ohne
   Host-Pointer endet der erste Flush mit Daten in
   `GFXSTREAM_FATAL("Unexpected")`. `needToMap` muss
   `|| !usingDirectMapping()` bekommen.
3. **Host-Allocation aus dem gecachten Type**, sonst kopiert der Decoder mit
   0,3 GB/s (siehe oben).
4. **Restore:** Die Host-Kopie füllt niemand neu (kein dma-buf zum
   Reattachen). Erwies sich als unnötig: Der gfxstream-Snapshot sichert und
   lädt Buffer-Inhalte selbst (`loadBufferContent`).
5. **Kein coherent memory.** Die Spec verlangt mindestens einen
   `HOST_VISIBLE | HOST_COHERENT`-Type. **Unity (Hollow Knight) kommt ohne
   aus und flusht korrekt** (Lauf E unten, Grafik visuell geprüft).
   Anwendungen, die coherent voraussetzen, brechen: vkcube findet keinen
   passenden Type, nimmt einen nicht mappbaren und stürzt in `vkMapMemory`
   ab. gfxstream hat außerdem `VulkanDisableCoherentMemoryAndEmulate`, nicht
   untersucht.

Für Unity ist das damit die bevorzugte Variante: kein Dirty-Tracking, keine
Befehlsanalyse für GPU-Schreibzugriffe, die Richtung GPU→CPU genau über
Invalidate, und die Synchronisation ist billiger (0,02 statt 0,15 ms pro
Frame).

Mischform, falls Unity coherent braucht: coherent beibehalten und den Guest
beim Submit Flushes aus uffd-Dirty-Pages erzeugen lassen. Für GPU→CPU fehlt
dann wieder das Wissen, was die GPU geschrieben hat.

## Messungen

### uffd-WP auf memfd (`wsl-gpu/uffd_wp.c`, 256 MB)

| | Kosten |
|---|---|
| erster Schreibzugriff auf geschützte Page | ~0,6 µs (ohne Tracking 20 ns) |
| `PAGEMAP_SCAN` + neu schützen, 1 / 16 / 256 MB dirty | 0,13 / 0,19 / 0,73 ms |
| Scan ohne Änderungen | 0,035 ms |
| erstes Schützen von 256 MB | 4 ms |

### Hollow Knight mit Simulations-Layer (`wsl-gpu/shadow_layer.c`)

Die Layer gibt bei `vkMapMemory` eine memfd-Schattenkopie mit async uffd-WP
zurück und kopiert vor jedem `vkQueueSubmit` die Dirty-Pages. Die Richtung
GPU→CPU fehlt, die Werte sind also eine Untergrenze. Läufe zu je 45 s ohne
VSync, Titelbildschirm, Spielstand laden, laufen (manuell gespielt, daher
nicht exakt vergleichbar). Mittel der Gameplay-Phase:

| Lauf | Frame-Zeit | Dirty/Frame | Sync/Frame |
|---|---|---|---|
| A durchreichen | ~8,5 ms | – | – |
| B nur gecachter Type | ~9,5 ms | – | – |
| C Schattenkopie + gecacht | ~9,5 ms | ~60 Pages (0,25 MB) | 0,15 ms (Scan 0,12) |
| D Schattenkopie, Unitys Type | ~10,5 ms | ~60 Pages | 0,75–0,9 ms |
| E nicht-coherent, Abgleich nur bei Flush/Invalidate, gecacht | ~9,3 ms | ~3 Flushes, ~0,25 MB, 0 Invalidates | 0,02 ms |

Lauf E (`SHADOW_MODE=2 SHADOW_NONCOHERENT=1`) blendet `HOST_COHERENT` aus
den Memory-Properties aus und überträgt nur geflushte Bereiche zur GPU.
Vergessene Flushes wären als Grafikfehler sichtbar, es gab keine. Die
geflushte Menge entspricht fast genau der Dirty-Menge aus Lauf C, Unity
flusht also ziemlich genau, was es schreibt. Hollow Knight liest nie über
Invalidate zurück.

Weitere Werte: 1–2 Submits pro Frame, 34–42 Mappings mit 70–88 MB, beim
Laden bis ~0,5 MB pro Frame. Die Unterschiede bei der Frame-Zeit zwischen den
Läufen liegen im Rauschen. Aussagekräftig ist die direkt gemessene
Sync-Zeit. Unity fragt `vkQueueSubmit2`/`vkMapMemory2` per ProcAddr ab, die
Layer hookt sie nicht. Bei Vulkan 1.1 (von Unity angefordert) ist das
vermutlich nur eine Abfrage, sollte aber bei echter Umsetzung geprüft werden.

## Fallstricke beim Nachbauen

- `D3D12CreateDevice(nullptr, ...)` liefert unter WSL
  `DXGI_ERROR_UNSUPPORTED`. Den Adapter über dxcore aufzählen
  (`DXCoreCreateAdapterFactory`) und übergeben. Die Bibliotheken liegen in
  `/usr/lib/wsl/lib`.
- DirectX-Headers unter Linux: `<wsl/winadapter.h>` vor `<directx/d3d12.h>`
  einbinden, mit `-Iinclude -Iinclude/wsl/stubs` und `src/dxguids.cpp`
  bauen.
- dzn ist nicht standardmäßig installiert (Arch: `vulkan-dzn`). Gezielt
  wählen mit `VK_ICD_FILENAMES=.../dzn_icd.json`.

## Weitere Ideen

Zusätzlich zu den offenen Punkten unter „Stand“:

1. Coherent memory im Kopiermodus emulieren (Schattenkopie mit uffd-WP,
   siehe „Der Weg mit Kopien“), für Anwendungen, die einen
   `HOST_COHERENT`-Type voraussetzen (z. B. vkcube). Für Unity nicht nötig,
   und nicht „nur bei Bedarf“ einschaltbar: der Guest muss sich vorher
   festlegen, welche Types er als coherent meldet.
2. Ein 3D-Unity-Spiel messen. Hollow Knight ist leicht.
3. Issue bei microsoft/WSL zu `OpenExistingHeapFromAddress1` (explizite
   Range) → `E_NOTIMPL`; damit ginge es ohne Kopien (dzn hat den Pfad schon).
   Die `Device3`-Variante ist laut Doku ein Diagnose-Heap und an
   VirtualAlloc-Regionen gebunden, also kein sinnvolles Ziel.
