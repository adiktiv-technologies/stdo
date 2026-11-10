use std::{
    io,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use sha2::{Digest, Sha256};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
};

use crate::{
    dag::schema::{CommandStep, StdoutFormat},
    error::Error,
};

use super::{
    budget::{Budget, Buffer, MAX_STEP_OUTPUT_BYTES},
    runner::Captured,
};

const DIRECTORY: &str = ".stdo/cache";
const MAGIC: &[u8; 8] = b"STDOCAS1";
// Magic, Unix exit code, stdout length, stderr length, followed by raw stream bytes.
const HEADER_SIZE: usize = 25;
const IO_CHUNK: usize = 64 * 1024;
static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

pub(super) struct Entry(PathBuf);

impl Entry {
    pub(super) fn new(argv: &[String], step: &CommandStep, stdin: &[u8]) -> Self {
        let mut hash = Sha256::new();
        hash.update(MAGIC);
        hash.update((argv.len() as u64).to_be_bytes());
        for argument in argv {
            hash_field(&mut hash, argument.as_bytes());
        }
        let stdout_format = match step.parse_stdout {
            None => 0,
            Some(StdoutFormat::Json) => 1,
            Some(StdoutFormat::Jsonl) => 2,
        };
        hash.update([u8::from(step.ignore_exit_code), stdout_format]);
        hash_field(&mut hash, stdin);
        Self(Path::new(DIRECTORY).join(format!("{:x}", hash.finalize())))
    }

    pub(super) async fn load(
        &self,
        budget: &Budget,
        step: &str,
    ) -> Result<Option<Captured>, Error> {
        let metadata = match fs::symlink_metadata(&self.0).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(self.error(error)),
        };
        if !metadata.is_file() {
            return Ok(None);
        }
        let mut file = match File::open(&self.0).await {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(self.error(error)),
        };
        file.set_max_buf_size(IO_CHUNK);
        let mut header = [0; HEADER_SIZE];
        if !self.read(&mut file, &mut header).await? || &header[..8] != MAGIC {
            return Ok(None);
        }
        let stdout_size = u64::from_be_bytes(header[9..17].try_into().expect("stdout size field"));
        let stderr_size = u64::from_be_bytes(header[17..25].try_into().expect("stderr size field"));
        let expected_size = stdout_size
            .checked_add(stderr_size)
            .and_then(|size| size.checked_add(HEADER_SIZE as u64));
        if expected_size
            != Some(
                file.metadata()
                    .await
                    .map_err(|error| self.error(error))?
                    .len(),
            )
        {
            return Ok(None);
        }
        let mut stdout = self.buffer(budget, step, stdout_size)?;
        let mut stderr = self.buffer(budget, step, stderr_size)?;
        if !self.read(&mut file, stdout.bytes_mut()).await?
            || !self.read(&mut file, stderr.bytes_mut()).await?
        {
            return Ok(None);
        }
        Ok(Some(Captured {
            stdout: Arc::new(stdout),
            stderr,
            status: ExitStatus::from_raw(i32::from(header[8]) << 8),
        }))
    }

    pub(super) async fn store(&self, captured: &Captured) -> Result<(), Error> {
        let Some(code) = captured
            .status
            .code()
            .and_then(|code| u8::try_from(code).ok())
        else {
            return Ok(());
        };
        fs::create_dir_all(DIRECTORY)
            .await
            .map_err(|error| self.error(error))?;
        let (temp, mut file) = loop {
            let nonce = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let temp = Path::new(DIRECTORY).join(format!(".tmp-{}-{nonce}", std::process::id()));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)
                .await
            {
                Ok(file) => break (temp, file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(self.error(error)),
            }
        };
        file.set_max_buf_size(IO_CHUNK);
        let stdout = captured.stdout.bytes();
        let stderr = captured.stderr.bytes();
        let result = async {
            file.write_all(MAGIC).await?;
            file.write_all(&[code]).await?;
            file.write_all(&(stdout.len() as u64).to_be_bytes()).await?;
            file.write_all(&(stderr.len() as u64).to_be_bytes()).await?;
            file.write_all(stdout).await?;
            file.write_all(stderr).await?;
            file.flush().await?;
            drop(file);
            fs::rename(&temp, &self.0).await
        }
        .await;
        if let Err(error) = result {
            let _ = fs::remove_file(&temp).await;
            return Err(self.error(error));
        }
        Ok(())
    }

    fn buffer(&self, budget: &Budget, step: &str, size: u64) -> Result<Buffer, Error> {
        if size > MAX_STEP_OUTPUT_BYTES as u64 {
            return Err(Error::MemoryLimitExceeded {
                step: step.to_owned(),
                limit_bytes: MAX_STEP_OUTPUT_BYTES,
            });
        }
        let mut buffer = budget.buffer();
        buffer.resize_zeroed(size as usize).map_err(|error| {
            buffer
                .limit_error(step)
                .unwrap_or_else(|| self.error(error))
        })?;
        Ok(buffer)
    }

    async fn read(&self, file: &mut File, buffer: &mut [u8]) -> Result<bool, Error> {
        match file.read_exact(buffer).await {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
            Err(error) => Err(self.error(error)),
        }
    }

    fn error(&self, source: io::Error) -> Error {
        Error::Cache {
            path: self.0.clone(),
            source,
        }
    }
}

fn hash_field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}
