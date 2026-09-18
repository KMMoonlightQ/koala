use std::time::Duration;
use tokio::io::AsyncWriteExt;

const HOOK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq)]
pub enum HookOutcome {
    Ok,
    /// Exit code 2: the hook vetoes the action; payload is the reason (stderr).
    Blocked(String),
    /// Non-zero exit or execution failure (payload is diagnostic text).
    Failed(String),
}

pub async fn run_hook(command: &str, payload: &serde_json::Value) -> HookOutcome {
    run_hook_with_timeout(command, payload, HOOK_TIMEOUT).await
}

async fn run_hook_with_timeout(
    command: &str,
    payload: &serde_json::Value,
    timeout: Duration,
) -> HookOutcome {
    let child = super::process::spawn(
        tokio::process::Command::new("bash")
            .arg("-c")
            .arg(command)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped()),
    );
    let (mut child, _group) = match child {
        Ok(c) => c,
        Err(e) => return HookOutcome::Failed(format!("spawn failed: {e}")),
    };
    let stdin = child.stdin.take();
    let data = payload.to_string().into_bytes();
    let exchange = async move {
        let writer = async move {
            if let Some(mut stdin) = stdin {
                // Hooks may deliberately exit without consuming their payload.
                let _ = stdin.write_all(&data).await;
                let _ = stdin.shutdown().await;
            }
        };
        let (_, output) = tokio::join!(writer, child.wait_with_output());
        output
    };
    match tokio::time::timeout(timeout, exchange).await {
        Ok(Ok(output)) => {
            let code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            match code {
                0 => HookOutcome::Ok,
                2 => HookOutcome::Blocked(stderr),
                _ => HookOutcome::Failed(format!("exit {code}: {stderr}")),
            }
        }
        Ok(Err(e)) => HookOutcome::Failed(format!("wait failed: {e}")),
        Err(_) => HookOutcome::Failed("hook timed out".into()),
    }
}

pub async fn run_all(commands: &[String], payload: &serde_json::Value) -> HookOutcome {
    for cmd in commands {
        match run_hook(cmd, payload).await {
            HookOutcome::Ok => {}
            other => return other,
        }
    }
    HookOutcome::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn timeout_covers_a_hook_that_never_reads_large_input() {
        let payload = serde_json::json!({"input": "x".repeat(2_000_000)});
        let outcome = tokio::time::timeout(
            Duration::from_secs(3),
            run_hook_with_timeout("sleep 30", &payload, Duration::from_millis(100)),
        )
        .await
        .expect("stdin write escaped hook timeout");
        assert_eq!(outcome, HookOutcome::Failed("hook timed out".into()));
    }

    #[tokio::test]
    async fn drains_stderr_while_writing_large_input() {
        let payload = serde_json::json!({"input": "x".repeat(2_000_000)});
        let outcome = run_hook_with_timeout(
            "head -c 200000 /dev/zero >&2; cat >/dev/null",
            &payload,
            Duration::from_secs(3),
        )
        .await;
        assert_eq!(outcome, HookOutcome::Ok);
    }

    #[tokio::test]
    async fn exit_zero_is_ok() {
        let outcome = run_hook("cat > /dev/null", &serde_json::json!({"a": 1})).await;
        assert_eq!(outcome, HookOutcome::Ok);
    }

    #[tokio::test]
    async fn exit_two_blocks_with_stderr() {
        let outcome = run_hook("echo nope >&2; exit 2", &serde_json::json!({})).await;
        assert_eq!(outcome, HookOutcome::Blocked("nope".into()));
    }

    #[tokio::test]
    async fn other_nonzero_is_failure() {
        let outcome = run_hook("exit 3", &serde_json::json!({})).await;
        assert!(matches!(outcome, HookOutcome::Failed(_)));
    }

    #[tokio::test]
    async fn payload_reaches_stdin() {
        let outcome = run_hook(
            "grep -q '\"tool\":\"bash\"' || exit 2",
            &serde_json::json!({"tool": "bash"}),
        )
        .await;
        assert_eq!(outcome, HookOutcome::Ok);
    }

    #[tokio::test]
    async fn run_all_stops_at_first_block() {
        let cmds = vec![
            "exit 0".to_string(),
            "exit 2".to_string(),
            "exit 3".to_string(),
        ];
        assert!(matches!(
            run_all(&cmds, &serde_json::json!({})).await,
            HookOutcome::Blocked(_)
        ));
    }
}
