// src-tauri/src/main.rs
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::{engine::general_purpose, Engine as _};
use encoding_rs::GBK;
use id3::{Tag, TagLike};
use rusqlite::{Connection, params};
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager, command};
use walkdir::WalkDir;

// ─────────────────────────────────────────────
// 数据结构
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
// 数据库初始化
// ─────────────────────────────────────────────

/// 返回数据库文件路径：%APPDATA%\MusicPlayer\library.db
fn db_path(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .expect("无法获取 AppData 目录")
        .join("library.db")
}

/// 建表 + 建索引（幂等，重复调用安全）
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
    .expect("数据库建表失败");
}

// ─────────────────────────────────────────────
// 元数据解析（ID3 + 乱码修复 + 封面提取）
// ─────────────────────────────────────────────

fn fix_encoding(raw: &str) -> String {
    let all_latin1 = raw.chars().all(|c| (c as u32) < 256);
    if all_latin1 && raw.chars().any(|c| (c as u32) > 127) {
        let bytes: Vec<u8> = raw.chars().map(|c| c as u8).collect();
        let (decoded, _, had_errors) = GBK.decode(&bytes);
        if !had_errors {
            return decoded.into_owned();
        }
    }
    raw.to_string()
}

fn extract_cover(tag: &Tag) -> Option<String> {
    let pictures: Vec<_> = tag.pictures().collect();
    if pictures.is_empty() {
        return None;
    }
    let pic = pictures
        .iter()
        .find(|p| p.picture_type == id3::frame::PictureType::CoverFront)
        .or_else(|| pictures.first())
        .unwrap();
    let b64 = general_purpose::STANDARD.encode(&pic.data);
    Some(format!("data:{};base64,{}", pic.mime_type, b64))
}

/// 计算音频时长（秒）
/// MP3：用 mp3-duration 库逐帧扫描
/// FLAC：从 metaflac 读取 STREAMINFO
fn calc_duration(path: &std::path::Path) -> Option<u64> {
    let ext = path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "mp3" => {
            let d = mp3_duration::from_path(path).ok().map(|d| d.as_secs());
            eprintln!("[DURATION] mp3 {:?} => {:?}", path, d);
            d
        }
        "flac" => {
            metaflac::Tag::read_from_path(path).ok().and_then(|tag| {
                tag.get_streaminfo().map(|info| {
                    if info.sample_rate > 0 {
                        info.total_samples / info.sample_rate as u64
                    } else {
                        0
                    }
                })
            })
        }
        _ => None,
    }
}

fn parse_metadata(
    path: &std::path::Path,
) -> (Option<String>, Option<String>, Option<String>, Option<u64>, Option<String>) {
    // 先算时长（不依赖 ID3 标签）
    let duration = calc_duration(path);
    match Tag::read_from_path(path) {
        Ok(tag) => (
            tag.title().map(|s| fix_encoding(s)),
            tag.artist().map(|s| fix_encoding(s)),
            tag.album().map(|s| fix_encoding(s)),
            duration,
            extract_cover(&tag),
        ),
        Err(_) => (None, None, None, duration, None),
    }
}

// ─────────────────────────────────────────────
// Command 1: 智能扫描（缓存优先 + 增量更新）
// ─────────────────────────────────────────────

#[command]
async fn start_scan_process(app: AppHandle, dir_path: String) -> Result<(), String> {
    tokio::spawn(async move {
        let db_file = db_path(&app);
        if let Some(parent) = db_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(&db_file).expect("打开数据库失败");
        init_db(&conn);

        // ① 收集磁盘上的全部音频文件
        let mut disk_paths: Vec<PathBuf> = Vec::new();
        for entry in WalkDir::new(&dir_path).into_iter().filter_map(|e| e.ok()) {
            let p = entry.path().to_path_buf();
            let ext = p.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if p.is_file() && (ext == "mp3" || ext == "flac") {
                disk_paths.push(p);
            }
        }

        // ② 对每个文件：缓存命中则秒出，否则解析并写库
        for path in &disk_paths {
            let path_str = path.to_string_lossy().to_string();
            let mtime = path
                .metadata().ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            type CacheRow = (i64, String, Option<String>, Option<String>, Option<String>, Option<i64>, Option<String>);
            let cached: Option<CacheRow> = conn.query_row(
                "SELECT mtime, name, title, artist, album, duration, cover
                 FROM songs WHERE path = ?1",
                params![path_str],
                |row| Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?,
                    row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?,
                )),
            ).ok();

            let file_info = if let Some((cached_mtime, name, title, artist, album, duration, cover)) = cached {
                if cached_mtime == mtime {
                    // ✅ 缓存命中，无需读磁盘，直接推送
                    MusicFile {
                        name, path: path_str,
                        title, artist, album,
                        duration: duration.map(|d| d as u64),
                        cover,
                    }
                } else {
                    // 文件有变动，重新解析并更新
                    let (title, artist, album, duration, cover) = parse_metadata(path);
                    let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                    conn.execute(
                        "INSERT OR REPLACE INTO songs
                         (path, name, title, artist, album, duration, cover, mtime)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                        params![path_str, name, title, artist, album,
                                duration.map(|d| d as i64), cover, mtime],
                    ).ok();
                    MusicFile { name, path: path_str, title, artist, album, duration, cover }
                }
            } else {
                // 新文件，解析并插入
                let (title, artist, album, duration, cover) = parse_metadata(path);
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                conn.execute(
                    "INSERT INTO songs
                     (path, name, title, artist, album, duration, cover, mtime)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![path_str, name, title, artist, album,
                            duration.map(|d| d as i64), cover, mtime],
                ).ok();
                MusicFile { name, path: path_str, title, artist, album, duration, cover }
            };

            let _ = app.emit("music-found", file_info);
        }

        // ③ 清理数据库中已被用户删除的文件记录
        let disk_set: std::collections::HashSet<String> = disk_paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let mut stmt = conn
            .prepare("SELECT path FROM songs WHERE path LIKE ?1")
            .unwrap();
        let db_paths: Vec<String> = stmt
            .query_map(params![format!("{}%", dir_path)], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for p in db_paths {
            if !disk_set.contains(&p) {
                conn.execute("DELETE FROM songs WHERE path = ?1", params![p]).ok();
            }
        }

        let _ = app.emit("scan-finished", "done");
    });
    Ok(())
}

// ─────────────────────────────────────────────
// Command 2: 全文搜索（B-Tree 索引，毫秒响应）
// ─────────────────────────────────────────────

#[command]
fn search_songs(app: AppHandle, keyword: String) -> Vec<MusicFile> {
    let db_file = db_path(&app);
    let conn = match Connection::open(&db_file) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let pattern = format!("%{}%", keyword);
    let mut stmt = conn
        .prepare(
            "SELECT path, name, title, artist, album, duration, cover FROM songs
             WHERE title LIKE ?1 OR artist LIKE ?1 OR album LIKE ?1
             ORDER BY artist, album, title
             LIMIT 200",
        )
        .unwrap();

    stmt.query_map(params![pattern], |row| {
        Ok(MusicFile {
            path:     row.get(0)?,
            name:     row.get(1)?,
            title:    row.get(2)?,
            artist:   row.get(3)?,
            album:    row.get(4)?,
            duration: row.get::<_, Option<i64>>(5)?.map(|d| d as u64),
            cover:    row.get(6)?,
        })
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

// ─────────────────────────────────────────────
// main
// ─────────────────────────────────────────────

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