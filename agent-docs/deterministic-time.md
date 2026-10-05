<!-- // TODO(ai-review): generated, not yet audited -->

# Deterministic Time: Zustand und Handover

Stand: Oktober 2026. Status nach der Session, die Fast-Forward (Skip-Pipeline,
Config-getrieben) fertiggestellt und die 60fps-Diagnose abgeschlossen hat
(siehe `agent-docs/explicit-sync.md`).

## Was existiert (`hooks/time.rs`)

- **Frame-locked virtuelle Uhr**: `advance_frame()` (aufgerufen von
  `frame.rs::frame_boundary`) schiebt die virtuelle Zeit pro Frame-Boundary
  um exakt eine Frame-Dauer weiter (`VALO_FRAMERATE`, Default 60, mit
  Nanosekunden-Restbruch in `FRAC`). **Boundary-Anzahl = Spielzeit** — das ist
  die Determinismus-Invariante des Ganzen.
- **Gehookte Quellen** (virtuell): `clock_gettime(CLOCK_MONOTONIC)`,
  `CLOCK_REALTIME`, `CLOCK_MONOTONIC_RAW` (erst nach der ersten Boundary,
  siehe unten), `gettimeofday`, `time`.
- **Epochen hartkodiert** (libTAS-Parität): MONOTONIC = 0, REALTIME =
  2023-11-14T22:13:20Z. Zeit ist damit replizierbar, nie von einer echten
  Uhr abgelesen.
- **`RAW_ARMED`-Gate**: RAW wird erst ab der ersten Boundary virtuell
  geserviert — Boot-Code (Engine, In-Process-Runtimes) wartet auf
  RAW-basierte Timeouts, und die virtuelle Uhr steht vor der ersten Boundary
  still: virtuelles RAW ab Prozessstart deadlockt den Boot (beobachtet:
  Spielfenster erschien nie). libTAS serviert RAW ab Start und fängt solche
  Waits mit Busy-Loop-Detection ab. TODO im Code steht.
- **Tick-State als plain statics**: Teil des Snapshots, rewindt mit jedem
  Restore gratis (libTAS serialisiert den Timer aus demselben Grund in
  Savestates).
- **`real_ns()`**: echte Zeit via rustix-Raw-Syscall — für valo-internes
  (fps-Fenster, Diagnose-Dumps). NIEMALS `libc::clock_gettime` in
  valo-eigenem Code rufen: der Weg läuft durch die Interposition zurück in
  die virtuelle Uhr.
- **Config-Migration steht aus**: `VALO_DETERMINISTIC_TIME` /
  `VALO_FRAMERATE` sind noch Env-Vars; das Config-System (SetConfig bei
  Handshake + Runtime) ist da, det-time hängt noch nicht dran. Fast-Forward
  (`Config.fast_forward`) ist das Vorbild, wie ein Feld durchgereicht wird
  (`GlobalState::apply_config` ist der einzige Schreiber, an Boundaries).
- **Test**: `tests/det_time_frame_lock.c`.

## Die zentrale Lehre: die Uhr ist notwendig, nicht hinreichend

Det-time virtualisiert die **Uhr**, nicht die **Waits**. Ein Frame ist
durch, sobald alle physischen Blocker durch sind — die Boundary feuert im
Tempo des synchronsten Blockers. Fast-Forward entsteht erst, wenn *alle*
Echtzeit-Kopplungen raus sind:

| Kopplung | Status |
| -------- | ------ |
| Uhr (Reads) | ✅ det-time |
| Sleeps/Waits (sleep-paced Spiele) | ❌ gebaut & reverted (`fast_poll`), Design steht unten |
| Acquire/Image-Pipeline | ✅ `Config.fast_forward` (Skip-Modus, libTAS-Parität) |
| Render-Kosten | bleibt (Variante A rendert weiter; Variante B — Submits droppen — bewusst nicht gebaut) |

HK paced auf keinem Pfad über Sleeps (GLX: llvmpipe-Compute-Limit; Vulkan:
Acquire) — deshalb zeigte det-time allein bei HK keinen Effekt. Sleep-paced
Spiele würden vom untenstehenden Design profitieren.

## Messregeln (wichtig)

- **Unter det-time zeigt jeder In-Process-FPS-Counter (mangohud!) per
  Definition `VALO_FRAMERATE`** — mangohud liest CLOCK_MONOTONIC über glibc,
  also die virtuelle Uhr, die pro Boundary exakt eine Frame-Dauer advanct.
  Mangohud ist als FPS-Anzeige dann tot.
- Reale Rate: `fps:`-Zeilen im valo-log (`frame.rs::monotonic_ms` nutzt
  absichtlich `real_ns()`), oder det-time abschalten.

## Offen / nächste Schritte

1. **Sleep-/Wait-Virtualisierung** (`fast_poll`, gebaut & reverted): Main-
   Thread-Waits ≤ 1 virtuelles Frame-Budget (`nanosleep`/`poll`/`ppoll`-Timeouts)
   auf 0 fallen, fd-Readiness bleibt. Gates: det-time an ∧ 0 < t ≤ frame_ns
   ∧ Aufrufer ist Frame-Thread (`main_thread_tid`, erst nach erster Boundary
   bekannt). Größere Timeouts und andere Threads bleiben real (I/O-paced,
   z. B. FMOD — sonst Hot-Spins). Vor Wiedereinbau die sleepstats-Falle
   beachten: `total_ns` friert bei 0ms-Requests ein, avg wird bedeutungslos
   (Zähler für Calls mit ns>0 separat führen).
2. **Busy-Loop-Detection** (für RAW ab Start) — libTAS-Vorbild.
3. **Config-Migration** von `VALO_DETERMINISTIC_TIME`/`VALO_FRAMERATE` auf
   `Config` (Vorlage: `fast_forward`).
4. **Determinismus-Orakel**: RNG-State als erster, schärfster Sensor;
   kooperative Frame-Traces als semantisches Orakel; Snapshot-Diff als
   Forensik (Details im explicit-sync.md-Umfeld diskutiert).

## Einordnung: Skip-Invarianz statt Skip-Muster

Korrigierter Determinismusbegriff dieser Session: `fast-forward =
non-fast-forward` heißt **Skip-Invarianz** — Spulen darf den Spielzustand
nicht ändern, weil Rendering reiner Output ist. Das Skip-*Muster* und die
Spul-Rate sind damit frei wählbar; pro Spiel prüft das Trace-Orakel die
Invarianz. Spiele mit GPU-Readbacks in der Logik brechen die Invarianz —
für die bleibt nur Modus „uncapped mit echtem Rendering" (siehe
explicit-sync.md).
