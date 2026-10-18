//! JSON-lines protocol over stdin/stdout. Any program in any language can
//! start `pdfk stdio` as a child process and talk to it; no network involved.
//! Requests run concurrently, so match responses to requests by `id`.

use pdf_kernel::rpc::{self, Access};
use pdf_kernel::Engine;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// `out` is the real stdout, isolated from stray library prints (see out.rs).
pub async fn run(engine: Arc<Engine>, out: std::fs::File) -> std::io::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let writer = tokio::spawn(async move {
        let mut out = tokio::fs::File::from_std(out);
        while let Some(line) = rx.recv().await {
            out.write_all(line.as_bytes()).await?;
            out.write_all(b"\n").await?;
            out.flush().await?;
        }
        Ok::<_, std::io::Error>(())
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let engine = engine.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let resp = rpc::handle_json(&engine, &line, Access { allow_file_paths: true }).await;
            let _ = tx.send(resp);
        });
    }
    // Stdin closed: finish in-flight requests, then exit.
    drop(tx);
    writer.await.map_err(std::io::Error::other)?
}
