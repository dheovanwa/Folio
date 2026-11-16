use std::path::PathBuf;

use rusqlite::{Connection, params};

/// Represents a saved article in the library.
#[derive(Debug, Clone)]
pub struct SavedArticle {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub byline: String,
    pub html_content: String,
    pub text_content: String,
    pub reading_time_mins: i32,
    pub scroll_position: f64,
    pub saved_at: String,
    pub last_read_at: Option<String>,
}

pub struct Storage {
    conn: Connection,
}

impl Storage {
    /// Open (or create) the database at `~/.local/share/folio/library.db`.
    pub fn open() -> Result<Self, rusqlite::Error> {
        let db_path = Self::db_path();
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA cache_size=-2000;
             PRAGMA temp_store=MEMORY;
             PRAGMA mmap_size=0;",
        )?;
        let storage = Self { conn };
        storage.init_tables()?;
        Ok(storage)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        let storage = Self { conn };
        storage.init_tables()?;
        Ok(storage)
    }

    fn db_path() -> PathBuf {
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("~/.local/share"))
            .join("folio");
        data_dir.join("library.db")
    }

    fn init_tables(&self) -> Result<(), rusqlite::Error> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS articles (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                url             TEXT UNIQUE NOT NULL,
                title           TEXT NOT NULL DEFAULT '',
                byline          TEXT NOT NULL DEFAULT '',
                html_content    TEXT NOT NULL DEFAULT '',
                text_content    TEXT NOT NULL DEFAULT '',
                reading_time_mins INTEGER NOT NULL DEFAULT 0,
                scroll_position REAL NOT NULL DEFAULT 0.0,
                saved_at        TEXT NOT NULL DEFAULT (datetime('now')),
                last_read_at    TEXT
            );

            CREATE TABLE IF NOT EXISTS reading_history (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                url             TEXT NOT NULL,
                title           TEXT NOT NULL DEFAULT '',
                opened_at       TEXT NOT NULL DEFAULT (datetime('now'))
            );"
        )?;
        Ok(())
    }

    /// Save or update an article in the library.
    pub fn save_article(
        &self,
        url: &str,
        title: &str,
        byline: &str,
        html_content: &str,
        text_content: &str,
        reading_time_mins: i32,
    ) -> Result<i64, rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO articles (url, title, byline, html_content, text_content, reading_time_mins)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(url) DO UPDATE SET
                title = excluded.title,
                byline = excluded.byline,
                html_content = excluded.html_content,
                text_content = excluded.text_content,
                reading_time_mins = excluded.reading_time_mins,
                saved_at = datetime('now')",
            params![url, title, byline, html_content, text_content, reading_time_mins],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Update the scroll position for an article by URL.
    pub fn update_scroll_position(&self, url: &str, position: f64) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE articles SET scroll_position = ?1, last_read_at = datetime('now') WHERE url = ?2",
            params![position, url],
        )?;
        Ok(())
    }

    /// Get the saved scroll position for a URL.
    pub fn get_scroll_position(&self, url: &str) -> Result<f64, rusqlite::Error> {
        self.conn.query_row(
            "SELECT scroll_position FROM articles WHERE url = ?1",
            params![url],
            |row| row.get(0),
        )
    }

    /// Get a cached article by URL.
    pub fn get_article(&self, url: &str) -> Result<Option<SavedArticle>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, url, title, byline, html_content, text_content, reading_time_mins,
                    scroll_position, saved_at, last_read_at
             FROM articles WHERE url = ?1",
        )?;
        let mut rows = stmt.query_map(params![url], |row| {
            Ok(SavedArticle {
                id: row.get(0)?,
                url: row.get(1)?,
                title: row.get(2)?,
                byline: row.get(3)?,
                html_content: row.get(4)?,
                text_content: row.get(5)?,
                reading_time_mins: row.get(6)?,
                scroll_position: row.get(7)?,
                saved_at: row.get(8)?,
                last_read_at: row.get(9)?,
            })
        })?;
        match rows.next() {
            Some(Ok(article)) => Ok(Some(article)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    /// List all saved articles, most recent first.
    pub fn list_articles(&self) -> Result<Vec<SavedArticle>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, url, title, byline, html_content, text_content, reading_time_mins,
                    scroll_position, saved_at, last_read_at
             FROM articles ORDER BY saved_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SavedArticle {
                id: row.get(0)?,
                url: row.get(1)?,
                title: row.get(2)?,
                byline: row.get(3)?,
                html_content: row.get(4)?,
                text_content: row.get(5)?,
                reading_time_mins: row.get(6)?,
                scroll_position: row.get(7)?,
                saved_at: row.get(8)?,
                last_read_at: row.get(9)?,
            })
        })?;
        rows.collect()
    }

    /// Delete an article by ID.
    pub fn delete_article(&self, id: i64) -> Result<(), rusqlite::Error> {
        self.conn.execute("DELETE FROM articles WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Record a URL open in reading history.
    pub fn record_history(&self, url: &str, title: &str) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO reading_history (url, title) VALUES (?1, ?2)",
            params![url, title],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_save_and_get() {
        let storage = Storage::open_in_memory().expect("in-memory db opens");

        storage
            .save_article(
                "https://example.com/art1",
                "Rust & GTK4",
                "Author Name",
                "<p>Hello world</p>",
                "Hello world",
                5,
            )
            .expect("save article succeeds");

        let article = storage
            .get_article("https://example.com/art1")
            .expect("get article query succeeds")
            .expect("article exists");

        assert_eq!(article.url, "https://example.com/art1");
        assert_eq!(article.title, "Rust & GTK4");
        assert_eq!(article.byline, "Author Name");
        assert_eq!(article.reading_time_mins, 5);
        assert_eq!(article.text_content, "Hello world");
        assert_eq!(article.scroll_position, 0.0);
        assert!(article.last_read_at.is_none());
    }

    #[test]
    fn test_storage_scroll_position() {
        let storage = Storage::open_in_memory().expect("in-memory db opens");

        storage
            .save_article("https://example.com/art2", "Title 2", "", "", "Body", 2)
            .unwrap();

        storage
            .update_scroll_position("https://example.com/art2", 345.5)
            .unwrap();

        let pos = storage
            .get_scroll_position("https://example.com/art2")
            .unwrap();
        assert!((pos - 345.5).abs() < 0.01);
    }

    #[test]
    fn test_storage_list_and_delete() {
        let storage = Storage::open_in_memory().expect("in-memory db opens");

        let id1 = storage
            .save_article("https://example.com/1", "Article 1", "", "", "", 1)
            .unwrap();
        let _id2 = storage
            .save_article("https://example.com/2", "Article 2", "", "", "", 2)
            .unwrap();

        let list = storage.list_articles().unwrap();
        assert_eq!(list.len(), 2);

        storage.delete_article(id1).unwrap();

        let list_after = storage.list_articles().unwrap();
        assert_eq!(list_after.len(), 1);
        assert_eq!(list_after[0].url, "https://example.com/2");
    }

    #[test]
    fn test_storage_reading_history() {
        let storage = Storage::open_in_memory().expect("in-memory db opens");

        storage
            .record_history("https://example.com/1", "Test")
            .unwrap();
    }
}
