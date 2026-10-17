//! Running `ffmpeg` for the M4B conversion (SKADI-T-0139).
//!
//! ffmpeg is used for this and nothing else: probing stays pure Rust
//! (`skadi-media-probe`). The process is bounded by a timeout, killed when the
//! daemon shuts down, and reports progress from its `-progress` output.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// Why a run did not produce a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The binary could not be started.
    Spawn(String),
    /// ffmpeg exited non-zero; carries the end of its stderr.
    Failed(String),
    /// The run took longer than its bound and was killed.
    TimedOut(Duration),
    /// The daemon is shutting down.
    Cancelled,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "could not start ffmpeg: {e}"),
            Self::Failed(e) => write!(f, "ffmpeg failed: {e}"),
            Self::TimedOut(d) => {
                write!(f, "ffmpeg ran longer than {}s and was stopped", d.as_secs())
            }
            Self::Cancelled => write!(f, "stopped: the daemon is shutting down"),
        }
    }
}

/// `true` when `ffmpeg -version` runs and exits 0.
pub async fn available(ffmpeg: &Path) -> bool {
    let run = Command::new(ffmpeg)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(
        tokio::time::timeout(Duration::from_secs(10), run).await,
        Ok(Ok(s)) if s.success()
    )
}

/// The bound on one conversion: the book's own running time, and never less
/// than an hour. AAC encodes many times faster than real time, so a run that
/// reaches this is stuck, not slow.
#[must_use]
pub fn timeout_for(total: Option<Duration>) -> Duration {
    total.unwrap_or_default().max(Duration::from_secs(3600))
}

/// Parse one `-progress` line: the output time reached, if the line says.
#[must_use]
pub fn progress_time(line: &str) -> Option<Duration> {
    let (k, v) = line.trim().split_once('=')?;
    // `out_time_ms` is microseconds too, despite its name.
    if k == "out_time_us" || k == "out_time_ms" {
        return v.parse::<u64>().ok().map(Duration::from_micros);
    }
    None
}

/// Run ffmpeg with `args`. `on_progress` gets whole percents (0–100) as they
/// advance, when `total` is known.
pub async fn run(
    ffmpeg: &Path,
    args: &[String],
    total: Option<Duration>,
    timeout: Duration,
    cancel: &CancellationToken,
    mut on_progress: impl FnMut(u8) + Send,
) -> Result<(), RunError> {
    let mut child = Command::new(ffmpeg)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| RunError::Spawn(e.to_string()))?;

    // Drain stderr on its own task so a chatty ffmpeg cannot fill the pipe
    // and stall. Only the tail is kept for the error message.
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        let text = String::from_utf8_lossy(&buf).into_owned();
        let tail: String = text
            .chars()
            .rev()
            .take(600)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        tail.trim().to_string()
    });

    let stdout = child.stdout.take().expect("stdout is piped");
    let mut lines = BufReader::new(stdout).lines();
    let total_us = total.map(|t| t.as_micros()).filter(|t| *t > 0);
    let mut last_pct: Option<u8> = None;

    // The future borrows `child`; it is dropped at the end of this block so
    // the child can be killed before the caller removes the partial output.
    let outcome = {
        let work = async {
            while let Ok(Some(line)) = lines.next_line().await {
                if let (Some(at), Some(total_us)) = (progress_time(&line), total_us) {
                    let pct = (at.as_micros().saturating_mul(100) / total_us).min(100) as u8;
                    if last_pct != Some(pct) {
                        last_pct = Some(pct);
                        on_progress(pct);
                    }
                }
            }
            child.wait().await
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(RunError::Cancelled),
            r = tokio::time::timeout(timeout, work) => match r {
                Err(_) => Err(RunError::TimedOut(timeout)),
                Ok(Err(e)) => Err(RunError::Failed(e.to_string())),
                Ok(Ok(s)) => Ok(s),
            },
        }
    };
    let status = match outcome {
        Ok(s) => s,
        Err(e) => {
            let _ = child.kill().await;
            return Err(e);
        }
    };
    let err_tail = err_task.await.unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        Err(RunError::Failed(if err_tail.is_empty() {
            format!("exit status {status}")
        } else {
            err_tail
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_progress_lines() {
        assert_eq!(
            progress_time("out_time_us=1500000"),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(
            progress_time("out_time_ms=2000000"),
            Some(Duration::from_secs(2))
        );
        assert_eq!(progress_time("out_time=00:00:01.500000"), None);
        assert_eq!(progress_time("progress=end"), None);
        assert_eq!(progress_time("out_time_us=N/A"), None);
    }

    #[test]
    fn the_bound_is_the_running_time_but_at_least_an_hour() {
        assert_eq!(timeout_for(None), Duration::from_secs(3600));
        assert_eq!(
            timeout_for(Some(Duration::from_secs(60))),
            Duration::from_secs(3600)
        );
        assert_eq!(
            timeout_for(Some(Duration::from_secs(20 * 3600))),
            Duration::from_secs(20 * 3600)
        );
    }

    #[tokio::test]
    async fn a_missing_binary_is_not_available() {
        assert!(!available(Path::new("/nonexistent/ffmpeg")).await);
        let err = run(
            Path::new("/nonexistent/ffmpeg"),
            &[],
            None,
            Duration::from_secs(5),
            &CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RunError::Spawn(_)));
    }
}
