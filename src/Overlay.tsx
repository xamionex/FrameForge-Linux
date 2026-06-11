import { useState, useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  buildLocalRewards, fetchRewardPrices, shortName, bestPickIndex,
  type PickPriority, type CraftingJobTs, type RewardItem,
} from "./rewardData";

import "./Overlay.css";

// ─── Icons ────────────────────────────────────────────────────────────────────
function PlatIcon({ size = 14 }: { size?: number }) {
  return <img src="/platinum.webp" alt="p" width={size} height={size} style={{ objectFit: "contain", flexShrink: 0, verticalAlign: "middle" }} />;
}
function DucatIcon({ size = 14 }: { size?: number }) {
  return <img src="/ducats.webp" alt="d" width={size} height={size} style={{ objectFit: "contain", flexShrink: 0, verticalAlign: "middle" }} />;
}

// ─── Pick arrow ───────────────────────────────────────────────────────────────
function PickArrow({ slotX, winW }: { slotX: number; winW: number }) {
  const cx = Math.round(winW * slotX);
  return (
    <svg className="ov-arrow" style={{ left: cx - 18 }} viewBox="0 0 36 28" fill="none">
      <polygon points="18,2 34,26 2,26" fill="#f0d060" />
    </svg>
  );
}

// ─── Reward card ──────────────────────────────────────────────────────────────
function RewardCard({ item, left, width, scannerEnabled }: { item: RewardItem; left: number; width: number; scannerEnabled: boolean }) {
  const hasSet    = !!item.components;
  const ownedFull = (item.complete_sets ?? 0) >= 1;
  const isUnknown = item.category === "Unrecognized";

  return (
    <div className={`ov-card${isUnknown ? " ov-card-unknown" : ""}`} style={{ left, width }}>

      {/* Item name */}
      <div className="ov-name">{isUnknown ? "? " : ""}{item.name}</div>

      {/* Plat price | vaulted lock | ducat value */}
      <div className="ov-price-row">
        <span className="ov-price-plat">
          {item.plat != null
            ? <><PlatIcon size={14} />&nbsp;{item.plat}</>
            : <span className="ov-price-na">—</span>}
        </span>
        {item.vaulted && <span className="ov-lock">🔒</span>}
        <span className="ov-price-ducat">
          {item.ducats != null && item.ducats > 0
            ? <><DucatIcon size={14} />&nbsp;{item.ducats}</>
            : <span className="ov-price-na">—</span>}
        </span>
      </div>

      {/* Set section — only when recipe data is available AND memory scanner is on */}
      {hasSet && scannerEnabled && <>
        {/* Ownership status bar */}
        <div className={`ov-own-bar ${ownedFull ? 'ov-own-yes' : 'ov-own-no'}`}>
          {ownedFull ? 'Full Item Owned' : 'Full Item Not Owned'}
        </div>

        {/* Total set value */}
        <div className="ov-set-price">
          <PlatIcon size={14} />
          &nbsp;{(item.total_plat ?? 0) > 0 ? item.total_plat : '—'}
        </div>

        {/* 2×2 component grid */}
        <div className="ov-comp-grid">
          {(item.components ?? []).slice(0, 4).map(c => {
            const raw  = shortName(c.name, item.set_name ?? '');
            // Strip trailing " Blueprint" from the cell label (keep "Blueprint" alone as-is)
            const stripped = raw !== 'Blueprint' && raw.endsWith(' Blueprint')
              ? raw.slice(0, -' Blueprint'.length)
              : raw;
            const owned = c.owned >= c.needed;
            return (
              <div key={c.unique_name} className={`ov-grid-cell ${owned ? 'ov-grid-owned' : 'ov-grid-miss'}`}>
                <span className="ov-grid-name">{stripped}</span>
                <span className="ov-grid-qty">{c.owned}</span>
              </div>
            );
          })}
        </div>
      </>}

      {/* Category */}
      {item.category && <div className="ov-cat">{item.category}</div>}
    </div>
  );
}

// ─── Main overlay ─────────────────────────────────────────────────────────────
export default function Overlay() {
  const [rewards, setRewards] = useState<RewardItem[]>([]);
  const params   = new URLSearchParams(window.location.search);
  const winW     = parseInt(params.get("ww") ?? "1920", 10);
  const priority = (params.get("priority") ?? "completion") as PickPriority;
  const scannerEnabled = (params.get("scanner") ?? "on") === "on";
  // In-game UI scale (fraction). Cards shrink and cluster toward centre at lower
  // scales, so the column spacing and width scale with it. Clamped to a sane range.
  const uiScale  = Math.min(1, Math.max(0.5, parseFloat(params.get("uiscale") ?? "1") || 1));

  const prevKey      = useRef<string>("");
  const catalogRef   = useRef<Record<string, any>>({});
  const quantRef     = useRef<Record<string, number>>({});
  const craftingRef  = useRef<Record<string, number>>({});  // normalized unique_name → crafting count

  // Force document-level transparency — only runs when this overlay window mounts,
  // never in the main app. App.css sets background on html/#root which overrides
  // the body-only rule in Overlay.css, so we clear it via JS here.
  useEffect(() => {
    const important = (el: HTMLElement | null) => {
      if (el) el.style.setProperty('background', 'transparent', 'important');
    };
    important(document.documentElement);
    important(document.getElementById('root'));
  }, []);

  // Subprocess mode: read the payload directly from the temp file (Tauri events
  // don't cross process boundaries). The overlay process has no AppState, so it
  // can't resolve items itself — the MAIN process pre-enriches them and ships the
  // finished RewardItem[] in the payload's `rewards`. We render those instantly,
  // then poll the enrichment file for the price-filled version the main process
  // writes a moment later (warframe.market lookups are rate-limited).
  useEffect(() => {
    // Subprocess mode only. The `payload` URL param marks this as the spawned
    // overlay process (the in-process Windows native window doesn't set it and
    // resolves via its own real invoke handler through the second effect below).
    const payloadPath = params.get("payload");
    if (!payloadPath) return;

    let cancelled = false;
    let pollTimer: ReturnType<typeof setInterval> | undefined;

    // The webview cannot fetch file:// under WebKitGTK (blocked from the app
    // origin), so the overlay subprocess exposes read_overlay_payload /
    // read_overlay_enriched commands that read the temp files in Rust and return
    // the parsed JSON. They resolve null while a file is still absent.
    invoke<any>("read_overlay_payload")
      .then((data: any) => {
        if (cancelled || !data) return;
        if (Array.isArray(data.rewards) && data.rewards.length > 0) {
          // Pre-enriched by the main process — render immediately.
          prevKey.current = (data.items ?? []).join(",");
          setRewards(data.rewards);
          // Keep polling the enrichment file for the overlay's lifetime: the main
          // process writes both price fills AND repeat-scan refinements (the
          // majority-vote re-reads) here. Apply whenever the content changes,
          // rather than stopping after the first read. Seed with the initial set so
          // an identical first enriched read doesn't trigger a redundant re-render.
          let lastEnriched = JSON.stringify(data.rewards);
          pollTimer = setInterval(() => {
            invoke<any>("read_overlay_enriched")
              .then((priced: any) => {
                if (cancelled || !Array.isArray(priced) || priced.length === 0) return;
                const key = JSON.stringify(priced);
                if (key === lastEnriched) return;
                lastEnriched = key;
                setRewards(priced);
              })
              .catch(() => {});
          }, 400);
        } else if (data.items && data.items.length > 0) {
          // Legacy / in-process (Windows) path: enrich here via invoke.
          const synthetic = new CustomEvent("relic-rewards-payload", {
            detail: { items: data.items, positions: data.positions, names: data.names },
          });
          window.dispatchEvent(synthetic);
        }
      })
      .catch(() => {});

    return () => { cancelled = true; if (pollTimer) clearInterval(pollTimer); };
  }, []);

  useEffect(() => {
    type EventPayload = { paths: string[]; positions: number[]; names?: string[] };
    let pendingEvent: EventPayload | null = null;
    let dataReady = false;

    // In-process enrichment path (Windows native window, or any case where the
    // payload arrived without pre-resolved rewards). Here invoke() reaches the
    // real AppState, so we resolve locally then layer prices on — the same shared
    // resolver the main process uses for the Linux subprocess.
    const processPayload = async (paths: string[], positions: number[], names?: string[]) => {
      const key = paths.join(",");
      if (key === prevKey.current) return;

      const currentCount = rewards.length;
      if (paths.length <= currentCount && currentCount > 0) return;

      prevKey.current = key;

      // Refresh quantities from the latest memory scan before rendering.
      // The overlay can open before the monitor thread has populated
      // current_quantities, so the initial mount-time fetch may be stale.
      try {
        quantRef.current = await invoke<Record<string, number>>("get_current_quantities");
      } catch { /* keep existing */ }

      // Upstream v1.12.0: "?: " prefix marks raw OCR text that couldn't be matched
      // to a catalog item. Build Unrecognized cards for these, then enrich the rest.
      const unrecognized: RewardItem[] = [];
      const recognizedPaths: string[] = [];
      const recognizedPositions: number[] = [];
      paths.forEach((path, i) => {
        if (path.startsWith("?:")) {
          unrecognized.push({
            unique_name: path + "-" + i,
            raw_unique: "",
            slot_x: positions[i] ?? 0.5,
            name: path.slice(2),
            category: "Unrecognized",
          });
        } else {
          recognizedPaths.push(path);
          recognizedPositions.push(positions[i] ?? 0.5);
        }
      });

      const local = recognizedPaths.length > 0
        ? await buildLocalRewards(
            recognizedPaths, recognizedPositions, catalogRef.current, quantRef.current, craftingRef.current, names,
          )
        : [];
      // Merge unrecognized items back in, preserving original order by slot_x.
      const combined = [...local, ...unrecognized].sort((a, b) => a.slot_x - b.slot_x);
      setRewards(combined);

      // `local` already carries plat from the bulk snapshot. Only hit the live,
      // rate-limited warframe.market path to fill snapshot MISSES (null plat).
      // Unrecognized items have no plat — skip them.
      const hasMiss = local.some(r =>
        r.plat == null || (r.components?.some(c => c.plat == null) ?? false));
      if (hasMiss) {
        fetchRewardPrices(local)
          .then(priced => {
            // Merge priced recognized items back with unrecognized.
            const pricedCombined = [...priced, ...unrecognized].sort((a, b) => a.slot_x - b.slot_x);
            if (prevKey.current === key) setRewards(pricedCombined);
          })
          .catch(() => {});
      }
    };

    // Tauri event (in-process native window on KDE)
    const unsub = listen<{ items: string[]; positions: number[]; names?: string[] } | null>(
      "relic-rewards",
      async (e) => {
        const payload = e.payload;
        if (!payload || payload.items.length === 0) {
          getCurrentWebviewWindow().close().catch(() => {});
          return;
        }

        if (dataReady) {
          await processPayload(payload.items, payload.positions, payload.names);
        } else {
          pendingEvent = { paths: payload.items, positions: payload.positions, names: payload.names };
        }
      }
    );

    // Synthetic DOM event (subprocess mode — payload read from temp file)
    const onSynthetic = async (e: Event) => {
      const d = (e as CustomEvent).detail as { items: string[]; positions: number[]; names?: string[] };
      if (dataReady) {
        await processPayload(d.items, d.positions, d.names);
      } else {
        pendingEvent = { paths: d.items, positions: d.positions, names: d.names };
      }
    };
    window.addEventListener("relic-rewards-payload", onSynthetic);

    Promise.allSettled([
      invoke<any[]>("get_all_items"),
      invoke<Record<string, number>>("get_current_quantities"),
      invoke<CraftingJobTs[]>("get_current_crafting"),
    ]).then(([itemsR, quantitiesR, craftingR]) => {
      if (itemsR.status === 'fulfilled') {
        const byUnique: Record<string, any> = {};
        for (const i of itemsR.value) byUnique[i.unique_name] = i;
        catalogRef.current = byUnique;
      }
      if (quantitiesR.status === 'fulfilled') {
        quantRef.current = quantitiesR.value;
      }
      if (craftingR.status === 'fulfilled') {
        const byCraft: Record<string, number> = {};
        for (const job of craftingR.value) {
          const lk = job.unique_name.replace("/Lotus/StoreItems/", "/Lotus/");
          byCraft[lk] = (byCraft[lk] ?? 0) + 1;
        }
        craftingRef.current = byCraft;
      }
      dataReady = true;

      if (pendingEvent) {
        processPayload(pendingEvent.paths, pendingEvent.positions, pendingEvent.names);
        pendingEvent = null;
      }
    });

    return () => {
      unsub.then(fn => fn());
      window.removeEventListener("relic-rewards-payload", onSynthetic);
    };
  }, []);

  if (rewards.length === 0) {
    // Diagnostic: always render something so the user can tell the overlay
    // window is actually alive (helps debug Linux transparent-window issues).
    // Placed at bottom-right so it never overlaps the reward text band.
    return (
      <div className="ov-root">
        <div style={{
          position: "fixed", bottom: 4, right: 4,
          color: "#f85149", fontSize: 11, background: "rgba(0,0,0,0.7)",
          padding: "2px 6px", borderRadius: 4, zIndex: 9999, pointerEvents: "none",
          fontFamily: "system-ui, sans-serif", fontWeight: 700,
        }}>
          FrameForge Overlay — waiting for rewards…
        </div>
      </div>
    );
  }

  const bestIdx = bestPickIndex(rewards, priority);

  const n  = rewards.length;
  // Fixed column layout symmetric around screen center.
  // Column spacing ≈ 12.7 % of screen width (measured from game at multiple resolutions).
  // For N cards, columns map to the game's actual slot positions:
  //   N=1: center; N=2: cols 2&3 (±0.5s); N=3: cols 1, 2.5, 4 (±1.5s and 0);
  //   N=4: cols 1-4 (±0.5s and ±1.5s).
  // Spacing and card width scale with the in-game UI scale (cards are smaller and
  // closer together at lower scales). The symmetric ±s layout below keeps them
  // centred, matching the game's centred reward box at any UI scale.
  const colSpacing    = winW * 0.127 * uiScale;
  const screenCenter  = winW / 2;
  const cardW = Math.max(Math.round(80 * uiScale), Math.round(colSpacing - 10));
  const colCenters: number[] = (() => {
    const s = colSpacing, c = screenCenter;
    if (n === 1) return [c];
    if (n === 2) return [c - 0.5 * s, c + 0.5 * s];
    if (n === 3) return [c - s, c, c + s];
    return [c - 1.5 * s, c - 0.5 * s, c + 0.5 * s, c + 1.5 * s];
  })();
  const cardLeft = (idx: number) => Math.round((colCenters[idx] ?? screenCenter) - cardW / 2);

  return (
    <div className="ov-root">
      {bestIdx >= 0 && <PickArrow slotX={(colCenters[bestIdx] ?? screenCenter) / winW} winW={winW} />}
      {rewards.map((item, idx) => (
        <RewardCard key={item.unique_name} item={item} left={cardLeft(idx)} width={cardW} scannerEnabled={scannerEnabled} />
      ))}
    </div>
  );
}
