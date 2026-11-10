use std::{
    io,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
};

static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

pub(crate) async fn write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let (temp, mut file) = loop {
        let nonce = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temp = path.with_file_name(format!(".stdo-save-{}-{nonce}", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)
            .await
        {
            Ok(file) => break (temp, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let result = async {
        file.write_all(bytes).await?;
        file.flush().await?;
        drop(file);
        fs::rename(&temp, path).await
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(temp).await;
    }
    result
}
