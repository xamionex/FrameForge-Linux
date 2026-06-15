import Reports from "./Reports";
import ItemReport from "./ItemReport";
import "./Statistics.css";

interface Props {
  tab: "trade" | "item";
  onTabChange: (t: "trade" | "item") => void;
  dateRange: number | "all";
  onDateRangeChange: (r: number | "all") => void;
}

export default function Statistics({ tab, onTabChange, dateRange, onDateRangeChange }: Props) {
  return (
    <div className="statistics">
      <div className="stat-sub-tabs">
        <button className={tab === "trade" ? "active" : ""} onClick={() => onTabChange("trade")}>
          Trade Report
        </button>
        <button className={tab === "item" ? "active" : ""} onClick={() => onTabChange("item")}>
          Item Report
        </button>
      </div>
      {tab === "trade" ? <Reports dateRange={dateRange} onDateRangeChange={onDateRangeChange} /> : <ItemReport />}
    </div>
  );
}
