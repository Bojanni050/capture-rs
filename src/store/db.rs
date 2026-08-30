//! SQLite-opslag met FTS5 voor full-text zoeken.
//!
//! Eén verbinding achter een mutex. Alle aanroepen zijn blokkerend en worden
//! vanuit async code via `spawn_blocking` gedaan; bij vier samples per seconde
//! is dat ruim voldoende en het scheelt een connection pool.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;
use std::sync::Mutex;

const SCHEMA_VERSION: i32 = 2;

pub struct Db {
    conn: Mutex<Connection>,
}

/// Eén opgeslagen waarneming.
#[derive(Debug, Clone, Serialize)]
pub struct Capture {
    pub id: i64,
    pub ts: i64,
    pub app: String,
    pub title: String,
    pub kind: String,
    /// Welke laag de tekst leverde: "uia", "ocr" of "none".
    pub source: String,
    pub quality: f32,
    pub char_len: i64,
    pub text: String,
    pub has_frame: bool,
    pub fallback_reason: Option<String>,
    pub monitor: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub id: i64,
    pub ts: i64,
    pub app: String,
    pub title: String,
    pub kind: String,
    pub source: String,
    pub snippet: String,
    pub quality: f32,
    pub has_frame: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SegmentRow {
    pub id: i64,
    pub app: String,
    pub title: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub captures: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub captures: i64,
    /// Captures waarvan de tekst uit de accessibility-boom kwam.
    pub uia_captures: i64,
    /// Captures waarvan de tekst uit OCR kwam.
    pub ocr_captures: i64,
    pub text_captures: i64,
    pub image_captures: i64,
    pub segments: i64,
    pub apps: i64,
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
    pub total_chars: i64,
    pub frames_on_disk: i64,
    pub skipped: Vec<(String, i64)>,
    pub top_apps: Vec<(String, i64)>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub text: String,
    pub app: Option<String>,
    pub kind: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub limit: i64,
    pub offset: i64,
}

/// Nieuwe capture, zoals de pipeline hem aanlevert.
pub struct NewCapture<'a> {
    pub segment_id: i64,
    pub ts: i64,
    pub kind: &'a str,
    /// "uia", "ocr" of "none".
    pub source: &'a str,
    pub monitor: &'a str,
    pub phash: u64,
    pub quality: f32,
    pub text: &'a str,
    pub index_text: &'a str,
    pub frame_path: Option<&'a str>,
    pub width: u32,
    pub height: u32,
    pub fallback_reason: Option<&'a str>,
}

#[derive(Debug, Default)]
pub struct PurgeReport {
    pub captures_deleted: i64,
    pub frames_deleted: i64,
    /// Relatieve paden van frames die van schijf mogen.
    pub frame_paths: Vec<String>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("database openen mislukt: {}", path.display()))?;
        Self::from_connection(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        // WAL houdt lezen (de webserver) en schrijven (de pipeline) uit elkaar.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let db = Self {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.lock();
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version >= SCHEMA_VERSION {
            return Ok(());
        }

        if version < 1 {
            Self::migrate_v1(&conn)?;
        }
        if version < 2 {
            Self::migrate_v2(&conn)?;
        }

        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    fn migrate_v1(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS apps (
                id       INTEGER PRIMARY KEY,
                key      TEXT NOT NULL UNIQUE,
                exe      TEXT NOT NULL,
                exe_path TEXT NOT NULL DEFAULT ''
            );

            CREATE TABLE IF NOT EXISTS segments (
                id         INTEGER PRIMARY KEY,
                app_id     INTEGER NOT NULL REFERENCES apps(id),
                title      TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                ended_at   INTEGER NOT NULL,
                captures   INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_segments_started ON segments(started_at);

            CREATE TABLE IF NOT EXISTS captures (
                id              INTEGER PRIMARY KEY,
                segment_id      INTEGER NOT NULL REFERENCES segments(id) ON DELETE CASCADE,
                ts              INTEGER NOT NULL,
                kind            TEXT NOT NULL,
                monitor         TEXT NOT NULL DEFAULT '',
                phash           INTEGER NOT NULL DEFAULT 0,
                quality         REAL NOT NULL DEFAULT 0,
                char_len        INTEGER NOT NULL DEFAULT 0,
                text            TEXT NOT NULL DEFAULT '',
                frame_path      TEXT,
                width           INTEGER NOT NULL DEFAULT 0,
                height          INTEGER NOT NULL DEFAULT 0,
                fallback_reason TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_captures_ts ON captures(ts);
            CREATE INDEX IF NOT EXISTS idx_captures_segment ON captures(segment_id);

            CREATE VIRTUAL TABLE IF NOT EXISTS captures_fts
                USING fts5(text, tokenize='unicode61 remove_diacritics 2');

            CREATE TABLE IF NOT EXISTS app_stats (
                app_id INTEGER PRIMARY KEY REFERENCES apps(id),
                frames INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS boilerplate (
                app_id    INTEGER NOT NULL REFERENCES apps(id),
                line_hash INTEGER NOT NULL,
                line      TEXT NOT NULL,
                hits      INTEGER NOT NULL,
                PRIMARY KEY (app_id, line_hash)
            ) WITHOUT ROWID;

            CREATE TABLE IF NOT EXISTS skips (
                day    TEXT NOT NULL,
                reason TEXT NOT NULL,
                count  INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (day, reason)
            ) WITHOUT ROWID;
            "#,
        )
        .context("schema aanmaken mislukt (is FTS5 beschikbaar?)")?;
        Ok(())
    }

    /// v2 voegt `source` toe: welke laag de tekst leverde.
    ///
    /// Bestaande rijen komen uit de tijd dat OCR de enige bron was, dus die
    /// krijgen terecht 'ocr' als default.
    fn migrate_v2(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "ALTER TABLE captures ADD COLUMN source TEXT NOT NULL DEFAULT 'ocr';
             CREATE INDEX IF NOT EXISTS idx_captures_source ON captures(source);",
        )
        .context("migratie naar schema v2 mislukt")?;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        // Een vergiftigde mutex betekent dat een andere thread paniekte terwijl
        // hij de verbinding vasthield; doorgaan is veiliger dan hier crashen.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // --- schrijven -------------------------------------------------------

    pub fn app_id(&self, key: &str, exe: &str, exe_path: &str) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO apps(key, exe, exe_path) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET exe = excluded.exe, exe_path = excluded.exe_path",
            params![key, exe, exe_path],
        )?;
        let id = conn.query_row("SELECT id FROM apps WHERE key = ?1", [key], |r| r.get(0))?;
        Ok(id)
    }

    pub fn open_segment(&self, app_id: i64, title: &str, ts: i64) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO segments(app_id, title, started_at, ended_at, captures)
             VALUES (?1, ?2, ?3, ?3, 0)",
            params![app_id, title, ts],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Verlengt een segment zonder een nieuwe capture; gebruikt wanneer het
    /// beeld ongewijzigd bleef.
    pub fn touch_segment(&self, segment_id: i64, ts: i64) -> Result<()> {
        self.lock().execute(
            "UPDATE segments SET ended_at = ?2 WHERE id = ?1",
            params![segment_id, ts],
        )?;
        Ok(())
    }

    pub fn insert_capture(&self, c: NewCapture<'_>) -> Result<i64> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;

        tx.execute(
            "INSERT INTO captures
               (segment_id, ts, kind, source, monitor, phash, quality, char_len, text,
                frame_path, width, height, fallback_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                c.segment_id,
                c.ts,
                c.kind,
                c.source,
                c.monitor,
                c.phash as i64,
                c.quality,
                c.index_text.chars().count() as i64,
                c.text,
                c.frame_path,
                c.width,
                c.height,
                c.fallback_reason,
            ],
        )?;
        let id = tx.last_insert_rowid();

        // Alleen de gefilterde tekst gaat de zoekindex in: zonder menubalken,
        // zonder statusbalk, zonder redacties.
        if !c.index_text.trim().is_empty() {
            tx.execute(
                "INSERT INTO captures_fts(rowid, text) VALUES (?1, ?2)",
                params![id, c.index_text],
            )?;
        }

        tx.execute(
            "UPDATE segments SET ended_at = ?2, captures = captures + 1 WHERE id = ?1",
            params![c.segment_id, c.ts],
        )?;

        tx.commit()?;
        Ok(id)
    }

    /// Telt hoe vaak het filter iets heeft overgeslagen, per dag en reden.
    pub fn record_skip(&self, day: &str, reason: &str) -> Result<()> {
        self.lock().execute(
            "INSERT INTO skips(day, reason, count) VALUES (?1, ?2, 1)
             ON CONFLICT(day, reason) DO UPDATE SET count = count + 1",
            params![day, reason],
        )?;
        Ok(())
    }

    // --- boilerplate-geheugen --------------------------------------------

    pub fn save_boilerplate(
        &self,
        app_id: i64,
        frames: u32,
        lines: &[crate::filter::BoilerplateLine],
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO app_stats(app_id, frames) VALUES (?1, ?2)
             ON CONFLICT(app_id) DO UPDATE SET frames = excluded.frames",
            params![app_id, frames],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO boilerplate(app_id, line_hash, line, hits) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(app_id, line_hash) DO UPDATE SET hits = excluded.hits",
            )?;
            for (hash, line, hits) in lines {
                stmt.execute(params![app_id, *hash as i64, line, hits])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Alle geleerde boilerplate, gegroepeerd per app-sleutel.
    pub fn load_boilerplate(&self) -> Result<Vec<crate::filter::BoilerplateSnapshot>> {
        let conn = self.lock();
        let mut apps = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT a.id, a.key, COALESCE(s.frames, 0)
                 FROM apps a LEFT JOIN app_stats s ON s.app_id = a.id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, u32>(2)?))
            })?;
            for row in rows {
                apps.push(row?);
            }
        }

        let mut out = Vec::new();
        let mut stmt =
            conn.prepare("SELECT line_hash, line, hits FROM boilerplate WHERE app_id = ?1")?;
        for (id, key, frames) in apps {
            let rows = stmt.query_map([id], |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, String>(1)?,
                    r.get::<_, u32>(2)?,
                ))
            })?;
            let mut lines = Vec::new();
            for row in rows {
                lines.push(row?);
            }
            if frames > 0 || !lines.is_empty() {
                out.push((key, frames, lines));
            }
        }
        Ok(out)
    }

    // --- lezen -----------------------------------------------------------

    pub fn search(&self, q: &SearchQuery) -> Result<Vec<SearchHit>> {
        if q.text.trim().is_empty() {
            return self.browse(q);
        }

        let conn = self.lock();
        let mut sql = String::from(
            "SELECT c.id, c.ts, a.key, s.title, c.kind, c.source,
                    snippet(captures_fts, 0, '<<', '>>', '…', 14),
                    c.quality, c.frame_path
             FROM captures_fts
             JOIN captures c ON c.id = captures_fts.rowid
             JOIN segments s ON s.id = c.segment_id
             JOIN apps a     ON a.id = s.app_id
             WHERE captures_fts MATCH ?1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(fts_query(&q.text))];
        push_filters(&mut sql, &mut args, q);
        sql.push_str(" ORDER BY bm25(captures_fts) LIMIT ?");
        args.push(Box::new(q.limit.max(1)));
        sql.push_str(" OFFSET ?");
        args.push(Box::new(q.offset.max(0)));

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())), |r| {
            Ok(SearchHit {
                id: r.get(0)?,
                ts: r.get(1)?,
                app: r.get(2)?,
                title: r.get(3)?,
                kind: r.get(4)?,
                source: r.get(5)?,
                snippet: r.get(6)?,
                quality: r.get(7)?,
                has_frame: r.get::<_, Option<String>>(8)?.is_some(),
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Bladeren zonder zoekterm: gewoon de nieuwste captures.
    fn browse(&self, q: &SearchQuery) -> Result<Vec<SearchHit>> {
        let conn = self.lock();
        let mut sql = String::from(
            "SELECT c.id, c.ts, a.key, s.title, c.kind, c.source, substr(c.text, 1, 220),
                    c.quality, c.frame_path
             FROM captures c
             JOIN segments s ON s.id = c.segment_id
             JOIN apps a     ON a.id = s.app_id
             WHERE 1 = 1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        push_filters(&mut sql, &mut args, q);
        sql.push_str(" ORDER BY c.ts DESC LIMIT ?");
        args.push(Box::new(q.limit.max(1)));
        sql.push_str(" OFFSET ?");
        args.push(Box::new(q.offset.max(0)));

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())), |r| {
            Ok(SearchHit {
                id: r.get(0)?,
                ts: r.get(1)?,
                app: r.get(2)?,
                title: r.get(3)?,
                kind: r.get(4)?,
                source: r.get(5)?,
                snippet: r.get(6)?,
                quality: r.get(7)?,
                has_frame: r.get::<_, Option<String>>(8)?.is_some(),
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn capture(&self, id: i64) -> Result<Option<Capture>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT c.id, c.ts, a.key, s.title, c.kind, c.source, c.quality, c.char_len,
                        c.text, c.frame_path, c.fallback_reason, c.monitor
                 FROM captures c
                 JOIN segments s ON s.id = c.segment_id
                 JOIN apps a     ON a.id = s.app_id
                 WHERE c.id = ?1",
                [id],
                |r| {
                    Ok(Capture {
                        id: r.get(0)?,
                        ts: r.get(1)?,
                        app: r.get(2)?,
                        title: r.get(3)?,
                        kind: r.get(4)?,
                        source: r.get(5)?,
                        quality: r.get(6)?,
                        char_len: r.get(7)?,
                        text: r.get(8)?,
                        has_frame: r.get::<_, Option<String>>(9)?.is_some(),
                        fallback_reason: r.get(10)?,
                        monitor: r.get(11)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Het pad op schijf van het frame bij een capture.
    pub fn frame_path(&self, id: i64) -> Result<Option<String>> {
        let conn = self.lock();
        let row = conn
            .query_row("SELECT frame_path FROM captures WHERE id = ?1", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?;
        Ok(row.flatten())
    }

    pub fn timeline(&self, from: i64, to: i64) -> Result<Vec<SegmentRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT s.id, a.key, s.title, s.started_at, s.ended_at, s.captures
             FROM segments s JOIN apps a ON a.id = s.app_id
             WHERE s.ended_at >= ?1 AND s.started_at <= ?2
             ORDER BY s.started_at ASC",
        )?;
        let rows = stmt.query_map([from, to], |r| {
            Ok(SegmentRow {
                id: r.get(0)?,
                app: r.get(1)?,
                title: r.get(2)?,
                started_at: r.get(3)?,
                ended_at: r.get(4)?,
                captures: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn stats(&self, from: i64, to: i64) -> Result<Stats> {
        let conn = self.lock();
        let mut stats = Stats::default();

        conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(kind != 'image'), 0),
                    COALESCE(SUM(kind = 'image'), 0),
                    COALESCE(SUM(source = 'uia'), 0),
                    COALESCE(SUM(source = 'ocr'), 0),
                    COALESCE(SUM(char_len), 0),
                    COALESCE(SUM(frame_path IS NOT NULL), 0),
                    MIN(ts), MAX(ts)
             FROM captures WHERE ts BETWEEN ?1 AND ?2",
            [from, to],
            |r| {
                stats.captures = r.get(0)?;
                stats.text_captures = r.get(1)?;
                stats.image_captures = r.get(2)?;
                stats.uia_captures = r.get(3)?;
                stats.ocr_captures = r.get(4)?;
                stats.total_chars = r.get(5)?;
                stats.frames_on_disk = r.get(6)?;
                stats.first_ts = r.get(7)?;
                stats.last_ts = r.get(8)?;
                Ok(())
            },
        )?;

        stats.segments = conn.query_row(
            "SELECT COUNT(*) FROM segments WHERE ended_at >= ?1 AND started_at <= ?2",
            [from, to],
            |r| r.get(0),
        )?;
        stats.apps = conn.query_row("SELECT COUNT(*) FROM apps", [], |r| r.get(0))?;

        {
            let mut stmt = conn.prepare(
                "SELECT a.key, COUNT(*) c
                 FROM captures cp
                 JOIN segments s ON s.id = cp.segment_id
                 JOIN apps a     ON a.id = s.app_id
                 WHERE cp.ts BETWEEN ?1 AND ?2
                 GROUP BY a.key ORDER BY c DESC LIMIT 12",
            )?;
            let rows = stmt.query_map([from, to], |r| Ok((r.get(0)?, r.get(1)?)))?;
            for row in rows {
                stats.top_apps.push(row?);
            }
        }

        {
            let mut stmt = conn.prepare(
                "SELECT reason, SUM(count) FROM skips GROUP BY reason ORDER BY 2 DESC",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            for row in rows {
                stats.skipped.push(row?);
            }
        }

        Ok(stats)
    }

    pub fn apps(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT key FROM apps ORDER BY key")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    // --- opruimen ---------------------------------------------------------

    /// Verwijdert oude captures en meldt welke frames van schijf mogen.
    ///
    /// `frames_before` verwijdert alléén de afbeelding en laat de tekst staan;
    /// `captures_before` verwijdert de hele waarneming.
    pub fn purge(&self, captures_before: Option<i64>, frames_before: Option<i64>) -> Result<PurgeReport> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let mut report = PurgeReport::default();
        let mut seen = std::collections::HashSet::new();

        if let Some(cutoff) = captures_before {
            {
                let mut stmt = tx.prepare(
                    "SELECT frame_path FROM captures WHERE ts < ?1 AND frame_path IS NOT NULL",
                )?;
                let rows = stmt.query_map([cutoff], |r| r.get::<_, String>(0))?;
                for row in rows {
                    let p = row?;
                    if seen.insert(p.clone()) {
                        report.frame_paths.push(p);
                    }
                }
            }
            tx.execute(
                "DELETE FROM captures_fts WHERE rowid IN (SELECT id FROM captures WHERE ts < ?1)",
                [cutoff],
            )?;
            report.captures_deleted = tx.execute("DELETE FROM captures WHERE ts < ?1", [cutoff])? as i64;
            tx.execute(
                "DELETE FROM segments WHERE ended_at < ?1
                 AND id NOT IN (SELECT DISTINCT segment_id FROM captures)",
                [cutoff],
            )?;
        }

        if let Some(cutoff) = frames_before {
            {
                let mut stmt = tx.prepare(
                    "SELECT frame_path FROM captures WHERE ts < ?1 AND frame_path IS NOT NULL",
                )?;
                let rows = stmt.query_map([cutoff], |r| r.get::<_, String>(0))?;
                for row in rows {
                    let p = row?;
                    if seen.insert(p.clone()) {
                        report.frame_paths.push(p);
                    }
                }
            }
            report.frames_deleted = tx.execute(
                "UPDATE captures SET frame_path = NULL WHERE ts < ?1 AND frame_path IS NOT NULL",
                [cutoff],
            )? as i64;
        }

        tx.commit()?;
        Ok(report)
    }

    /// Telt wat `purge` zou opruimen, zonder iets te verwijderen.
    pub fn purge_preview(
        &self,
        captures_before: Option<i64>,
        frames_before: Option<i64>,
    ) -> Result<(i64, i64)> {
        let conn = self.lock();
        let captures = match captures_before {
            Some(cutoff) => {
                conn.query_row("SELECT COUNT(*) FROM captures WHERE ts < ?1", [cutoff], |r| {
                    r.get(0)
                })?
            }
            None => 0,
        };
        let frames = match frames_before {
            Some(cutoff) => conn.query_row(
                "SELECT COUNT(*) FROM captures WHERE ts < ?1 AND frame_path IS NOT NULL",
                [cutoff],
                |r| r.get(0),
            )?,
            None => 0,
        };
        Ok((captures, frames))
    }

    /// Comprimeert de database na een grote opruiming.
    pub fn vacuum(&self) -> Result<()> {
        self.lock().execute_batch("VACUUM")?;
        Ok(())
    }
}

/// Voegt de gedeelde WHERE-filters toe aan zowel zoeken als bladeren.
fn push_filters(sql: &mut String, args: &mut Vec<Box<dyn rusqlite::ToSql>>, q: &SearchQuery) {
    if let Some(from) = q.from {
        sql.push_str(" AND c.ts >= ?");
        args.push(Box::new(from));
    }
    if let Some(to) = q.to {
        sql.push_str(" AND c.ts <= ?");
        args.push(Box::new(to));
    }
    if let Some(app) = &q.app {
        sql.push_str(" AND a.key = ?");
        args.push(Box::new(app.clone()));
    }
    if let Some(kind) = &q.kind {
        sql.push_str(" AND c.kind = ?");
        args.push(Box::new(kind.clone()));
    }
}

/// Maakt vrije invoer veilig voor FTS5.
///
/// Zonder dit laat een zoekterm als `foo-bar` of `c++` de query klappen op een
/// syntaxfout. Elk woord wordt een geciteerde term; een afsluitende `*` blijft
/// werken als prefix-zoekopdracht.
fn fts_query(input: &str) -> String {
    let mut terms = Vec::new();
    for raw in input.split_whitespace() {
        let (word, prefix) = match raw.strip_suffix('*') {
            Some(stripped) if !stripped.is_empty() => (stripped, true),
            _ => (raw, false),
        };
        let escaped = word.replace('"', "\"\"");
        if escaped.trim().is_empty() {
            continue;
        }
        terms.push(if prefix {
            format!("\"{escaped}\"*")
        } else {
            format!("\"{escaped}\"")
        });
    }
    if terms.is_empty() {
        "\"\"".to_string()
    } else {
        terms.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(db: &Db, ts: i64, app: &str, title: &str, text: &str, kind: &str) -> i64 {
        seed_with_source(db, ts, app, title, text, kind, "ocr")
    }

    fn seed_with_source(
        db: &Db,
        ts: i64,
        app: &str,
        title: &str,
        text: &str,
        kind: &str,
        source: &str,
    ) -> i64 {
        let app_id = db.app_id(app, &format!("{app}.exe"), "").unwrap();
        let seg = db.open_segment(app_id, title, ts).unwrap();
        db.insert_capture(NewCapture {
            segment_id: seg,
            ts,
            kind,
            source,
            monitor: "hoofdscherm",
            phash: 42,
            quality: 0.8,
            text,
            index_text: text,
            frame_path: if kind == "image" { Some("2026/01/01/1.jpg") } else { None },
            width: 1920,
            height: 1080,
            fallback_reason: None,
        })
        .unwrap()
    }

    #[test]
    fn fts5_is_beschikbaar_en_zoekt() {
        let db = Db::open_in_memory().unwrap();
        seed(&db, 1000, "code", "main.rs", "de kwartaalrapportage staat klaar", "text");
        seed(&db, 2000, "chrome", "nieuws", "het weer wordt morgen beter", "text");

        let hits = db
            .search(&SearchQuery {
                text: "kwartaalrapportage".into(),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].app, "code");
        assert!(hits[0].snippet.contains("kwartaalrapportage"), "{:?}", hits[0]);
    }

    #[test]
    fn lastige_tekens_laten_de_query_niet_klappen() {
        let db = Db::open_in_memory().unwrap();
        seed(&db, 1000, "code", "main.rs", "gebruik van c++ en foo-bar hier", "text");

        for term in ["c++", "foo-bar", "\"quoted", "AND", "*"] {
            let hits = db.search(&SearchQuery {
                text: term.into(),
                limit: 10,
                ..Default::default()
            });
            assert!(hits.is_ok(), "term {term:?} gaf een fout: {:?}", hits.err());
        }
    }

    #[test]
    fn prefix_zoeken_werkt() {
        let db = Db::open_in_memory().unwrap();
        seed(&db, 1000, "code", "main.rs", "kwartaalrapportage over verkoop", "text");
        let hits = db
            .search(&SearchQuery {
                text: "kwartaal*".into(),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn filters_op_app_en_soort_werken() {
        let db = Db::open_in_memory().unwrap();
        seed(&db, 1000, "code", "main.rs", "gedeelde term hier", "text");
        seed(&db, 2000, "chrome", "tab", "gedeelde term daar", "image");

        let hits = db
            .search(&SearchQuery {
                text: "gedeelde".into(),
                app: Some("chrome".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].app, "chrome");
        assert!(hits[0].has_frame);
    }

    #[test]
    fn bladeren_zonder_zoekterm_geeft_nieuwste_eerst() {
        let db = Db::open_in_memory().unwrap();
        seed(&db, 1000, "code", "oud", "eerste", "text");
        seed(&db, 5000, "code", "nieuw", "tweede", "text");

        let hits = db
            .search(&SearchQuery {
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].ts, 5000);
    }

    #[test]
    fn purge_verwijdert_oud_en_meldt_frames() {
        let db = Db::open_in_memory().unwrap();
        seed(&db, 1000, "vlc", "film", "", "image");
        seed(&db, 9000, "code", "main.rs", "recent werk", "text");

        let report = db.purge(Some(5000), None).unwrap();
        assert_eq!(report.captures_deleted, 1);
        assert_eq!(report.frame_paths, vec!["2026/01/01/1.jpg".to_string()]);

        let hits = db.search(&SearchQuery { limit: 10, ..Default::default() }).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].app, "code");
    }

    #[test]
    fn boilerplate_overleeft_een_ronde_opslaan_en_laden() {
        let db = Db::open_in_memory().unwrap();
        let app_id = db.app_id("code", "code.exe", "").unwrap();
        db.save_boilerplate(app_id, 120, &[(7u64, "Bestand Bewerken".into(), 118)])
            .unwrap();

        let loaded = db.load_boilerplate().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0, "code");
        assert_eq!(loaded[0].1, 120);
        assert_eq!(loaded[0].2[0].2, 118);
    }

    #[test]
    fn statistieken_tellen_tekst_en_beeld_apart() {
        let db = Db::open_in_memory().unwrap();
        seed(&db, 1000, "code", "main.rs", "werk", "text");
        seed(&db, 2000, "vlc", "film", "", "image");

        let s = db.stats(0, 10_000).unwrap();
        assert_eq!(s.captures, 2);
        assert_eq!(s.text_captures, 1);
        assert_eq!(s.image_captures, 1);
        assert_eq!(s.frames_on_disk, 1);
    }

    #[test]
    fn statistieken_splitsen_uia_en_ocr() {
        let db = Db::open_in_memory().unwrap();
        seed_with_source(&db, 1000, "code", "main.rs", "uit de boom", "text", "uia");
        seed_with_source(&db, 2000, "code", "main.rs", "uit de boom", "text", "uia");
        seed_with_source(&db, 3000, "game", "level", "gelezen pixels", "text", "ocr");
        seed_with_source(&db, 4000, "vlc", "film", "", "image", "none");

        let s = db.stats(0, 10_000).unwrap();
        assert_eq!(s.uia_captures, 2);
        assert_eq!(s.ocr_captures, 1);
        assert_eq!(s.image_captures, 1);
    }

    #[test]
    fn zoekresultaten_dragen_hun_bron_mee() {
        let db = Db::open_in_memory().unwrap();
        seed_with_source(&db, 1000, "code", "main.rs", "declaratie indienen", "text", "uia");

        let hits = db
            .search(&SearchQuery {
                text: "declaratie".into(),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits[0].source, "uia");
        assert_eq!(db.capture(hits[0].id).unwrap().unwrap().source, "uia");
    }
}
