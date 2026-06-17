use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_rusqlite::Connection;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

use crate::config::Config;
use crate::control::protocol::*;
use crate::db::queries;

pub struct PendingUpload {
    pub room_id: i64,
    pub filename: String,
    pub size: i64,
    pub uploaded_by: i64,
    pub storage_path: PathBuf,
    pub bytes_written: i64,
}

pub struct FileHandler {
    db: Arc<Connection>,
    upload_dir: PathBuf,
    max_upload_size: i64,
    /// Gesamt-Speicherlimit des Servers in Byte (0 = unbegrenzt).
    file_limit_bytes: i64,
    pending_uploads: RwLock<HashMap<String, PendingUpload>>,
}

impl FileHandler {
    pub fn new(db: Arc<Connection>, config: &Config) -> Arc<Self> {
        let upload_dir = PathBuf::from(&config.storage.upload_dir);
        std::fs::create_dir_all(&upload_dir).ok();

        Arc::new(Self {
            db,
            upload_dir,
            max_upload_size: config.storage.max_upload_size_mb as i64 * 1024 * 1024,
            file_limit_bytes: config.storage.file_limit_bytes,
            pending_uploads: RwLock::new(HashMap::new()),
        })
    }

    pub async fn start_upload(
        &self,
        user_id: i64,
        req: FileUploadStart,
    ) -> anyhow::Result<FileUploadAck> {
        if req.size > self.max_upload_size {
            anyhow::bail!("File too large (max {} MB)", self.max_upload_size / 1024 / 1024);
        }

        // Gesamt-Speicherlimit des Servers prüfen (Bestand + laufende Uploads + neu).
        if self.file_limit_bytes > 0 {
            let stored = queries::total_storage_bytes(&self.db).await.unwrap_or(0);
            let pending: i64 = self
                .pending_uploads
                .read()
                .await
                .values()
                .map(|u| u.size)
                .sum();
            if stored + pending + req.size > self.file_limit_bytes {
                let gib = self.file_limit_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                anyhow::bail!(
                    "Speicherlimit des Servers erreicht (max {:.1} GB) — bitte Dateien löschen oder Limit erhöhen lassen",
                    gib
                );
            }
        }

        let upload_id = uuid::Uuid::new_v4().to_string();
        let safe_filename = sanitize_filename(&req.filename);
        let storage_path = self.upload_dir.join(format!("{}_{}", upload_id, safe_filename));

        // Create empty file
        tokio::fs::File::create(&storage_path).await?;

        let pending = PendingUpload {
            room_id: req.room_id,
            filename: req.filename,
            size: req.size,
            uploaded_by: user_id,
            storage_path,
            bytes_written: 0,
        };

        self.pending_uploads.write().await.insert(upload_id.clone(), pending);

        Ok(FileUploadAck {
            upload_id,
            success: true,
        })
    }

    pub async fn write_chunk(&self, chunk: FileUploadChunk) -> anyhow::Result<()> {
        let data = BASE64.decode(&chunk.data)?;

        let mut uploads = self.pending_uploads.write().await;
        let upload = uploads.get_mut(&chunk.upload_id)
            .ok_or_else(|| anyhow::anyhow!("Upload not found"))?;

        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .append(true)
            .open(&upload.storage_path)
            .await?;
        file.write_all(&data).await?;
        upload.bytes_written += data.len() as i64;

        Ok(())
    }

    pub async fn complete_upload(&self, req: FileUploadComplete) -> anyhow::Result<i64> {
        let upload = self.pending_uploads.write().await.remove(&req.upload_id)
            .ok_or_else(|| anyhow::anyhow!("Upload not found"))?;

        let file_id = queries::save_room_file(
            &self.db,
            upload.room_id,
            upload.filename,
            upload.storage_path.to_string_lossy().to_string(),
            upload.bytes_written,
            upload.uploaded_by,
        ).await?;

        Ok(file_id)
    }

    pub async fn get_file_list(&self, room_id: i64) -> anyhow::Result<Vec<FileInfo>> {
        let files = queries::get_room_files(&self.db, room_id).await?;
        Ok(files.iter().map(|f| FileInfo {
            id: f.id,
            filename: f.filename.clone(),
            size_bytes: f.size_bytes,
            uploaded_by: f.uploaded_by,
            uploaded_at: f.uploaded_at.clone(),
        }).collect())
    }

    pub async fn download_file(&self, file_id: i64) -> anyhow::Result<(FileInfo, Vec<u8>)> {
        let db_file = queries::get_room_file_by_id(&self.db, file_id).await?
            .ok_or_else(|| anyhow::anyhow!("File not found"))?;

        let data = tokio::fs::read(&db_file.storage_path).await?;

        let info = FileInfo {
            id: db_file.id,
            filename: db_file.filename,
            size_bytes: db_file.size_bytes,
            uploaded_by: db_file.uploaded_by,
            uploaded_at: db_file.uploaded_at,
        };

        Ok((info, data))
    }

    pub async fn delete_file(&self, file_id: i64) -> anyhow::Result<()> {
        if let Some(path) = queries::delete_room_file(&self.db, file_id).await? {
            tokio::fs::remove_file(&path).await.ok();
        }
        Ok(())
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect()
}
