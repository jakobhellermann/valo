<!-- // TODO(ai-review): generated, not yet audited -->

# Unity PreloadManager: Ablauf und Determinismus

Stand: Oktober 2026, Unity 6000.0.61f1 (Hollow Knight). Zusammengesetzt aus
der Call-Struktur im Maschinencode (`UnityPlayer.so`, Symbole aus
`UnityPlayer_s.debug`), dem U6-vtable-Layout von libTAS und den Logs von
`gamespecific::unity` (Kategorie `unity`).

## Überblick

```
Main-Thread                          │  Thread "Loading.Preload" (PreloadManager::Run)
─────────────────────────────────────┼──────────────────────────────────────────────
SceneManager.LoadSceneAsync(...)     │
 └ PreloadManager::AddToQueue(op)    │
     lock; queue.push_back(op)       │
     futex wake ─────────────────────┼─► wacht auf
                                     │   lock; nimmt die op mit höchster
                                     │     GetPriority()                   (vtable +0x28)
                                     │   GetAllowParallelExecution()?       (vtable +0x90)
                                     │   op->Perform()  ← Hintergrundarbeit:
                                     │     Dateien lesen (SyncReadRequest), deserialisieren
                                     │   op wird "bereit zur Integration"
jeder Frame:                         │   wieder schlafen (futex wait)
 PreloadManager::UpdatePreloading()  │
   lock; MustCompleteNextFrame()?    │                                      (+0x78)
   UpdatePreloadingSingleStep(f, 50) │
     GetAllowSceneActivation()?      │                                      (+0x38)
     IntegrateTimeSliced(op, budget) │  ← Main-Thread-Teil in Zeitscheiben  (+0x68)
       (clock_gettime: Zeitbudget)   │
     IntegrateMainThread(op)         │  ← Abschluss, z. B. Szene aktivieren (+0x70)
     CanLoadObjects()…               │                                      (+0x80)
   op fertig → AsyncOperation.isDone │
```

## Drei Phasen pro Operation

1. **Einreihen** (`AddToQueue`, Main-Thread): die Operation landet in einem
   Vector, der Preload-Thread wird per futex geweckt (der `syscall` in
   `AddToQueue`).
2. **Perform** (Preload-Thread): nimmt die Operation mit der höchsten Priorität
   (also nicht strikt FIFO) und führt `Perform()` aus: Dateien lesen, Objekte
   aufbauen. Der teure Teil; wie viele Frames er dauert, hängt von CPU und I/O
   ab.
3. **Integrieren** (Main-Thread, einmal pro Frame in `UpdatePreloading`):
   sobald eine Operation durch `Perform` ist, integriert der Main-Thread sie in
   Zeitscheiben (`IntegrateTimeSliced` mit Budget, im Log `budget=50`), dann
   `IntegrateMainThread` als Abschluss. `GetAllowSceneActivation` kann eine
   Szene absichtlich zurückhalten (`allowSceneActivation = false` in C#).

Operationstypen mit eigenem `Perform`/`IntegrateMainThread` in dieser Version:
`LoadOperation`, `LoadSceneOperation`, `UnloadSceneOperation`,
`UnloadUnusedAssetsOperation`, `AssetBundle*Operation`.

## Sonderfälle

- **`GetAllowParallelExecution() == false`**: darf nicht parallel zu anderen
  laufen; laut libTAS wartet der Preload-Thread dann in `Run` auf das
  `UpdatePreloadingSingleStep` des Main-Threads. Wer auf dem Main-Thread auf
  das Ende von `Perform` wartet, kann sich hier deadlocken.
- **`MustCompleteNextFrame()`**: erzwingt die Integration im nächsten Frame.
- **`WaitForAllAsyncOperationsToComplete`** (Start, synchrone Loads): der
  Main-Thread wartet in einer Schleife auf ein Semaphor
  (`CappedSemaphore::TryTimedAcquire`, 1 ms) und integriert, bis die Queue leer
  ist; jede Iteration liest `CLOCK_MONOTONIC_RAW` für das Zeitbudget. Das ist
  die Boot-Schleife, die bei eingefrorener virtueller Uhr ewig spinnt
  (siehe `deterministic-time.md`).

## Determinismus

Phase 1 und 3 laufen auf dem Main-Thread und sind pro Frame deterministisch.
Phase 2 läuft parallel zu den Frames: wann sie fertig ist und damit ab welchem
Frame `UpdatePreloading` integrieren kann, ist die Stelle, an der Echtzeit in
die Frame-Zählung leckt.

Gemessen mit zwei parallelen Läufen von `menu.tas` (260 Frames):

| Operation (2. Zyklus, eingereiht in Frame 1) | Lauf 1 | Lauf 2 |
| --- | --- | --- |
| `LoadSceneOperation::Perform` | Frame 1 → 1 | Frame 1 → **41** |
| `UnloadUnusedAssetsOperation::Perform` | Frame 1 | Frame 41 |
| `IntegrateMainThread` | Frame 3 | Frame 42 |
| nächster Zyklus (`AddToQueue`) | Frame 5 | Frame 44 |

Die Abfolge ist in beiden Läufen identisch, nur die Dauer von `Perform` in
Frames nicht.

Bisher in `gamespecific::unity`:

- Async-Reads synchron (`AsyncReadManagerThreaded::Request` →
  `SyncReadRequest`): entfernt das I/O-Rennen, aber nicht das von `Perform`.

Offen: in `AddToQueue` warten, bis `Perform` dieser Operation durch ist.
Dafür reichen die eigenen `Perform`-Hooks, die Queue-Felder des Managers
braucht es nicht (libTAS liest sie in `WaitForQueueToProcess`, weil es
`Perform` nicht pro Typ hookt). Nur warten bei Typen, deren `Perform` gehookt
ist (am vtable-Pointer der Operation erkennbar), und nicht bei
`GetAllowParallelExecution() == false` (vtable `+0x90`), sonst Deadlock.

## Ungeprüft

- Bedeutung der Parameter von `UpdatePreloadingSingleStep` (Flags, `50`) und
  der Manager-Felder bei `+0x328`/`+0x348`, auf die
  `WaitForAllAsyncOperationsToComplete` prüft (vermutlich die Queues).
- Die vtable-Offsets stammen aus libTAS' U6-Klasse und passen zu den
  Call-Stellen (`+0x28` vor der Auswahl in `Run`, `+0x68`/`+0x70` in der
  Integration), sind aber nicht einzeln verifiziert.
