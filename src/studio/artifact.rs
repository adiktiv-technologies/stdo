use std::{
    collections::BTreeMap,
    fs::Metadata,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::Mutex,
};

use serde::Serialize;
use tokio::{
    fs::{self, File},
    io::AsyncReadExt,
};

use crate::{
    dag::schema::{InputKind, Workflow},
    engine,
};

use super::http::Problem;

#[derive(Default)]
pub(super) struct Store(Mutex<BTreeMap<PathBuf, (u64, u64)>>);

#[derive(Serialize)]
pub(super) struct Artifact {
    path: String,
    name: String,
    content_type: &'static str,
}

impl Store {
    pub async fn register(
        &self,
        root: &Path,
        workflow: &Workflow,
        overrides: &[String],
        output: &str,
    ) -> Vec<Artifact> {
        let Ok(inputs) = engine::resolve_inputs(workflow, overrides) else {
            return Vec::new();
        };
        let mut allowed = BTreeMap::new();
        let mut artifacts = Vec::new();
        let declared = inputs.iter().filter_map(|(name, value)| {
            if matches!(workflow.inputs[name].kind, InputKind::Path) {
                value.as_str().map(|path| (path, true))
            } else {
                None
            }
        });
        let printed = output.lines().map(|line| (line.trim(), false));
        for (input, declared) in declared.chain(printed) {
            let Ok((path, metadata, mime)) = candidate(root, input).await else {
                continue;
            };
            if (!declared && !path.starts_with(root))
                || allowed.contains_key(&path)
                || open_media(&path, &metadata, mime).await.is_err()
            {
                continue;
            }
            let Some(name) = Path::new(input).file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            allowed.insert(path, identity(&metadata));
            artifacts.push(Artifact {
                path: input.to_owned(),
                name: name.to_owned(),
                content_type: mime,
            });
        }
        *self.0.lock().expect("artifact lock") = allowed;
        artifacts
    }

    pub async fn open(
        &self,
        root: &Path,
        input: &str,
    ) -> Result<(File, u64, &'static str), Problem> {
        let (path, metadata, mime) = candidate(root, input).await?;
        if !path.starts_with(root)
            && self.0.lock().expect("artifact lock").get(&path) != Some(&identity(&metadata))
        {
            return Err(unavailable());
        }
        let file = open_media(&path, &metadata, mime).await?;
        Ok((file, metadata.len(), mime))
    }
}

async fn candidate(root: &Path, input: &str) -> Result<(PathBuf, Metadata, &'static str), Problem> {
    let input = Path::new(input);
    if input.components().any(|part| part == Component::ParentDir) {
        return Err(unavailable());
    }
    let mime = match input
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("wav" | "wave") => "audio/wav",
        Some("mp3") => "audio/mpeg",
        Some("ogg" | "opus") => "audio/ogg",
        Some("flac") => "audio/flac",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        _ => return Err(unavailable()),
    };
    let requested = root.join(input);
    let metadata = fs::symlink_metadata(&requested)
        .await
        .map_err(|_| unavailable())?;
    if !metadata.is_file() {
        return Err(unavailable());
    }
    let path = fs::canonicalize(&requested)
        .await
        .map_err(|_| unavailable())?;
    Ok((path, metadata, mime))
}

fn identity(metadata: &Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

async fn open_media(path: &Path, expected: &Metadata, mime: &str) -> Result<File, Problem> {
    let mut file = File::open(path).await.map_err(|_| unavailable())?;
    let metadata = file.metadata().await.map_err(|_| unavailable())?;
    if !metadata.is_file() || identity(&metadata) != identity(expected) {
        return Err(unavailable());
    }
    let mut header = [0; 12];
    file.read_exact(&mut header)
        .await
        .map_err(|_| unavailable())?;
    let matches = match mime {
        "audio/wav" => header.starts_with(b"RIFF") && header.ends_with(b"WAVE"),
        "audio/mpeg" => {
            header.starts_with(b"ID3") || (header[0] == 0xff && header[1] & 0xe0 == 0xe0)
        }
        "audio/ogg" => header.starts_with(b"OggS"),
        "audio/flac" => header.starts_with(b"fLaC"),
        "video/mp4" => &header[4..8] == b"ftyp",
        "video/webm" => header.starts_with(b"\x1a\x45\xdf\xa3"),
        "video/quicktime" => matches!(&header[4..8], b"ftyp" | b"moov" | b"wide" | b"mdat"),
        _ => false,
    };
    if !matches {
        return Err(unavailable());
    }
    Ok(file)
}

fn unavailable() -> Problem {
    Problem::new("404 Not Found", "Media artifact not available")
}
