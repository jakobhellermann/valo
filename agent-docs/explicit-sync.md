<!-- // TODO(ai-review): generated, not yet audited -->

# Der 60fps-Frame-Rate-Gate auf dem GPU-Pfad: Explicit Sync fehlt im gfxstream-Gast

Stand: Oktober 2026. Gesammelt beim Aufklären von „warum läuft Hollow
Knight im GPU-Modus nicht schneller als der Monitor — und warum
fast-forward trotz `VALO_DETERMINISTIC_TIME=1` nicht greift". Alle Zahlen
aus valo-Runs auf dem Desktop (niri, XWayland) und dem Laptop
(sipgatejj, ebenfalls niri); Quellenangaben sind Commit-Stände in
`~/dev/contrib/gpu/{mesa,gfxstream}`.

## Endergebnis in einem Satz

Der Gast-Treiber `gfxstream_vk` hat keine Explicit-Sync-Fähigkeit
(Timeline-Semaphores mit OPAQUE_FD-Export); Mesa stock `wsi_common_x11`
fällt dadurch auf Implicit Sync zurück, und deren Buffer-Release ist an
die Compositor-Frame-Rate des Outputs gekoppelt, auf dem das Fenster
liegt — **Framerate = Monitor-Rate, punktgenau, auf jeder Maschine**.

## Architektur-Kurzreferenz (die zwei Beine)

Siehe README-Mermaid „GPU session architecture". Wichtig fürs Verständnis
des Gates:

- **Render-Bein**: Unity → gfxstream_vk ICD (Guest Mesa) → virtio-gpu
  über `gpu.sock` → kumquat (embeds gfxstream-Host-Translator) → echtes
  Host-Vulkan auf der GPU. kumquat führt nichts selbst aus und ist
  **headless** — kein Display.
- **Display/Sync-Bein**: Swapchain/Acquire/Present = **stock Mesa
  `wsi_common_x11`, in-process im Spiel** (`gfxstream_vk_wsi.cpp` macht
  nur `wsi_device_init()` — keine eigene WSI!). Das Spielfenster ist ein
  normales XWayland-Fenster (Launcher setzt `WAYLAND_DISPLAY=""`,
  DISPLAY geht durch). kumquat und der gfxstream-Host-Compositor sind an
  der Takt-Kette **nicht** beteiligt.

## Die Messung, die alles entscheidet

Der Gate sitzt im Acquire: `vkAcquireNextImageKHR` blockt ~14,5–16,4 ms
pro Frame (≈ eine Compositor-Frame-Periode); Present und der komplette
valo-Client-IPC-Roundtrip zusammen ~90 µs. Bei Swaps von der
Image-Pipeline entleerten Phasen (Load, Swapchain-Recreate) feuern die
Boundaries mit 200–660 fps — die Render-Pipeline selbst ist *nicht* die
Decke.

Drei-Punkte-Bestätigung des Modells (gleicher gfxstream+kumquat-Stack):

| Setup                     | Output-Refresh | real gemessen |
| ------------------------- | -------------- | -------------- |
| Desktop HDMI-A-1 (primary) | 59,96 Hz      | 60 fps         |
| Desktop DP-1               | 143,80 Hz     | ~144 fps       |
| Laptop eDP (sipgatejj)     | 120,001 Hz     | 120–122 fps    |

Natives Vulkan auf demselben XWayland: `vkcube --present_mode 1`
(mailbox) läuft mit **12000 fps** — der Server *kann* schnell releasen,
wenn der Treiber Explicit Sync kann. `vkcube` durch den gfxstream-Stack:
exakt 60 fps (Isolation: Kette, nicht Spiel).

## Beweiskette: was eliminiert wurde

| Verdächtiger          | Test                                             | Ergebnis |
| --------------------- | ------------------------------------------------ | -------- |
| Present-Mode          | `immediate` erzwungen (Rewrite + MESA_VK_WSI_PRESENT_MODE) | 60 fps |
| valo-Client-IPC       | `present+ipc`-Dauer gemessen; VALO_IPC_MODE=async | ~90 µs, irrelevant |
| Sleeps/Frame-Pacing  | sleepstats + fastpoll-Gate                        | Main pollt 0 ms — kein Sleep-Pace |
| gfxstream-Host-Vsync  | `GFXSTREAM_VSYNC_HZ=1000` (Patch, Log bestätigt) | 60 fps — irrelevant |
| XWayland per se       | natives vkcube                                   | 12k fps — unschuldig |
| llvmpipe/no-GPU-Pfad  | —                                                | `LIBGL_ALWAYS_SOFTWARE=1` (Savestate-Kompatibilität): ~36 fps CPU-Decke, kein Pacing-Problem |

## Quellenebene (alles in ~/dev/contrib/gpu verifiziert)

Mesa `wsi_common_x11.c` wählt Explicit Sync nur bei:

```c
.explicit_sync = wsi_conn->has_dri3_explicit_sync &&         // X-Seite ok
                 (present_caps & XCB_PRESENT_CAPABILITY_SYNCOBJ) && // ok
                 wsi_device_supports_explicit_sync(wsi_device);
                 // = !sw && KHR_timeline_semaphore &&
                 //   timeline_semaphore_export_handle_types & OPAQUE_FD_BIT
```

Warum gfxstream_vk das nicht erfüllt:

- `gfxstream_vk_device.cpp`: einziger Sync-Typ ist `vk_sync_dummy_type`
  (Feature-Kommentar: „Must use dummy_sync for correct sync object path
  in WSI operations"). Dummy: binär, keine Timeline-, Shareable- oder
  FD-Features; alle Waits sind No-ops.
- `ResourceTracker.cpp::on_vkGetPhysicalDeviceExternalSemaphoreProperties`:
  Timeline+FD wird **bewusst genullt** (Bug b/304373623,
  dEQP-VK.api.external.semaphore.sync_fd#info_timeline). Binary SYNC_FD:
  erlaubt.
- `on_vkCreateSemaphore` / `on_vkGetSemaphoreFdKHR`: echte Substanz
  existiert nur **binär** — `VirtioGpuNativeSync` erzeugt echte
  Kernel-Sync-Files (kernel-global, gleicher Kernel!). OPAQUE_FD-Export
  ist ein **Fake**: Guest-Anon-File, in das die Host-FD-Nummer
  geschrieben wird — nur gegen andere gfxstream-Endpunkte
  interpretierbar.
- Damit fehlt genau: drm-Syncobjekte (timeline-fähig) über die Pipe,
  ein `vk_sync_type` darüber, echte Wait/Signal-Verdrahtung im
  Submit-Pfad, und die Capability-Aussage.

## Fix-Optionen (Status: **Entscheidung offen**)

1. **Output-Rate als Decke akzeptieren** — 144 fps auf dem 144Hz-Output,
   null Arbeit. Framerate folgt dem Monitor; dokumentiertes Verhalten.
2. **gamescope nested** — `gamescope --immediate-flips` zwischen Spiel und
   niri; historischer Workaround (Steam Deck), testbar ohne gfxstream-Arbeit.
3. **valo-seitige Idle-Synthese** — `xcb_present_pixmap` hooken
   (Pixmap+Serial mitbekommen), Idle-Events sofort in die Present-Special-Queue
   injizieren (Technik wie `valo_xcb_kick_special_event`), echte Server-Idles
   anhand der Serials verschlucken, `xshmfence_await` sofort returnen.
   Artefakte (Compositor zeigt veraltete Frames), Session-größen Aufwand.
4. **Explicit-Sync-Feature im magma-Stack** (der saubere Weg, venus-skaliert):
   a. Host/kumquat: drm-Syncobjekte (timeline-fähig) über die Pipe
      exportieren — heute exportiert VirtioGpuNativeSync nur Sync-Files.
      Da Gast+Host denselben Kernel teilen, wäre ein durchgereichter
      Syncobj-FD kernel-global.
   b. Guest-ICD: `vk_sync_type` über diese Syncobjekte; Vorlagen:
      `src/vulkan/util/vk_drm_syncobj.c` (radv — deshalb sagt native YES)
      und venus `src/virtio/vulkan/vn_sync.*` (dasselbe Problem, Guest
      ohne DRM-Knoten, FDs über die Pipe).
   c. ResourceTracker: Timeline-Nullung durch echte Abfrage ersetzen;
      b/304373623-Fall muss weiterhin korrekt bleiben.
      Testfall zusätzlich: venus-Commit `wkklss` („ignore imported
      SYNC_FD payload for signal semaphore inspection") — derselbe
      Fehlerklassen-Typ, den die X11-WSI-Explicit-Sync-Pfade erzeugen.

## Messregeln (wichtig für Nacharbeit)

- Unter `VALO_DETERMINISTIC_TIME=1` liest **jeder** In-Process-FPS-Counter
  (mangohud!) die virtuelle Uhr → zeigt per Definition `VALO_FRAMERATE`,
  egal wie schnell es real läuft. mangohud ist dann als FPS-Anzeige tot.
- Reale Rate: valo-log `fps:`-Zeile (frame.rs `monotonic_ms()` nutzt
  absichtlich `real_ns()`), oder mangohud ohne det-time.
- Die Acquire/Present-Messung dieses Dokuments war eine temporäre
  Instrumentierung in `hooks/vulkan.rs` (Dauer um
  `vkAcquireNextImageKHR`/`vkQueuePresentKHR` via `time::real_ns()`,
  1/s-Dump) — mit dem Debug-Kram reverted; bei Bedarf wieder so einbauen.

## Verwandte Funde der Session

- **`MESA_VK_WSI_HEADLESS_SWAPCHAIN=true` + gfxstream = Crash beim Boot**
  (GPUVM fault, DEVICE_LOST im ersten Submit): headless-WSI baut die
  Swapchain-Images über einen anderen Pfad, als der ICD erwartet. Kein
  valo-Bug; für gfxstream-Gäste vom Tisch.
- **Determinismus-Begriff korrigiert**: fast-forward=non-fast-forward heißt
  **Skip-Invarianz** (Rendern darf den Spielzustand nicht beeinflussen),
  nicht „deterministisches Skip-Muster". Skip/Spul-Rate ist damit frei
  wählbar; pro Spiel prüft das Trace-Orakel (RNG-State etc.) die Invarianz.
- **sleepstats-Avg-Falle**: `total_ns` friert bei nur-0ms-Requests ein
  (kumulative Summe der *angeforderten* Timeouts); avg = frozen
  total/growing calls ist dann bedeutungslos — die 12-ms-Poll-Diagnose
  dieser Session war ein Artefakt davon. Bei Diagnose: Zähler für
  Calls mit ns>0 separat führen.
- **Det-time allein fast-forwarded nicht**: die Uhr ist frame-locked,
  Boundaries feuern im Tempo des synchronsten Blockers. Bei HK ist das
  Acquire; sleep-paced Spiele würden von der (reverteten) fast_poll-Gate
  profitieren (wieder aufbauen als Teil der Run-Mode-Arbeit).
