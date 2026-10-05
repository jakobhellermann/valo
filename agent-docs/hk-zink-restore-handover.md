<!-- // TODO(ai-review): generated, not yet audited -->

# Hollow Knight über zink/gfxstream: Restore-Kette, Stand und offene Hänger

Stand: 12. Oktober 2026. Handover der Session, die den P1-Repro
(`gpu_glx_swapchain_resize_restore`) grün gemacht und Hollow Knight (Unity
GL → zink → gfxstream) über mehrere Restores gebracht hat. Drei Repos:
valo, `~/dev/contrib/gpu/gfxstream` (WIP-Stack), `~/dev/contrib/gpu/mesa`
(WIP-Stack, nur Codegen).

## Erledigt (committed)

| Repo | Change | Inhalt | Retest |
|---|---|---|---|
| mesa | `ow` | Codegen: `vkCmdBeginRendering(KHR)` registriert Attachment-Views als Recording-Dependencies | Codegen in Scratch laufen lassen, Output vs. gfxstream diffen |
| gfxstream | `rt` | Regenerierte Bodies davon (nur gespliced, kein Full-Regen: WIP hat Hand-Edits in generierten Dateien) | `just test gpu_glx_swapchain_resize_restore` |
| gfxstream | `vs` | Laute Diagnose für verworfene Recordings: Grund im Skip-Log (`was destroyed` / `never in the replay graph` / `handle reused by a newer object`), Dependency-Timestamps gegen Handle-Recycling, Load markiert verworfene Command Buffer, Submit/ExecuteCommands ohne Re-Record → einmal `GFXSTREAM_ERROR ... it executes empty`. **Snapshot-Format um zwei Buffer erweitert** | Repro + volle Suite |
| valo | `rro` | `dlsym("vkGetDeviceProcAddr")` aus dem Loader (zink tut das) bekommt eine Treiber-Tabelle: nur `vkAcquireNextImageKHR` (Restore-Invalidation → OUT_OF_DATE, ohne skip_draw-Fake) und `vkDestroySwapchainKHR` (armiert die Present-Kicks). Present bleibt ungehookt → kein doppelter Frame-Boundary (GL-Swap bleibt die Grenze; zink presentet z.T. vom `zfq0`-Thread) | Repro 15/15, volle Suite |
| valo | `mx` | Repro-Test grün + Snapshot | — |

Mechanik-Kurzfassung:
- **gfxstream-Abort beim Restore**: Recordings, deren Objekte vor dem Save
  zerstört wurden (Swapchain-Resize → alte Images/Views weg), wurden
  replayed → `GFXSTREAM_FATAL` im Unbox. Save verwirft solche Command Buffer
  bereits (`invalidRecordings` in `vk_reconstruction.cpp`), kannte aber nur
  die per Hand im Codegen registrierten Dependencies.
- **Guest-Hänger nach Restore** (beide in `restore-hangs.md` Klasse 3 bzw.
  Acquire-Stall): zink ging an valos Vulkan-Hooks vorbei, also weder Kick
  beim Destroy-Join der alten Swapchain noch OUT_OF_DATE beim ersten Acquire.

## Später in der Session (committed)

- **valo, RELRO-Restore**: `skip()` in `checkpoint/writer.rs` übersprang alle
  nicht-schreibbaren Datei-Mappings. RELRO (`.got`, bei BIND_NOW auch
  `.got.plt`) schreibt der Loader aber relozierte Adressen und macht es erst
  dann read-only. Solange die Lib beim Restore gemappt bleibt, fällt das
  nicht auf; wurde die Lib erst nach einem früheren Slot geladen (HK:
  Steam-Plugins) und wird von dort aus ein späterer Slot geladen, legt der
  Restore das Segment aus der Datei an → unrelozierte GOT → SIGSEGV
  (`SteamAPI_RunCallbacks` → `0x13f06`). Jetzt werden lesbare private
  Datei-Mappings nicht mehr übersprungen; der Per-Page-Pfad speichert nur
  COW-Seiten. Kosten im GPU-Repro: +3 % Savestate-Größe, ca. +4 ms pro
  Checkpoint/Restore. Nur echte Dateipfade (`[vvar_vclock]` liefert sonst
  EFAULT). libTAS hat dieselbe Skip-Regel (`MemArea.cpp` `isSkipped`).
  Test: `dlopen_relro_restore` (rot ohne Fix: `rip=0x0`).
- **gfxstream, Mapping-Lookup**: Lücke in `yy`. Ein einmal restaurierter
  Ring wird beim nächsten Save als `external_memory_mapping` geschrieben,
  dieser Zweig suchte nur per `(ctx, blob 0)` → „failed to find mapping
  info“ (Slot nach einem Restore gespeichert, dann geladen). Jetzt erst per
  Resource-ID. Kein automatischer Test; HK-Hotkey-Folge (Slot 1 speichern,
  restoren, Slot 2 speichern, Slot 2 restoren) hat es bestätigt.
- **Xwayland-gdb** (`scratchpad/xw/xw.gdb`-Muster: Breakpoints auf
  `connection.c:909` = inlined `IgnoreClient`, `AttendClient`,
  `dri3_request.c:173 if client->ignoreCount != 0`; braucht
  `set debuginfod enabled on`) lief über zwei HK-Sessions ohne einen Treffer
  — der DRI3-Hänger trat dabei nicht auf.

## gfxstream: Dependencies aus dem Unbox (committed)

Statt pro `vkCmd*` Codegen-Hooks: `ScopedUnboxedHandleRecorder`
(`host/vulkan/vulkan_boxed_handles.{h,cpp}`) sammelt thread-lokal jedes
nicht-dispatchable Handle, das `unbox_VkType` auflöst. Der Sub-Decoder
(`vk_sub_decoder.cpp`, alle 149 Opcodes) öffnet pro Paket einen Recorder
und setzt die Menge per `setApiDependencies` als Dependencies des Pakets.
Deckt per Konstruktion genau die Handles ab, die der Replay unboxt (inkl.
pNext). Fixt den HK-Crash `Failed to unbox VkImage 0x7000300000fd0`
(`vkCmdPipelineBarrier2` → `VkImageMemoryBarrier2`, hatte keinen Hook).

- Validiert: HK-Lauf des Users (1 Checkpoint, 8 Restores, kumquat
  stabil), Repro ×10, volle Suite. Retest-Set: Repro, Suite,
  HK `--checkpoint-frame 30 --restore-frame 50`.
- Nebenwirkung: Descriptor-Sets (`0xd…`) sind nicht im Replay-Graph (werden
  beim Load lazy re-alloziert) → Command Buffer, die sie binden, werden mit
  `never in the replay graph` verworfen. Im HK-Lauf harmlos (kein
  `executes empty`). Falls doch: lazy re-allozierte Sets beim Save als
  vorhanden zählen, nicht das Recording abschalten.

## Offen 1: DRI3 Open nach Restore wird von Xwayland nie beantwortet

Symptom (zwei Läufe, HK mit `--use-gpu`, Checkpoint ~851, Restore 1200):
- ohne xtrace: Main hängt in zink `update_caps` →
  `vkGetPhysicalDeviceSurfaceCapabilitiesKHR` → `xcb_wait_for_reply`.
  `out.request_written` weit vorne, `in.request_read` steht; alle
  Events im Socket tragen dieselbe Sequenz → der Server hat genau einen
  Request *begonnen* und nie beendet, danach nichts mehr verarbeitet.
- mit xtrace: der hängende Request ist `DRI3 Open drawable=root provider=0`
  (WSI-Recreate nach OUT_OF_DATE). Alle 10 DRI3 Opens vor dem Restore wurden
  beantwortet, alles bis zum Request davor auch. Danach X-IO-Error → Xlib
  `exit()` → Mono-atexit hängt in `semtimedop`.

Analyse (Xwayland 24.1.14, niri ohne `wl_drm`, nur linux-dmabuf mit
`renderD128`): `xwl_dri3_open_client` nimmt den synchronen
Render-Node-Pfad (`fd_render_node = TRUE`). `dri3/dri3_request.c:173`
schickt die Antwort aber nur `if (client->ignoreCount == 0)` und verlässt
sich sonst auf den asynchronen `wl_drm`-Auth-Pfad, der hier nie läuft →
Antwort verloren, Client steckt. Also war der Game-Client zum Zeitpunkt
ignoriert. **Wer ihn ignoriert, ist unbekannt**: Das Game sendet kein
`SyncAwait`/`GrabServer`, GLX nur Setup-Queries; die übrigen
`IgnoreClient`-Aufrufer in Xwayland passen nicht offensichtlich.

Nicht deterministisch (ein User-Lauf schaffte 8 Restores). Durch `rro`
viel häufiger erreicht, weil zink jetzt nach jedem Restore die Swapchain
neu baut.

Nächster Schritt: den Hänger live fangen und Xwayland inspizieren
(`ignoreCount` des Game-`ClientRec`, besser Watchpoint auf das Feld, um
den Inkrementierer zu sehen). **gdb an der Session-Xwayland friert alle
X-Fenster ein → vorher den User fragen.** Alternative: gepatchtes
Xwayland, das `IgnoreClient`/`AttendClient` mit Backtrace loggt, als
eigene Instanz nur für das Game.

Nebenbefund im Trace: nach Restore zerstört die rewoundene Swapchain
Fences/Pixmaps/Regions, die der Server in der verworfenen Timeline schon
freigegeben hat (`BadPixmap`/`BadFence`/`BadRegion`, Present einer
unbekannten Pixmap) — asynchron, harmlos, `restore-hangs.md` Klasse 2.
1068 `DestroyFence` auf der Game-Connection: viel Swapchain-Churn.

## Offen 2: Watchdog-SIGABRT deadlockt bei Mono-Games

`crates/client/src/main.rs` schickt dem hängenden Target SIGABRT für einen
Core. Monos eigener SIGABRT-Handler forkt ein Kind für einen
Native-Backtrace und liest dessen Ausgabe über eine Pipe; das Kind ruft
`exit(1)` → mesas `util_queue`-atexit joint Queue-Threads, die im
geforkten Kind nicht existieren → hängt ewig. Parent hängt im `read` der
Pipe, Client in `child.wait()`: kein Core, Session endet nie, verwaiste
`hollow_knight.x86_64`-Prozesse unter `systemd --user` bleiben zurück.
Vorschlag (nicht umgesetzt, User-Entscheidung offen): nach SIGABRT
begrenzt warten, dann laut melden und SIGKILL; oder die Lib lässt das Game
den SIGABRT-Handler nicht ersetzen (`sigaction`-Hook loggt ohnehin
„oldact bookkeeping not yet implemented“).

## Werkzeuge und Kniffe aus der Session

- **xtrace**: `/usr/bin/xtrace` ist glibcs Funktions-Tracer, nicht der
  X11-Tracer. Den echten über nix: `nix build nixpkgs#xtrace`, dann
  `xtrace -d :1 -D :9 -n -k -o <log>` und das Game mit `DISPLAY=:9`.
  `just repro` nicht benutzen, wenn Savestates/Run-Homes erhalten bleiben
  sollen (ruft `just clean`). Connection 000/001 im Trace sind
  valo-client Overlay/Hotkeys, das Game ist die große (hier 004).
- **X-Socket peeken ohne zu konsumieren**: gdb auf einen idle Thread
  (nicht Main), `mmap`-Buffer, `recv(fd, buf, n, MSG_PEEK|MSG_DONTWAIT)`,
  `dump binary memory`, Pakete parsen (Typ 0 Error, 1 Reply, 35 Generic,
  Sequenz in Bytes 2–3). Alle Events mit gleicher Sequenz = Server
  verarbeitet nichts mehr.
- **Signal + Sender aus dem Signal-Frame**: im Frame `<signal handler
  called>` liegt `siginfo_t` bei `$sp + 304` (Kernel-`ucontext`, nicht
  glibcs `sizeof(ucontext_t)`): `p ((siginfo_t*)((char*)$sp+304))->si_signo`
  und `->_sifields._kill.si_pid`.
- **Run-Home zuordnen**: „removed savestate data /tmp/valo-N“ beim Start
  ist der Sweep eines *alten* Homes, nicht das aktuelle. Aktuelles Home
  über `ipc connected on /tmp/valo-N/ipc.sock` oder die kumquat-pid im
  `valo.log`; Core-Dumps über `coredumpctl` nach pid zuordnen (der User
  lässt parallel eigene HK-Läufe laufen).
- Xlib/xcb-Zähler eines hängenden Prozesses:
  `XGetXCBConnection(dpy)` pro `display 0x…` aus dem Log, dann
  `(struct _XDisplay*)`-Felder `request`/`last_request_read` und
  `((xcb_connection_t*)c)->out.request_written` / `in.request_read`.
