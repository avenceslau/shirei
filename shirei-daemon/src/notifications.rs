use std::process::Stdio;

fn interpolate(template: &str, title: &str, body: &str) -> String {
    template.replace("{title}", title).replace("{body}", body)
}

async fn run_command(parts: &[String]) -> bool {
    if parts.is_empty() {
        return false;
    }

    let program = &parts[0];
    let args = &parts[1..];
    match tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
    {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

pub async fn send(title: &str, body: &str) {
    if let Ok(command) = std::env::var("SHIREI_NOTIFY_CMD") {
        if let Ok(mut parts) = shell_words::split(&command) {
            for part in &mut parts {
                *part = interpolate(part, title, body);
            }
            let _ = run_command(&parts).await;
            return;
        }
    }

    #[cfg(target_os = "macos")]
    {
        let first = vec![
            "terminal-notifier".to_string(),
            "-title".to_string(),
            title.to_string(),
            "-message".to_string(),
            body.to_string(),
            "-group".to_string(),
            "shirei".to_string(),
        ];
        if run_command(&first).await {
            return;
        }

        let body = body.replace('"', "\\\"");
        let title = title.replace('"', "\\\"");
        let second = vec![
            "osascript".to_string(),
            "-e".to_string(),
            format!("display notification \"{body}\" with title \"{title}\""),
        ];
        let _ = run_command(&second).await;
        return;
    }

    #[cfg(not(target_os = "macos"))]
    {
        let command = vec![
            "notify-send".to_string(),
            title.to_string(),
            body.to_string(),
        ];
        let _ = run_command(&command).await;
    }
}
