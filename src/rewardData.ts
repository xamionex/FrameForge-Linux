// Shared reward-resolution logic.
//
// Why this module exists:
//   The relic overlay on Linux runs as a separate XWayland subprocess whose
//   Tauri `invoke` handler returns EMPTY data (it has no AppState — the catalog,
//   prices, quantities and recipes all live in the main process). So the overlay
//   process can never resolve an item's name / plat / ducats / components on its
//   own; it only receives the raw unique-name paths.
//
//   The fix: resolve everything in the MAIN process (App.tsx), where `invoke`
//   and the catalog work, and ship the finished RewardItem[] to the overlay
//   through the payload file. These functions are the shared resolver used both
//   by App.tsx (to enrich before spawning the overlay) and by Overlay.tsx (for
//   the in-process Windows path, where its own invoke calls do return real data).

import { invoke } from "@tauri-apps/api/core";

export type PickPriority = "completion" | "plat" | "ducat" | "setPlat";

export interface ComponentRow {
  unique_name: string;
  name: string;
  needed: number;
  owned: number;  // blueprint_qty + built_qty + crafting_qty combined
  plat?: number;
  ducats?: number;
}

export interface CraftingJobTs {
  unique_name: string;
  item_name: string;
  completion_ms: number;
}

export interface RewardItem {
  unique_name: string;
  raw_unique: string;
  slot_x: number;
  name: string;
  category?: string;
  vaulted?: boolean;
  plat?: number;
  ducats?: number;
  owned_qty?: number;
  set_name?: string;
  components?: ComponentRow[];
  complete_sets?: number;
  missing_plat?: number;
  total_plat?: number;   // sum of all component prices × needed qty
}

// ─── Set-name derivation ──────────────────────────────────────────────────────
const WF_COMP_SUFF = [' Neuroptics Blueprint', ' Chassis Blueprint', ' Systems Blueprint'];
const PART_SUFF = [
  ' Upper Limb', ' Lower Limb', ' Receiver', ' Barrel', ' Stock',
  ' Blade', ' Handle', ' Guard', ' Hilt', ' Link', ' Gauntlet',
  ' Carapace', ' Cerebrum', ' Systems', ' Head', ' Strike', ' Boot',
];

export function getSetName(itemName: string): string | null {
  for (const s of WF_COMP_SUFF) if (itemName.endsWith(s)) return itemName.slice(0, -s.length);
  for (const s of PART_SUFF)    if (itemName.endsWith(s)) return itemName.slice(0, -s.length);
  if (itemName.endsWith(' Blueprint')) {
    const base = itemName.slice(0, -' Blueprint'.length);
    for (const s of PART_SUFF) if (base.endsWith(s)) return base.slice(0, -s.length);
    return base;
  }
  return null;
}

export function shortName(fullName: string, setName: string): string {
  return fullName.startsWith(setName + ' ') ? fullName.slice(setName.length + 1) : fullName;
}

// ─── Best-pick calculation ────────────────────────────────────────────────────
export function bestPickIndex(items: RewardItem[], priority: PickPriority): number {
  if (items.length === 0) return -1;

  const scores = items.map(item => {
    switch (priority) {
      case "plat":    return item.plat ?? 0;
      case "ducat":   return item.ducats ?? 0;
      case "setPlat": return item.missing_plat ?? item.plat ?? 0;
      case "completion": {
        if (!item.components || !item.set_name) return item.plat ?? 0;
        const sn = item.set_name;
        const sn_short = shortName(item.name, sn);
        const thisComp = item.components.find(c => shortName(c.name, sn) === sn_short);
        if (!thisComp) return 0;
        const stillNeed = thisComp.needed - thisComp.owned;
        if (stillNeed <= 0) return -1;
        const totalNeeded = item.components.reduce((a, c) => a + Math.max(0, c.needed - c.owned), 0);
        return stillNeed * 100 - totalNeeded;
      }
    }
  });

  let best = 0;
  for (let i = 1; i < scores.length; i++) if ((scores[i] ?? 0) > (scores[best] ?? 0)) best = i;
  return (scores[best] ?? 0) > 0 ? best : -1;
}

// ─── Local enrichment (instant) ────────────────────────────────────────────────
//
// Resolves everything that comes from LOCAL data: display name, category,
// vaulted, ducats, owned quantity, the component list (with per-component
// owned/needed counts and ducats) and the full-set ownership count.
//
// Plat is resolved here too via `get_item_prices`, which reads the bulk price
// snapshot (wfinfo) in memory — no per-item warframe.market call — so plat shows
// in the SAME frame as ducats. Snapshot misses come back null and are reconciled
// afterwards by the live, rate-limited path (see fetchRewardPrices). `get_recipe`
// is a local AppState lookup, not a network call.
export async function buildLocalRewards(
  paths: string[],
  positions: number[],
  byUnique: Record<string, any>,
  quantities: Record<string, number>,
  crafting: Record<string, number>,
  names?: string[],
): Promise<RewardItem[]> {
  const cat = byUnique;
  const qty = quantities;

  // Instant plat from the in-memory bulk snapshot (null = snapshot miss).
  const bulkPrices = async (names: string[]): Promise<(number | null)[]> =>
    invoke<(number | null)[]>("get_item_prices", { itemNames: names }).catch(() => names.map(() => null));

  const resolveOwned = (uniqueName: string, displayName: string): number => {
    const clk = uniqueName.replace("/Lotus/StoreItems/", "/Lotus/");
    const catEntry = cat[clk] ?? cat[uniqueName]
      ?? (Object.values(cat).find((it: any) => it.name === displayName) as any);
    const catClk = catEntry
      ? (catEntry.unique_name as string).replace("/Lotus/StoreItems/", "/Lotus/")
      : clk;
    const directQty = qty[catClk] ?? qty[clk] ?? qty[uniqueName] ?? 0;

    let altQty = 0;
    let altClk: string | null = null;
    if (displayName.endsWith(' Blueprint')) {
      const partName = displayName.slice(0, -' Blueprint'.length);
      const partEntry = Object.values(cat).find(
        (it: any) => it.name === partName && it.category === 'Parts'
      ) as any;
      if (partEntry) {
        altClk = (partEntry.unique_name as string).replace("/Lotus/StoreItems/", "/Lotus/");
        altQty = qty[altClk] ?? qty[partEntry.unique_name] ?? 0;
      }
    } else {
      const bpName = displayName + ' Blueprint';
      const bpEntry = Object.values(cat).find(
        (it: any) => it.name === bpName && it.category === 'Blueprints'
      ) as any;
      if (bpEntry) {
        altClk = (bpEntry.unique_name as string).replace("/Lotus/StoreItems/", "/Lotus/");
        altQty = qty[altClk] ?? qty[bpEntry.unique_name] ?? 0;
      }
    }

    const craftQty =
      crafting[catClk] ?? crafting[clk] ?? crafting[uniqueName]
      ?? (altClk ? (crafting[altClk] ?? 0) : 0);

    return directQty + altQty + craftQty;
  };

  return Promise.all(paths.map(async (path, i): Promise<RewardItem> => {
    const lk   = path.replace("/Lotus/StoreItems/", "/Lotus/");
    // The OCR layer's authoritative display name (when provided) is the most
    // reliable join key. The catalog dedups blueprints by name and keeps the
    // ExportRecipes path, so the recipe-component path the OCR emits often isn't
    // a catalog key — but the NAME always is. Resolve by name first, then by path.
    const ocrName = names?.[i];
    const meta = (ocrName ? (Object.values(cat).find((it: any) => it.name === ocrName) as any) : undefined)
      ?? cat[lk] ?? cat[path]
      ?? (Object.values(cat).find((it: any) => it.unique_name === path || it.unique_name === lk) as any);
    const name = ocrName ?? meta?.name ?? path.split("/").pop() ?? path;

    const reward: RewardItem = {
      unique_name: path + "-" + i,
      raw_unique: lk,
      slot_x:    positions[i] ?? 0.5,
      name,
      category:  meta?.category,
      vaulted:   meta?.vaulted,
      ducats:    meta?.ducats,
      owned_qty: qty[lk] ?? qty[path] ?? 0,
    };

    // Reward-item plat from the bulk snapshot (instant). null on a snapshot miss.
    const [selfPlat] = await bulkPrices([name]);
    reward.plat = selfPlat ?? undefined;

    const setName = getSetName(name);
    if (!setName) return reward;

    const setPrefix = setName + " ";
    let components: ComponentRow[] | null = null;

    let setEntry = Object.values(cat).find((item: any) =>
      item.name === setName && item.category !== 'Blueprints' && item.category !== 'Parts'
    ) as any;
    if (!setEntry) {
      setEntry = Object.values(cat).find((item: any) => item.name === setName) as any;
    }

    type RC = { unique_name: string; name: string; count: number; result_count: number };

    // Attempt 1: recipe lookup (exact ingredient list + correct needed counts)
    if (setEntry) {
      const recipe = await invoke<RC[]>("get_recipe", { unique_name: setEntry.unique_name }).catch(() => [] as RC[]);
      if (recipe.length) {
        const parts = recipe.filter(c => {
          const clk = c.unique_name.replace("/Lotus/StoreItems/", "/Lotus/");
          const cm  = cat[clk] ?? cat[c.unique_name];
          return cm?.category === 'Parts' || cm?.category === 'Blueprints';
        });
        if (parts.length >= 1) {
          components = parts.map(c => {
            const clk = c.unique_name.replace("/Lotus/StoreItems/", "/Lotus/");
            const cm  = cat[clk] ?? cat[c.unique_name];
            return {
              unique_name: c.unique_name,
              name:        c.name,
              needed:      c.count ?? 1,
              owned:       resolveOwned(c.unique_name, c.name),
              ducats:      cm?.ducats,
            };
          });
        }
      }
    }

    // Attempt 2: catalog prefix search (works even when recipe cache is empty)
    if (!components) {
      const parts = Object.values(cat)
        .filter((item: any) => item.name?.startsWith(setPrefix) &&
                        (item.category === 'Parts' || item.category === 'Blueprints'))
        .map((item: any) => ({
          unique_name: item.unique_name,
          name:        item.name as string,
          needed:      1,
          owned:       resolveOwned(item.unique_name, item.name as string),
          ducats:      item.ducats as number | undefined,
        }));
      if (parts.length >= 2) components = parts;
    }

    if (!components) return reward;

    const compSets = components.length > 0
      ? Math.floor(Math.min(...components.map(c => c.owned / c.needed)))
      : 0;
    const builtLk  = (setEntry?.unique_name ?? '').replace("/Lotus/StoreItems/", "/Lotus/");
    const builtQty = setEntry ? (qty[builtLk] ?? qty[setEntry.unique_name] ?? 0) : 0;
    const completeSets = builtQty > 0 ? Math.max(compSets, 1) : compSets;

    // DEBUG: log every component lookup for this reward item
    // Also dump matching qty keys so we can verify path alignment
    const matchingKeys = Object.entries(qty).filter(([k]) =>
      k.toLowerCase().includes((setName ?? '').toLowerCase().replace(' ', ''))
    ).map(([k, v]) => `${k}=${v}`);
    console.log(`[FF reward] ${name}  built=${builtQty} compSets=${compSets} complete=${completeSets}`);
    console.log(`[FF reward] qty keys matching '${setName}': ${matchingKeys.length > 0 ? matchingKeys.join(', ') : 'NONE'}`);
    for (const c of components) {
      console.log(`  component: ${c.name}  unique=${c.unique_name}  owned=${c.owned}  needed=${c.needed}`);
    }

    // Component plat from the bulk snapshot (instant), folded into set totals so
    // missing_plat / total_plat are ready on first paint. Misses (null) are filled
    // later by fetchRewardPrices.
    const compPrices = await bulkPrices(components.map(c => c.name));
    const pricedComponents = components.map((c, idx) =>
      compPrices[idx] != null ? { ...c, plat: compPrices[idx]! } : c);
    const missing_plat = pricedComponents.reduce((acc, c) =>
      (c.owned < c.needed && c.plat) ? acc + c.plat * (c.needed - c.owned) : acc, 0);
    const total_plat = pricedComponents.reduce((acc, c) =>
      c.plat != null ? acc + c.plat * (c.needed ?? 1) : acc, 0);

    return { ...reward, set_name: setName, components: pricedComponents, complete_sets: completeSets, missing_plat, total_plat };
  }));
}

// ─── Price enrichment (network) ─────────────────────────────────────────────────
//
// Fetches plat for each item and each component from warframe.market, then folds
// component prices into missing_plat / total_plat. Runs AFTER the overlay is
// already on screen, so a cold (uncached) price lookup never delays the overlay —
// the values simply fill in once they arrive.
export async function fetchRewardPrices(rewards: RewardItem[]): Promise<RewardItem[]> {
  return Promise.all(rewards.map(async (r): Promise<RewardItem> => {
    const plat = await invoke<number | null>("get_item_price", { itemName: r.name }).catch(() => null);

    let components = r.components;
    let missing_plat = r.missing_plat;
    let total_plat = r.total_plat;

    if (components && components.length) {
      const priced = await Promise.all(components.map(async c => {
        const cp = await invoke<number | null>("get_item_price", { itemName: c.name }).catch(() => null);
        return cp != null ? { ...c, plat: cp } : c;
      }));
      components = priced;
      missing_plat = priced.reduce((acc, c) =>
        (c.owned < c.needed && c.plat) ? acc + c.plat * (c.needed - c.owned) : acc, 0);
      total_plat = priced.reduce((acc, c) =>
        c.plat != null ? acc + c.plat * (c.needed ?? 1) : acc, 0);
    }

    return {
      ...r,
      plat: plat ?? r.plat,
      components,
      missing_plat,
      total_plat,
    };
  }));
}
