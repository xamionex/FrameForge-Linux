import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./Statistics.css";

// ── Types ─────────────────────────────────────────────────────────────────────

interface Trade {
  id: number;
  timestamp: string;
  with_player: string;
  direction: "sold" | "bought";
  item_name: string;
  item_url: string;
  quantity: number;
  platinum: number;
  source: "wfm" | "ingame" | "manual";
  notes: string;
}

function fmt(n: number) { return n.toLocaleString(); }

function fmtDate(iso: string) {
  const d = new Date(iso);
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" })
    + " " + d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

// ── Platinum sparkline ────────────────────────────────────────────────────────

function PlatChart({ trades }: { trades: Trade[] }) {
  if (trades.length < 2) return null;

  // Group by day, accumulate running net platinum
  const byDay = new Map<string, number>();
  [...trades].reverse().forEach(t => {
    const day = t.timestamp.slice(0, 10);
    const delta = t.direction === "sold" ? t.platinum * t.quantity : -t.platinum * t.quantity;
    byDay.set(day, (byDay.get(day) ?? 0) + delta);
  });

  const days = [...byDay.entries()];
  let running = 0;
  const points = days.map(([d, delta]) => { running += delta; return { d, v: running }; });
  if (points.length < 2) return null;

  const vals = points.map(p => p.v);
  const min = Math.min(...vals);
  const max = Math.max(...vals);
  const range = max - min || 1;
  const W = 400, H = 80;
  const px = (i: number) => (i / (points.length - 1)) * W;
  const py = (v: number) => H - ((v - min) / range) * (H - 4) - 2;
  const polyline = points.map((p, i) => `${px(i)},${py(p.v)}`).join(" ");
  const area = `0,${H} ` + polyline + ` ${W},${H}`;
  const lastVal = vals[vals.length - 1];

  return (
    <div className="stat-chart-wrap">
      <div className="stat-chart-label">Platinum over time</div>
      <svg viewBox={`0 0 ${W} ${H}`} className="stat-chart" preserveAspectRatio="none">
        <defs>
          <linearGradient id="pg" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor={lastVal >= 0 ? "var(--green)" : "var(--red)"} stopOpacity="0.25"/>
            <stop offset="100%" stopColor={lastVal >= 0 ? "var(--green)" : "var(--red)"} stopOpacity="0"/>
          </linearGradient>
        </defs>
        <polygon points={area} fill="url(#pg)" />
        <polyline points={polyline} fill="none"
          stroke={lastVal >= 0 ? "var(--green)" : "var(--red)"}
          strokeWidth="1.8" strokeLinejoin="round" />
      </svg>
      <div className="stat-chart-footer">
        <span>{points[0]?.d}</span>
        <span style={{ color: lastVal >= 0 ? "var(--green)" : "var(--red)", fontWeight: 700 }}>
          {lastVal >= 0 ? "+" : ""}{fmt(lastVal)}p net
        </span>
        <span>{points[points.length - 1]?.d}</span>
      </div>
    </div>
  );
}

// ── Add trade form ────────────────────────────────────────────────────────────

function AddTradeForm({ onAdd, onCancel }: { onAdd: () => void; onCancel: () => void }) {
  const [direction, setDirection] = useState<"sold" | "bought">("sold");
  const [itemName, setItemName]   = useState("");
  const [player, setPlayer]       = useState("");
  const [platinum, setPlatinum]   = useState(0);
  const [quantity, setQuantity]   = useState(1);
  const [notes, setNotes]         = useState("");
  const [saving, setSaving]       = useState(false);

  const submit = async () => {
    if (!itemName || !platinum) return;
    setSaving(true);
    await invoke("add_trade", {
      withPlayer: player, direction, itemName, itemUrl: "", quantity, platinum, source: "manual", notes,
    }).catch(() => {});
    setSaving(false);
    onAdd();
  };

  return (
    <div className="stat-add-form">
      <div className="stat-form-title">Log a Trade</div>
      <div className="stat-form-row">
        <label>Type</label>
        <div className="stat-type-btns">
          <button className={direction === "sold" ? "active" : ""} onClick={() => setDirection("sold")}>Sold</button>
          <button className={direction === "bought" ? "active" : ""} onClick={() => setDirection("bought")}>Bought</button>
        </div>
      </div>
      <div className="stat-form-row">
        <label>Item</label>
        <input placeholder="Ash Prime Set…" value={itemName} onChange={e => setItemName(e.target.value)} />
      </div>
      <div className="stat-form-row">
        <label>Player</label>
        <input placeholder="IGN (optional)" value={player} onChange={e => setPlayer(e.target.value)} />
      </div>
      <div className="stat-form-row">
        <label>Platinum</label>
        <input type="number" min={1} value={platinum} onChange={e => setPlatinum(+e.target.value)} />
      </div>
      <div className="stat-form-row">
        <label>Quantity</label>
        <input type="number" min={1} value={quantity} onChange={e => setQuantity(+e.target.value)} />
      </div>
      <div className="stat-form-row">
        <label>Notes</label>
        <input placeholder="Optional notes…" value={notes} onChange={e => setNotes(e.target.value)} />
      </div>
      <div className="stat-form-actions">
        <button className="stat-btn-primary" onClick={submit} disabled={saving || !itemName || !platinum}>
          {saving ? "Saving…" : "Add Trade"}
        </button>
        <button className="stat-btn-secondary" onClick={onCancel}>Cancel</button>
      </div>
    </div>
  );
}

// ── Trades tab ────────────────────────────────────────────────────────────────

function TradesTab() {
  const [trades, setTrades]       = useState<Trade[]>([]);
  const [showForm, setShowForm]   = useState(false);
  const [filter, setFilter]       = useState("");
  const [confirmDel, setConfirmDel] = useState<{ id: number; name: string } | null>(null);

  const loadTrades = useCallback(async () => {
    const t = await invoke<Trade[]>("get_trades").catch(() => [] as Trade[]);
    setTrades(t);
  }, []);

  useEffect(() => { loadTrades(); }, [loadTrades]);

  // Listen for fully parsed trade completions from EE.log — auto-log them
  useEffect(() => {
    const unlisten = listen<{
      withPlayer: string; direction: string; itemName: string;
      quantity: number; platinum: number; timestamp: string;
    }>("trade-completed", async e => {
      const p = e.payload;
      await invoke("add_trade", {
        withPlayer: p.withPlayer,
        direction:  p.direction,
        itemName:   p.itemName,
        itemUrl:    "",
        quantity:   p.quantity ?? 1,
        platinum:   p.platinum,
        source:     "ingame",
        notes:      "",
      }).catch(() => {});
      loadTrades();
    });
    return () => { unlisten.then(fn => fn()); };
  }, [loadTrades]);

  const deleteTrade = async () => {
    if (!confirmDel) return;
    await invoke("delete_trade", { id: confirmDel.id }).catch(() => {});
    setConfirmDel(null);
    loadTrades();
  };

  const visible = trades.filter(t =>
    !filter || t.item_name.toLowerCase().includes(filter.toLowerCase()) ||
    t.with_player.toLowerCase().includes(filter.toLowerCase())
  );

  const totalSold   = trades.filter(t => t.direction === "sold").reduce((s, t) => s + t.platinum * t.quantity, 0);
  const totalBought = trades.filter(t => t.direction === "bought").reduce((s, t) => s + t.platinum * t.quantity, 0);
  const net         = totalSold - totalBought;

  return (
    <div className="stat-trades">
      {/* Summary cards */}
      <div className="stat-summary">
        <div className="stat-card stat-card-sold">
          <div className="stat-card-label">Total Earned</div>
          <div className="stat-card-value">+{fmt(totalSold)}p</div>
          <div className="stat-card-sub">{trades.filter(t => t.direction === "sold").length} sales</div>
        </div>
        <div className="stat-card stat-card-bought">
          <div className="stat-card-label">Total Spent</div>
          <div className="stat-card-value">-{fmt(totalBought)}p</div>
          <div className="stat-card-sub">{trades.filter(t => t.direction === "bought").length} purchases</div>
        </div>
        <div className={`stat-card ${net >= 0 ? "stat-card-profit" : "stat-card-loss"}`}>
          <div className="stat-card-label">Net Profit</div>
          <div className="stat-card-value">{net >= 0 ? "+" : ""}{fmt(net)}p</div>
          <div className="stat-card-sub">{trades.length} trades total</div>
        </div>
      </div>

      {/* Chart */}
      <PlatChart trades={trades} />

      {/* Trade list header */}
      <div className="stat-list-header">
        <input className="stat-search" placeholder="Filter by item or player…"
          value={filter} onChange={e => setFilter(e.target.value)} />
        <button className="stat-btn-primary" onClick={() => setShowForm(true)}>+ Log Trade</button>
      </div>

      {/* Add form */}
      {showForm && (
        <AddTradeForm onAdd={() => { setShowForm(false); loadTrades(); }} onCancel={() => setShowForm(false)} />
      )}

      {/* Trade list */}
      {visible.length === 0 ? (
        <div className="stat-empty">
          {trades.length === 0
            ? "No trades logged yet. Trades from warframe.market are logged automatically when you click Sold in the Messages tab. Use the + button to log in-game trades manually."
            : "No trades match your filter."}
        </div>
      ) : (
        <div className="stat-trade-list">
          {visible.map(t => (
            <div key={t.id} className={`stat-trade-row ${t.direction}`}>
              <span className={`stat-dir ${t.direction}`}>{t.direction === "sold" ? "S" : "B"}</span>
              <div className="stat-trade-main">
                <span className="stat-trade-item">{t.item_name}</span>
                {t.with_player && <span className="stat-trade-player">{t.with_player}</span>}
              </div>
              <div className="stat-trade-meta">
                <span className={`stat-trade-plat ${t.direction}`}>
                  {t.direction === "sold" ? "+" : "-"}{fmt(t.platinum * t.quantity)}p
                </span>
                {t.quantity > 1 && <span className="stat-trade-qty">×{t.quantity}</span>}
              </div>
              <span className="stat-trade-date">{fmtDate(t.timestamp)}</span>
              <span className={`stat-source-badge source-${t.source}`}>{t.source}</span>
              <button className="stat-del-btn" onClick={() => setConfirmDel({ id: t.id, name: t.item_name })} title="Delete trade">×</button>
            </div>
          ))}
        </div>
      )}
      {/* In-app delete confirmation */}
      {confirmDel && (
        <div className="stat-confirm-overlay" onClick={() => setConfirmDel(null)}>
          <div className="stat-confirm-card" onClick={e => e.stopPropagation()}>
            <div className="stat-confirm-title">Delete trade?</div>
            <div className="stat-confirm-body">
              Remove <strong>{confirmDel.name}</strong> from your trade history?
              This cannot be undone.
            </div>
            <div className="stat-confirm-actions">
              <button className="stat-btn-danger" onClick={deleteTrade}>Delete</button>
              <button className="stat-btn-secondary" onClick={() => setConfirmDel(null)}>Cancel</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

// ── Reports tab ───────────────────────────────────────────────────────────────

type DateRange = "today" | "week" | "month" | "quarter" | "ytd" | "year" | "all";

function filterByRange(trades: Trade[], range: DateRange): Trade[] {
  if (range === "all") return trades;
  const now = new Date();
  let cutoff: Date;
  switch (range) {
    case "today":   cutoff = new Date(now); cutoff.setHours(0, 0, 0, 0); break;
    case "week":    cutoff = new Date(now.getTime() - 7 * 86400000); break;
    case "month":   cutoff = new Date(now.getTime() - 30 * 86400000); break;
    case "quarter": cutoff = new Date(now.getTime() - 90 * 86400000); break;
    case "ytd":     cutoff = new Date(now.getFullYear(), 0, 1); break;
    case "year":    cutoff = new Date(now.getTime() - 365 * 86400000); break;
    default:        return trades;
  }
  return trades.filter(t => new Date(t.timestamp) >= cutoff);
}

function ReportsTab() {
  const [allTrades, setAllTrades] = useState<Trade[]>([]);
  const [range, setRange]         = useState<DateRange>("all");

  useEffect(() => {
    invoke<Trade[]>("get_trades").then(setAllTrades).catch(() => {});
  }, []);

  const trades = filterByRange(allTrades, range);

  const sold   = trades.filter(t => t.direction === "sold");
  const bought = trades.filter(t => t.direction === "bought");
  const totalEarned  = sold.reduce((s, t) => s + t.platinum * t.quantity, 0);
  const totalSpent   = bought.reduce((s, t) => s + t.platinum * t.quantity, 0);
  const net          = totalEarned - totalSpent;

  // ── Per-item analytics ───────────────────────────────────────────────────
  const itemMap = new Map<string, { sold: number; bought: number; platEarned: number; platSpent: number; prices: number[] }>();
  trades.forEach(t => {
    const key = t.item_name;
    const cur = itemMap.get(key) ?? { sold: 0, bought: 0, platEarned: 0, platSpent: 0, prices: [] };
    if (t.direction === "sold") {
      cur.sold     += t.quantity;
      cur.platEarned += t.platinum * t.quantity;
      cur.prices.push(t.platinum);
    } else {
      cur.bought   += t.quantity;
      cur.platSpent += t.platinum * t.quantity;
    }
    itemMap.set(key, cur);
  });
  const itemRows = [...itemMap.entries()].map(([name, d]) => ({
    name,
    sold:      d.sold,
    bought:    d.bought,
    platEarned: d.platEarned,
    platSpent:  d.platSpent,
    net:       d.platEarned - d.platSpent,
    avgPrice:  d.prices.length ? Math.round(d.prices.reduce((a, b) => a + b, 0) / d.prices.length) : 0,
  })).sort((a, b) => b.net - a.net);

  // ── Per-partner analytics ────────────────────────────────────────────────
  const partnerMap = new Map<string, { trades: number; platExchanged: number }>();
  trades.filter(t => t.with_player).forEach(t => {
    const cur = partnerMap.get(t.with_player) ?? { trades: 0, platExchanged: 0 };
    cur.trades += 1;
    cur.platExchanged += t.platinum * t.quantity;
    partnerMap.set(t.with_player, cur);
  });
  const partnerRows = [...partnerMap.entries()]
    .map(([player, d]) => ({ player, ...d }))
    .sort((a, b) => b.platExchanged - a.platExchanged)
    .slice(0, 10);

  // ── Daily platinum chart ─────────────────────────────────────────────────
  const dailyMap = new Map<string, number>();
  [...trades].reverse().forEach(t => {
    const day = t.timestamp.slice(0, 10);
    const delta = t.direction === "sold" ? t.platinum * t.quantity : -t.platinum * t.quantity;
    dailyMap.set(day, (dailyMap.get(day) ?? 0) + delta);
  });
  const dailyPoints = [...dailyMap.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  let running = 0;
  const chartData: { d: string; v: number; daily: number }[] = dailyPoints.map(([d, delta]) => {
    running += delta;
    return { d, v: running, daily: delta };
  });

  const renderChart = () => {
    if (chartData.length < 2) return null;
    const vals = chartData.map(p => p.v);
    const W = 400, H = 100;
    const min = Math.min(...vals) * 1.05;
    const max = Math.max(...vals) * 1.05;
    const range2 = max - min || 1;
    const px = (i: number) => (i / (chartData.length - 1)) * W;
    const py = (v: number) => H - ((v - min) / range2) * (H - 6) - 3;
    const polyline = chartData.map((p, i) => `${px(i)},${py(p.v)}`).join(" ");
    const area = `0,${H} ` + polyline + ` ${W},${H}`;
    const last = vals[vals.length - 1];
    return (
      <div className="stat-chart-wrap">
        <div className="stat-chart-label">Cumulative platinum ({chartData[0]?.d} → {chartData[chartData.length-1]?.d})</div>
        <svg viewBox={`0 0 ${W} ${H}`} className="stat-chart" style={{ height: 100 }} preserveAspectRatio="none">
          <defs>
            <linearGradient id="rg" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor={last >= 0 ? "var(--green)" : "var(--red)"} stopOpacity=".25"/>
              <stop offset="100%" stopColor={last >= 0 ? "var(--green)" : "var(--red)"} stopOpacity="0"/>
            </linearGradient>
          </defs>
          <polygon points={area} fill="url(#rg)" />
          <polyline points={polyline} fill="none"
            stroke={last >= 0 ? "var(--green)" : "var(--red)"}
            strokeWidth="2" strokeLinejoin="round" />
        </svg>
        <div className="stat-chart-footer">
          <span>{chartData[0]?.d}</span>
          <span style={{ color: last >= 0 ? "var(--green)" : "var(--red)", fontWeight: 700 }}>
            {last >= 0 ? "+" : ""}{fmt(last)}p net
          </span>
          <span>{chartData[chartData.length-1]?.d}</span>
        </div>
      </div>
    );
  };

  const RANGES: { key: DateRange; label: string }[] = [
    { key: "today",   label: "Today" },
    { key: "week",    label: "7 Days" },
    { key: "month",   label: "30 Days" },
    { key: "quarter", label: "90 Days" },
    { key: "ytd",     label: "YTD" },
    { key: "year",    label: "1 Year" },
    { key: "all",     label: "All Time" },
  ];

  return (
    <div className="stat-trades">
      {/* Date range */}
      <div className="stat-range-bar">
        {RANGES.map(r => (
          <button key={r.key} className={`stat-range-btn${range === r.key ? " active" : ""}`}
            onClick={() => setRange(r.key)}>{r.label}</button>
        ))}
        <span className="stat-range-count">{trades.length} trades</span>
      </div>

      {/* KPI cards */}
      <div className="stat-summary">
        <div className="stat-card stat-card-sold">
          <div className="stat-card-label">Earned</div>
          <div className="stat-card-value">+{fmt(totalEarned)}p</div>
          <div className="stat-card-sub">{sold.length} sales</div>
        </div>
        <div className="stat-card stat-card-bought">
          <div className="stat-card-label">Spent</div>
          <div className="stat-card-value">-{fmt(totalSpent)}p</div>
          <div className="stat-card-sub">{bought.length} purchases</div>
        </div>
        <div className={`stat-card ${net >= 0 ? "stat-card-profit" : "stat-card-loss"}`}>
          <div className="stat-card-label">Net Profit</div>
          <div className="stat-card-value">{net >= 0 ? "+" : ""}{fmt(net)}p</div>
          <div className="stat-card-sub">avg {trades.length ? fmt(Math.round(Math.abs(net) / trades.length)) : 0}p/trade</div>
        </div>
      </div>

      {/* Cumulative platinum line chart */}
      {renderChart()}

      {/* Daily bar chart */}
      {chartData.length >= 2 && (() => {
        const W = 400, H = 70;
        const maxAbs = Math.max(...chartData.map(p => Math.abs(p.daily)), 1);
        const barW = Math.max(2, Math.floor(W / chartData.length) - 1);
        return (
          <div className="stat-chart-wrap">
            <div className="stat-chart-label">Daily platinum (earned / spent)</div>
            <svg viewBox={`0 0 ${W} ${H}`} className="stat-chart" style={{ height: 70 }} preserveAspectRatio="none">
              {chartData.map((p, i) => {
                const x = (i / chartData.length) * W;
                const h = Math.max(1, (Math.abs(p.daily) / maxAbs) * (H / 2 - 2));
                const positive = p.daily >= 0;
                return (
                  <rect key={i} x={x} y={positive ? H / 2 - h : H / 2}
                    width={barW} height={h}
                    fill={positive ? "var(--green)" : "var(--red)"} opacity={0.7} />
                );
              })}
              <line x1="0" y1={H / 2} x2={W} y2={H / 2} stroke="rgba(255,255,255,.15)" strokeWidth="0.5" />
            </svg>
            <div className="stat-chart-footer">
              <span>{chartData[0]?.d}</span>
              <span>green = earned · red = spent</span>
              <span>{chartData[chartData.length - 1]?.d}</span>
            </div>
          </div>
        );
      })()}

      {/* Items breakdown */}
      {itemRows.length > 0 && (
        <>
          <div className="stat-section-label">Items breakdown</div>
          <div className="stat-table">
            <div className="stat-table-head">
              <span>Item</span>
              <span>Sold</span>
              <span>Bought</span>
              <span>Avg price</span>
              <span>Net plat</span>
            </div>
            {itemRows.map(r => (
              <div key={r.name} className="stat-table-row">
                <span className="stat-table-name">{r.name}</span>
                <span className="stat-table-num sold-num">{r.sold > 0 ? `×${r.sold}` : "—"}</span>
                <span className="stat-table-num bought-num">{r.bought > 0 ? `×${r.bought}` : "—"}</span>
                <span className="stat-table-num">{r.avgPrice > 0 ? `${fmt(r.avgPrice)}p` : "—"}</span>
                <span className={`stat-table-num ${r.net >= 0 ? "sold-num" : "bought-num"}`}>
                  {r.net >= 0 ? "+" : ""}{fmt(r.net)}p
                </span>
              </div>
            ))}
          </div>
        </>
      )}

      {/* Top partners */}
      {partnerRows.length > 0 && (
        <>
          <div className="stat-section-label">Top trading partners</div>
          <div className="stat-table stat-table-3col">
            <div className="stat-table-head">
              <span>Player</span>
              <span>Trades</span>
              <span>Plat exchanged</span>
            </div>
            {partnerRows.map(r => (
              <div key={r.player} className="stat-table-row">
                <span className="stat-table-name">{r.player}</span>
                <span className="stat-table-num">{r.trades}</span>
                <span className="stat-table-num">{fmt(r.platExchanged)}p</span>
              </div>
            ))}
          </div>
        </>
      )}

      {trades.length === 0 && (
        <div className="stat-empty">No trades in the selected period.</div>
      )}
    </div>
  );
}

// ── Main Statistics component ─────────────────────────────────────────────────

export default function Statistics() {
  const [subTab, setSubTab] = useState<"trades" | "reports">("trades");

  return (
    <div className="statistics">
      <div className="stat-sub-tabs">
        <button className={subTab === "trades" ? "active" : ""} onClick={() => setSubTab("trades")}>Trades</button>
        <button className={subTab === "reports" ? "active" : ""} onClick={() => setSubTab("reports")}>Reports</button>
      </div>
      {subTab === "trades"  && <TradesTab />}
      {subTab === "reports" && <ReportsTab />}
    </div>
  );
}
