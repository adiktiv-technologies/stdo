use std::{
    env, io,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::UNIX_EPOCH,
};

use sha2::{Digest, Sha256};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
};

use crate::{
    dag::schema::{CommandStep, StdoutFormat},
    doctor::resolve_executable,
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
    pub(super) async fn new(
        id: &str,
        argv: &[String],
        step: &CommandStep,
        stdin: &[u8],
        paths: &[String],
    ) -> Result<Self, Error> {
        let mut hash = Sha256::new();
        hash.update(b"stdo cache key v2");
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
        hash_executable(&mut hash, id, &argv[0]).await?;
        hash.update((paths.len() as u64).to_be_bytes());
        if !paths.is_empty() {
            let root = fs::canonicalize(".")
                .await
                .map_err(|source| dependency(id, ".", source))?;
            let mut chunk = vec![0; IO_CHUNK];
            for path in paths {
                let digest = hash_file(id, &root, Path::new(path), &mut chunk).await?;
                hash_field(&mut hash, path.as_bytes());
                hash.update(digest);
            }
        }
        hash.update((step.cache_env.len() as u64).to_be_bytes());
        for name in &step.cache_env {
            hash_field(&mut hash, name.as_bytes());
            if let Some(value) = env::var_os(name) {
                hash.update([1]);
                hash_field(&mut hash, value.as_encoded_bytes());
            } else {
                hash.update([0]);
            }
        }
        Ok(Self(
            Path::new(DIRECTORY).join(format!("{:x}", hash.finalize())),
        ))
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

async fn hash_executable(hash: &mut Sha256, step: &str, program: &str) -> Result<(), Error> {
    let path = resolve_executable(program).ok_or_else(|| {
        dependency(
            step,
            program,
            io::Error::new(io::ErrorKind::NotFound, "executable not found"),
        )
    })?;
    let path = fs::canonicalize(&path)
        .await
        .map_err(|source| dependency(step, path, source))?;
    let metadata = fs::metadata(&path)
        .await
        .map_err(|source| dependency(step, &path, source))?;
    let modified = metadata
        .modified()
        .map_err(|source| dependency(step, &path, source))?;
    let (sign, elapsed) = match modified.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => (0, elapsed),
        Err(error) => (1, error.duration()),
    };
    hash_field(hash, path.as_os_str().as_encoded_bytes());
    hash.update(metadata.len().to_be_bytes());
    hash.update([sign]);
    hash.update(elapsed.as_nanos().to_be_bytes());
    Ok(())
}

async fn hash_file(
    step: &str,
    root: &Path,
    path: &Path,
    chunk: &mut [u8],
) -> Result<[u8; 32], Error> {
    if path.is_absolute() || path.as_os_str().is_empty() {
        return Err(dependency(
            step,
            path,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a relative workspace file path",
            ),
        ));
    }
    let canonical = fs::canonicalize(root.join(path))
        .await
        .map_err(|source| dependency(step, path, source))?;
    if !canonical.starts_with(root) {
        return Err(dependency(
            step,
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "path escapes the workspace"),
        ));
    }
    let metadata = fs::metadata(&canonical)
        .await
        .map_err(|source| dependency(step, path, source))?;
    if !metadata.is_file() {
        return Err(dependency(
            step,
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "expected a regular file"),
        ));
    }
    let mut file = File::open(canonical)
        .await
        .map_err(|source| dependency(step, path, source))?;
    file.set_max_buf_size(IO_CHUNK);
    let mut hash = Sha256::new();
    loop {
        let count = file
            .read(chunk)
            .await
            .map_err(|source| dependency(step, path, source))?;
        if count == 0 {
            return Ok(hash.finalize().into());
        }
        hash.update(&chunk[..count]);
    }
}

fn dependency(step: &str, path: impl Into<PathBuf>, source: io::Error) -> Error {
    Error::CacheDependency {
        step: step.to_owned(),
        path: path.into(),
        source,
    }
}

fn hash_field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}
