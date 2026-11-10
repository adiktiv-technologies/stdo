use std::{
    io,
    process::{Output, Stdio},
};

use tokio::{io::AsyncWriteExt, process::Command};

/// Runs one command with buffered input and captured output.
/// The caller supplies arguments individually and handles the exit status.
pub async fn run_command(program: &str, args: &[String], input: &[u8]) -> io::Result<Output> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let mut stdin = child.stdin.take().expect("child stdin is piped");
    let write_input = async move {
        match stdin.write_all(input).await {
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
            result => result,
        }
    };
    let (output, ()) = tokio::try_join!(child.wait_with_output(), write_input)?;
    Ok(output)
}
