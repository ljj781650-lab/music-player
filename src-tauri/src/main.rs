// src-tauri/src/main.rs
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::{engine::general_purpose, Engine as _};
use lofty::prelude::*; // 引入 AudioFile, TaggedFileExt 等必须的 Trait
use lofty::probe::Probe;
use lofty::tag::Accessor; // 现代版本使用 Accessor 来获取 Title/Artist
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager, command};
use walkdir::WalkDir;

// ─────────────────────────────────────────────
// 数据结构定义
// ─────────────────────────────────────────────

#[derive(serde::Serialize, Clone)]
struct MusicFile {
    name: String,
    path: String,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    duration: Option<u64>,
    cover: Option<String>,
}

// ─────────────────────────────────────────────
// 数据库存储与初始化
// ─────────────────────────────────────────────

fn db_path(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .expect("无法获取 AppData 目录")
        .join("library.db")
}

fn init_db(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS songs (
            path     TEXT    PRIMARY KEY,
            name     TEXT    NOT NULL,
            title    TEXT,
            artist   TEXT,
            album    TEXT,
            duration INTEGER,
            cover    TEXT,
            mtime    INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_artist ON songs(artist);
        CREATE INDEX IF NOT EXISTS idx_album  ON songs(album);
        CREATE INDEX IF NOT EXISTS idx_title  ON songs(title);",
    )
    .expect("数据库初始化失败");
}

// ─────────────────────────────────────────────
// 元数据解析（Lofty 方案）
// ─────────────────────────────────────────────

fn parse_metadata(path: &Path) -> (Option<String>, Option<String>, Option<String>, Option<u64>, Option<String>) {
    let mut title = None;
    let mut artist = None;
    let mut album = None;
    let mut duration = None;
    let mut cover = None;

    // --- 修改后的扫描核心逻辑片段 ---
if let Ok(probe) = Probe::open(path) {
    if let Ok(tagged_file) = probe.read() {
        // 获取时长
        duration = Some(tagged_file.properties().duration().as_secs());

        // 获取标签 (Title, Artist, Album)
        if let Some(tag) = tagged_file.primary_tag().or_else(|| tagged_file.first_tag()) {
            // 使用标准的 Accessor 方法，这能自动处理类型推导
            title = tag.title().map(|s| s.to_string());
            artist = tag.artist().map(|s| s.to_string());
            album = tag.album().map(|s| s.to_string());

            // 获取封面 (保持原样即可，或者参考下方)
            // 获取封面
// 获取封面
if let Some(picture) = tag.pictures().first() {
    let base64_str = general_purpose::STANDARD.encode(picture.data());
    
    // 关键修正：使用 map 或 unwrap_or 来处理 Option
    // 如果有 mime_type 就转成字符串，如果没有就给个默认值 "image/jpeg"
    let mime_type = picture.mime_type()
        .map(|m| m.to_string())
        .unwrap_or_else(|| "image/jpeg".to_string());
    
    cover = Some(format!("data:{};base64,{}", mime_type, base64_str));
}
        }
    }
}

    if title.is_none() {
        title = path.file_stem().map(|s| s.to_string_lossy().to_string());
    }

    (title, artist, album, duration, cover)
}

// ─────────────────────────────────────────────
// 核心逻辑：扫描流程
// ─────────────────────────────────────────────

#[command]
async fn start_scan_process(app: AppHandle, dir_path: String) -> Result<(), String> {
    tokio::spawn(async move {
        let db_file = db_path(&app);
        if let Some(parent) = db_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(&db_file).expect("数据库连接失败");
        init_db(&conn);

        // ① 递归搜索
        let mut disk_paths: Vec<PathBuf> = Vec::new();
        for entry in WalkDir::new(&dir_path).into_iter().filter_map(|e| e.ok()) {
            let p = entry.path().to_path_buf();
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            let supported = ["mp3", "flac", "m4a", "wav", "ogg"];
            if p.is_file() && supported.contains(&ext.as_str()) {
                disk_paths.push(p);
            }
        }

        // ② 增量扫描
        for path in &disk_paths {
            let path_str = path.to_string_lossy().to_string();
            let mtime = path.metadata().ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64).unwrap_or(0);

            type Row = (i64, String, Option<String>, Option<String>, Option<String>, Option<i64>, Option<String>);
            let cached: Option<Row> = conn.query_row(
                "SELECT mtime, name, title, artist, album, duration, cover FROM songs WHERE path = ?1",
                params![path_str],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
            ).ok();

            let file_info = if let Some((c_mtime, name, title, artist, album, dur, cov)) = cached {
                if c_mtime == mtime {
                    MusicFile { name, path: path_str, title, artist, album, duration: dur.map(|d| d as u64), cover: cov }
                } else {
                    let (t, ar, al, d, c) = parse_metadata(path);
                    let n = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                    let _ = conn.execute(
                        "INSERT OR REPLACE INTO songs (path, name, title, artist, album, duration, cover, mtime) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                        params![path_str, n, t, ar, al, d.map(|x| x as i64), c, mtime]
                    );
                    MusicFile { name: n, path: path_str, title: t, artist: ar, album: al, duration: d, cover: c }
                }
            } else {
                let (t, ar, al, d, c) = parse_metadata(path);
                let n = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                let _ = conn.execute(
                    "INSERT INTO songs (path, name, title, artist, album, duration, cover, mtime) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![path_str, n, t, ar, al, d.map(|x| x as i64), c, mtime]
                );
                MusicFile { name: n, path: path_str, title: t, artist: ar, album: al, duration: d, cover: c }
            };
            let _ = app.emit("music-found", file_info);
        }

        // ③ 精确清理
        let disk_set: std::collections::HashSet<String> = disk_paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
        let prefix = if dir_path.ends_with('\\') || dir_path.ends_with('/') {
            format!("{}%", dir_path)
        } else {
            #[cfg(windows)] { format!("{}\\%", dir_path) }
            #[cfg(not(windows))] { format!("{}/%", dir_path) }
        };

        let db_paths: Vec<String> = {
            let mut s = conn.prepare("SELECT path FROM songs WHERE path LIKE ?1").unwrap();
            s.query_map(params![prefix], |r| r.get(0)).unwrap().filter_map(|r| r.ok()).collect()
        };

        for p in db_paths {
            if !disk_set.contains(&p) {
                let _ = conn.execute("DELETE FROM songs WHERE path = ?1", params![p]);
            }
        }
        let _ = app.emit("scan-finished", "done");
    });
    Ok(())
}

#[command]
fn search_songs(app: AppHandle, keyword: String) -> Vec<MusicFile> {
    let conn = Connection::open(db_path(&app)).unwrap();
    let p = format!("%{}%", keyword);
    let mut s = conn.prepare(
        "SELECT path, name, title, artist, album, duration, cover FROM songs 
         WHERE title LIKE ?1 OR artist LIKE ?1 OR album LIKE ?1 
         ORDER BY artist, album, title LIMIT 200"
    ).unwrap();

    s.query_map(params![p], |r| {
        Ok(MusicFile {
            path: r.get(0)?, name: r.get(1)?, title: r.get(2)?,
            artist: r.get(3)?, album: r.get(4)?,
            duration: r.get::<_, Option<i64>>(5)?.map(|d| d as u64),
            cover: r.get(6)?,
        })
    }).unwrap().filter_map(|r| r.ok()).collect()
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .invoke_handler(tauri::generate_handler![
            start_scan_process,
            search_songs
        ])
        .run(tauri::generate_context!())
        .expect("启动失败");
}