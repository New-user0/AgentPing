//! `agentping logs [--follow]`

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;

use agentping_core::redact::redact;

use crate::cli::LogsArgs;
use crate::context::Ctx;
use crate::logging::log_files;
use crate::ui;

fn tail(path: &PathBuf, lines: usize) -> anyhow::Result<(Vec<String>, u64)> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    // Read at most the last 2 MiB; enough for any reasonable --lines.
    let start = len.saturating_sub(2 * 1024 * 1024);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    let all: Vec<&str> = buf.lines().collect();
    let from = all.len().saturating_sub(lines);
    Ok((all[from..].iter().map(|s| (*s).to_owned()).collect(), len))
}

pub fn run(ctx: &Ctx, args: &LogsArgs) -> anyhow::Result<()> {
    let files = log_files(&ctx.paths);
    let Some(mut current) = files.last().cloned() else {
        ui::info(&format!("No logs yet in {}", ctx.paths.log_dir.display()));
        return Ok(());
    };
    let (lines, mut pos) = tail(&current, args.lines)?;
    for l in lines {
        // Logs never contain secrets by design; redact defensively anyway.
        ui::line(&redact(&l));
    }
    if !args.follow {
        ui::line(&ui::dim(&format!("({})", current.display())));
        return Ok(());
    }
    loop {
        std::thread::sleep(Duration::from_millis(500));
        if let Some(newest) = log_files(&ctx.paths).last() {
            if *newest != current {
                current = newest.clone();
                pos = 0;
            }
        }
        let Ok(mut file) = std::fs::File::open(&current) else {
            continue;
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len < pos {
            pos = 0;
        }
        if len == pos {
            continue;
        }
        file.seek(SeekFrom::Start(pos))?;
        let mut buf = String::new();
        file.read_to_string(&mut buf)?;
        pos = len;
        for l in buf.lines() {
            ui::line(&redact(l));
        }
    }
}
