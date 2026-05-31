use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct QuantityChange {
    pub id: i64,
    pub unique_name: String,
    pub item_name: String,
    pub old_qty: i64,
    pub new_qty: i64,
    pub delta: i64,
    pub timestamp: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Trade {
    pub id: i64,
    pub timestamp: String,      // ISO-8601
    pub with_player: String,
    pub direction: String,      // "sold" | "bought"
    pub item_name: String,
    pub item_url: String,       // WFM slug (for price lookup), may be empty
    pub quantity: i64,
    pub platinum: i64,
    pub source: String,         // "wfm" | "ingame" | "manual"
    pub notes: String,
}

pub fn init_db(db_path: &PathBuf) -> Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;

        CREATE TABLE IF NOT EXISTS quantity_changes (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            unique_name TEXT    NOT NULL,
            item_name   TEXT    NOT NULL,
            old_qty     INTEGER NOT NULL,
            new_qty     INTEGER NOT NULL,
            delta       INTEGER NOT NULL,
            timestamp   INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS trades (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp   TEXT    NOT NULL,
            with_player TEXT    NOT NULL DEFAULT '',
            direction   TEXT    NOT NULL DEFAULT 'sold',
            item_name   TEXT    NOT NULL,
            item_url    TEXT    NOT NULL DEFAULT '',
            quantity    INTEGER NOT NULL DEFAULT 1,
            platinum    INTEGER NOT NULL DEFAULT 0,
            source      TEXT    NOT NULL DEFAULT 'manual',
            notes       TEXT    NOT NULL DEFAULT ''
        );

        DELETE FROM quantity_changes;",
    )?;
    Ok(conn)
}

pub fn add_trade(conn: &Connection, trade: &Trade) -> Result<i64> {
    conn.execute(
        "INSERT INTO trades (timestamp, with_player, direction, item_name, item_url, quantity, platinum, source, notes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            trade.timestamp, trade.with_player, trade.direction,
            trade.item_name, trade.item_url, trade.quantity,
            trade.platinum, trade.source, trade.notes,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn get_trades(conn: &Connection) -> Result<Vec<Trade>> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, with_player, direction, item_name, item_url,
                quantity, platinum, source, notes
         FROM trades ORDER BY timestamp DESC",
    )?;
    let rows = stmt.query_map([], |row| Ok(Trade {
        id: row.get(0)?,
        timestamp: row.get(1)?,
        with_player: row.get(2)?,
        direction: row.get(3)?,
        item_name: row.get(4)?,
        item_url: row.get(5)?,
        quantity: row.get(6)?,
        platinum: row.get(7)?,
        source: row.get(8)?,
        notes: row.get(9)?,
    }))?.filter_map(|r| r.ok()).collect();
    Ok(rows)
}

pub fn delete_trade(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM trades WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn add_quantity_change(
    conn: &Connection,
    unique_name: &str,
    item_name: &str,
    old_qty: i64,
    new_qty: i64,
) -> Result<()> {
    let delta = new_qty - old_qty;
    let timestamp = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO quantity_changes (unique_name, item_name, old_qty, new_qty, delta, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![unique_name, item_name, old_qty, new_qty, delta, timestamp],
    )?;
    Ok(())
}

pub fn get_quantity_changes(conn: &Connection, limit: i64) -> Result<Vec<QuantityChange>> {
    let mut stmt = conn.prepare(
        "SELECT id, unique_name, item_name, old_qty, new_qty, delta, timestamp
         FROM quantity_changes
         ORDER BY id DESC
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit], |row| {
            Ok(QuantityChange {
                id: row.get(0)?,
                unique_name: row.get(1)?,
                item_name: row.get(2)?,
                old_qty: row.get(3)?,
                new_qty: row.get(4)?,
                delta: row.get(5)?,
                timestamp: row.get(6)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}
