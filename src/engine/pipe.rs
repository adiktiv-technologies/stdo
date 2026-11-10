use std::{future, io, sync::Arc};

use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::mpsc,
};

use crate::error::Error;

use super::budget::{Budget, Buffer};

type Packet = Option<Arc<Buffer>>;

pub(super) const CHUNK_SIZE: usize = 64 * 1024;

pub(super) enum Input {
    Buffered(Arc<Buffer>),
    Live(mpsc::Receiver<Packet>),
}

impl Input {
    pub(super) fn cache_bytes(&self) -> &[u8] {
        match self {
            Self::Buffered(buffer) => buffer.bytes(),
            Self::Live(_) => unreachable!("live input cannot be used as a cache key"),
        }
    }

    pub(super) async fn write(self, mut stdin: impl AsyncWrite + Unpin) -> io::Result<()> {
        let result = match self {
            Self::Buffered(buffer) => stdin.write_all(buffer.bytes()).await,
            Self::Live(mut pipe) => {
                while let Some(packet) = pipe.recv().await {
                    let Some(buffer) = packet else { return Ok(()) };
                    if let Err(error) = stdin.write_all(buffer.bytes()).await {
                        return ignore_broken_pipe(error);
                    }
                }
                // A producer dropped its sender without committing EOF. Keep
                // stdin open until the group handles the original failure.
                future::pending().await
            }
        };
        result.or_else(ignore_broken_pipe)
    }
}

fn ignore_broken_pipe(error: io::Error) -> io::Result<()> {
    if error.kind() == io::ErrorKind::BrokenPipe {
        Ok(())
    } else {
        Err(error)
    }
}

#[derive(Default)]
pub(super) struct Outputs(Vec<mpsc::Sender<Packet>>);

impl Outputs {
    pub(super) fn connect(&mut self) -> Input {
        let (tx, rx) = mpsc::channel(1);
        self.0.push(tx);
        Input::Live(rx)
    }

    pub(super) async fn forward(
        &mut self,
        bytes: &[u8],
        budget: &Budget,
        step: &str,
    ) -> Result<(), Error> {
        for chunk in bytes.chunks(CHUNK_SIZE) {
            self.0.retain(|sender| !sender.is_closed());
            if self.0.is_empty() {
                break;
            }
            let mut packet = budget.buffer();
            packet.append(chunk, step)?;
            let packet = Arc::new(packet);
            for sender in &self.0 {
                let _ = sender.send(Some(packet.clone())).await;
            }
        }
        Ok(())
    }

    pub(super) async fn finish(self) {
        for sender in self.0 {
            let _ = sender.send(None).await;
        }
    }
}
