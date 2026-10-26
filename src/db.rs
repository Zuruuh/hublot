use std::{fs, path::Path, time::Duration};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::reference::ImageRef;

pub const MODE_SEMVER: &str = "semver";
pub const MODE_DIGEST: &str = "digest";

pub const STATUS_OK: &str = "ok";
pub const STATUS_UPDATE: &str = "update";
pub const STATUS_ERROR: &str = "error";
pub const STATUS_BASELINE: &str = "baseline";

#[derive(Debug, Clone)]
pub struct ImageRecord {
    pub id: i64,
    pub reference: String,
    pub registry: String,
    pub repository: String,
    pub tag: String,
    pub mode: String,
    pub current_digest: Option<String>,
    pub baseline_digest: Option<String>,
    pub compatible_tag: Option<String>,
    pub latest_tag: Option<String>,
    pub status: String,
    pub last_error: Option<String>,
    pub active: bool,
    pub first_seen: i64,
    pub last_checked: Option<i64>,
    pub last_changed: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct RunRecord {
    pub id: i64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub status: String,
    pub image_count: i64,
    pub error_count: i64,
}

#[derive(Debug, Clone)]
pub struct EventRecord {
    pub id: i64,
    pub image_id: i64,
    pub kind: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub detected_at: i64,
}

#[derive(Debug, Clone)]
pub struct RecentEvent {
    pub reference: String,
    pub kind: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub detected_at: i64,
}

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create database directory {}", parent.display()))?;
    }

    let connection = Connection::open(path)
        .with_context(|| format!("failed to open database {}", path.display()))?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(connection)
}

pub fn init(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            started_at INTEGER NOT NULL,
            finished_at INTEGER,
            status TEXT NOT NULL,
            image_count INTEGER NOT NULL DEFAULT 0,
            error_count INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS images (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            reference TEXT NOT NULL UNIQUE,
            registry TEXT NOT NULL,
            repository TEXT NOT NULL,
            tag TEXT NOT NULL,
            mode TEXT NOT NULL,
            current_digest TEXT,
            baseline_digest TEXT,
            compatible_tag TEXT,
            latest_tag TEXT,
            status TEXT NOT NULL,
            last_error TEXT,
            active INTEGER NOT NULL DEFAULT 1,
            first_seen INTEGER NOT NULL,
            last_checked INTEGER,
            last_changed INTEGER
        );

        CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            image_id INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
            kind TEXT NOT NULL,
            old_value TEXT,
            new_value TEXT,
            detected_at INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_events_image_id ON events(image_id);
        CREATE INDEX IF NOT EXISTS idx_events_detected_at ON events(detected_at);
        "#,
    )?;
    Ok(())
}

pub fn deactivate_all(connection: &Connection) -> Result<()> {
    connection.execute("UPDATE images SET active = 0", [])?;
    Ok(())
}

pub fn upsert_image(
    connection: &Connection,
    image: &ImageRef,
    mode: &str,
    now: i64,
) -> Result<ImageRecord> {
    connection.execute(
        r#"
        INSERT INTO images (
            reference, registry, repository, tag, mode, status, active, first_seen
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7)
        ON CONFLICT(reference) DO UPDATE SET
            registry = excluded.registry,
            repository = excluded.repository,
            tag = excluded.tag,
            mode = excluded.mode,
            active = 1
        "#,
        params![
            image.canonical(),
            image.registry,
            image.repository,
            image.tag,
            mode,
            STATUS_OK,
            now
        ],
    )?;

    get_image_by_reference(connection, &image.canonical())?
        .ok_or_else(|| anyhow::anyhow!("failed to read image after upsert"))
}

pub fn get_image(connection: &Connection, id: i64) -> Result<Option<ImageRecord>> {
    Ok(connection
        .query_row(
            "SELECT * FROM images WHERE id = ?1",
            params![id],
            image_from_row,
        )
        .optional()?)
}

pub fn get_image_by_reference(
    connection: &Connection,
    reference: &str,
) -> Result<Option<ImageRecord>> {
    Ok(connection
        .query_row(
            "SELECT * FROM images WHERE reference = ?1",
            params![reference],
            image_from_row,
        )
        .optional()?)
}

pub fn list_active_images(connection: &Connection) -> Result<Vec<ImageRecord>> {
    let mut statement = connection.prepare(
        r#"
        SELECT * FROM images
        WHERE active = 1
        ORDER BY
            CASE status
                WHEN 'update' THEN 0
                WHEN 'error' THEN 1
                WHEN 'baseline' THEN 2
                ELSE 3
            END,
            reference
        "#,
    )?;
    let images = statement
        .query_map([], image_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(images)
}

pub fn start_run(connection: &Connection, now: i64) -> Result<i64> {
    connection.execute(
        "INSERT INTO runs (started_at, status) VALUES (?1, 'running')",
        params![now],
    )?;
    Ok(connection.last_insert_rowid())
}

pub fn finish_run(
    connection: &Connection,
    id: i64,
    status: &str,
    image_count: i64,
    error_count: i64,
    now: i64,
) -> Result<()> {
    connection.execute(
        r#"
        UPDATE runs
        SET finished_at = ?2, status = ?3, image_count = ?4, error_count = ?5
        WHERE id = ?1
        "#,
        params![id, now, status, image_count, error_count],
    )?;
    Ok(())
}

pub fn last_run(connection: &Connection) -> Result<Option<RunRecord>> {
    Ok(connection
        .query_row("SELECT * FROM runs ORDER BY id DESC LIMIT 1", [], |row| {
            Ok(RunRecord {
                id: row.get("id")?,
                started_at: row.get("started_at")?,
                finished_at: row.get("finished_at")?,
                status: row.get("status")?,
                image_count: row.get("image_count")?,
                error_count: row.get("error_count")?,
            })
        })
        .optional()?)
}

pub fn record_semver(
    connection: &Connection,
    image: &ImageRecord,
    compatible: Option<&str>,
    latest: Option<&str>,
    now: i64,
) -> Result<()> {
    let compatible_changed = compatible.is_some() && compatible != image.compatible_tag.as_deref();
    let latest_changed = latest.is_some() && latest != image.latest_tag.as_deref();
    let status = if compatible.is_some() || latest.is_some() {
        STATUS_UPDATE
    } else {
        STATUS_OK
    };
    let last_changed = if compatible_changed || latest_changed {
        Some(now)
    } else {
        image.last_changed
    };

    connection.execute(
        r#"
        UPDATE images
        SET compatible_tag = ?2,
            latest_tag = ?3,
            status = ?4,
            last_error = NULL,
            last_checked = ?5,
            last_changed = ?6
        WHERE id = ?1
        "#,
        params![image.id, compatible, latest, status, now, last_changed],
    )?;

    if compatible_changed {
        insert_event(
            connection,
            image.id,
            "compatible",
            image.compatible_tag.as_deref(),
            compatible,
            now,
        )?;
    }
    if latest_changed {
        insert_event(
            connection,
            image.id,
            "latest",
            image.latest_tag.as_deref(),
            latest,
            now,
        )?;
    }

    Ok(())
}

pub fn record_digest(
    connection: &Connection,
    image: &ImageRecord,
    digest: &str,
    now: i64,
) -> Result<()> {
    match image.baseline_digest.as_deref() {
        None => {
            connection.execute(
                r#"
                UPDATE images
                SET current_digest = ?2,
                    baseline_digest = ?2,
                    status = ?3,
                    last_error = NULL,
                    last_checked = ?4
                WHERE id = ?1
                "#,
                params![image.id, digest, STATUS_BASELINE, now],
            )?;
        }
        Some(previous) if previous != digest => {
            connection.execute(
                r#"
                UPDATE images
                SET current_digest = ?2,
                    baseline_digest = ?2,
                    status = ?3,
                    last_error = NULL,
                    last_checked = ?4,
                    last_changed = ?4
                WHERE id = ?1
                "#,
                params![image.id, digest, STATUS_UPDATE, now],
            )?;
            insert_event(
                connection,
                image.id,
                "digest",
                Some(previous),
                Some(digest),
                now,
            )?;
        }
        Some(_) => {
            connection.execute(
                r#"
                UPDATE images
                SET current_digest = ?2,
                    status = ?3,
                    last_error = NULL,
                    last_checked = ?4
                WHERE id = ?1
                "#,
                params![image.id, digest, STATUS_OK, now],
            )?;
        }
    }

    Ok(())
}

pub fn record_error(connection: &Connection, image_id: i64, error: &str, now: i64) -> Result<()> {
    connection.execute(
        r#"
        UPDATE images
        SET status = ?2, last_error = ?3, last_checked = ?4
        WHERE id = ?1
        "#,
        params![image_id, STATUS_ERROR, error, now],
    )?;
    Ok(())
}

pub fn list_events(connection: &Connection, image_id: i64, limit: i64) -> Result<Vec<EventRecord>> {
    let mut statement = connection.prepare(
        r#"
        SELECT id, image_id, kind, old_value, new_value, detected_at
        FROM events
        WHERE image_id = ?1
        ORDER BY detected_at DESC, id DESC
        LIMIT ?2
        "#,
    )?;
    let events = statement
        .query_map(params![image_id, limit], |row| {
            Ok(EventRecord {
                id: row.get("id")?,
                image_id: row.get("image_id")?,
                kind: row.get("kind")?,
                old_value: row.get("old_value")?,
                new_value: row.get("new_value")?,
                detected_at: row.get("detected_at")?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(events)
}

pub fn recent_events(connection: &Connection, limit: i64) -> Result<Vec<RecentEvent>> {
    let mut statement = connection.prepare(
        r#"
        SELECT i.reference, e.kind, e.old_value, e.new_value, e.detected_at
        FROM events e
        JOIN images i ON i.id = e.image_id
        WHERE i.active = 1
        ORDER BY e.detected_at DESC, e.id DESC
        LIMIT ?1
        "#,
    )?;
    let events = statement
        .query_map(params![limit], |row| {
            Ok(RecentEvent {
                reference: row.get("reference")?,
                kind: row.get("kind")?,
                old_value: row.get("old_value")?,
                new_value: row.get("new_value")?,
                detected_at: row.get("detected_at")?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(events)
}

fn insert_event(
    connection: &Connection,
    image_id: i64,
    kind: &str,
    old_value: Option<&str>,
    new_value: Option<&str>,
    now: i64,
) -> Result<()> {
    connection.execute(
        r#"
        INSERT INTO events (image_id, kind, old_value, new_value, detected_at)
        VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
        params![image_id, kind, old_value, new_value, now],
    )?;
    Ok(())
}

fn image_from_row(row: &Row<'_>) -> rusqlite::Result<ImageRecord> {
    Ok(ImageRecord {
        id: row.get("id")?,
        reference: row.get("reference")?,
        registry: row.get("registry")?,
        repository: row.get("repository")?,
        tag: row.get("tag")?,
        mode: row.get("mode")?,
        current_digest: row.get("current_digest")?,
        baseline_digest: row.get("baseline_digest")?,
        compatible_tag: row.get("compatible_tag")?,
        latest_tag: row.get("latest_tag")?,
        status: row.get("status")?,
        last_error: row.get("last_error")?,
        active: row.get::<_, i64>("active")? != 0,
        first_seen: row.get("first_seen")?,
        last_checked: row.get("last_checked")?,
        last_changed: row.get("last_changed")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_image() -> ImageRef {
        ImageRef::parse("nginx:1.20.0").unwrap()
    }

    fn setup() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let connection = open(&dir.path().join("state/hublot.db")).unwrap();
        init(&connection).unwrap();
        (dir, connection)
    }

    #[test]
    fn records_baselines_and_digest_changes() {
        let (_dir, connection) = setup();
        let now = 100;
        let image = upsert_image(&connection, &test_image(), MODE_DIGEST, now).unwrap();
        record_digest(&connection, &image, "sha256:one", now).unwrap();

        let image = get_image(&connection, image.id).unwrap().unwrap();
        assert_eq!(image.status, STATUS_BASELINE);
        assert_eq!(image.baseline_digest.as_deref(), Some("sha256:one"));

        record_digest(&connection, &image, "sha256:two", now + 10).unwrap();
        let image = get_image(&connection, image.id).unwrap().unwrap();
        assert_eq!(image.status, STATUS_UPDATE);
        assert_eq!(image.baseline_digest.as_deref(), Some("sha256:two"));
        assert_eq!(image.last_changed, Some(now + 10));

        let events = list_events(&connection, image.id, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "digest");
        assert_eq!(events[0].old_value.as_deref(), Some("sha256:one"));
        assert_eq!(events[0].new_value.as_deref(), Some("sha256:two"));
    }

    #[test]
    fn records_semver_candidate_changes() {
        let (_dir, connection) = setup();
        let now = 200;
        let image = upsert_image(&connection, &test_image(), MODE_SEMVER, now).unwrap();

        record_semver(&connection, &image, Some("1.21.0"), Some("2.0.0"), now).unwrap();
        let image = get_image(&connection, image.id).unwrap().unwrap();
        assert_eq!(image.status, STATUS_UPDATE);
        assert_eq!(image.compatible_tag.as_deref(), Some("1.21.0"));
        assert_eq!(image.latest_tag.as_deref(), Some("2.0.0"));

        let events = list_events(&connection, image.id, 10).unwrap();
        assert_eq!(events.len(), 2);

        record_semver(&connection, &image, Some("1.22.0"), Some("2.0.0"), now + 10).unwrap();
        let events = list_events(&connection, image.id, 10).unwrap();
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn deactivates_images_missing_from_the_current_file() {
        let (_dir, connection) = setup();
        let now = 300;
        let image = upsert_image(&connection, &test_image(), MODE_SEMVER, now).unwrap();
        assert_eq!(list_active_images(&connection).unwrap().len(), 1);

        deactivate_all(&connection).unwrap();
        assert!(list_active_images(&connection).unwrap().is_empty());
        assert!(!get_image(&connection, image.id).unwrap().unwrap().active);
    }
}
