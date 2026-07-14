//! HLS downloads integrated with the registry pipeline (resume via `.hls.warp`).

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use indicatif::ProgressBar;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

use crate::core::DownloadEntry;
use crate::download::rate_limit::{acquire_composed, RunLimits};

#[derive(Serialize, Deserialize)]
struct HlsSnapshot {
    segment_urls: Vec<String>,
    completed: Vec<usize>,
}

pub async fn run_entry(
    entry: &DownloadEntry,
    _semaphore: Arc<Semaphore>,
    limits: RunLimits,
    pb: Option<&ProgressBar>,
) -> Result<u64> {
    let quality = entry
        .hls_quality
        .clone()
        .unwrap_or_else(|| "best".to_string());
    let max_concurrent = entry.hls_concurrent.unwrap_or(8) as usize;

    let mut client_builder = reqwest::Client::builder()
        .user_agent("Warp/0.3")
        .tcp_keepalive(Some(std::time::Duration::from_secs(30)));
    if let Some(ref proxy_url) = entry.proxy
        && let Ok(proxy) = reqwest::Proxy::all(proxy_url)
    {
        client_builder = client_builder.proxy(proxy);
    }
    let client = Arc::new(client_builder.build()?);

    let snapshot_path = entry.target_path.with_extension("hls.warp");
    let segment_urls = resolve_segments(&client, &entry.url, &quality).await?;

    let mut completed: HashSet<usize> = load_snapshot(&snapshot_path)
        .map(|s| s.completed.into_iter().collect())
        .unwrap_or_default();

    let total = segment_urls.len();
    if total == 0 {
        anyhow::bail!("no segments in playlist");
    }

    let parts_dir = entry.target_path.with_extension("hls_parts");
    tokio::fs::create_dir_all(&parts_dir).await.ok();

    if let Some(bar) = pb {
        bar.set_length(total as u64);
        bar.set_position(completed.len() as u64);
    }

    let mut join_set = tokio::task::JoinSet::new();
    let mut in_flight = 0usize;

    for (i, url) in segment_urls.iter().enumerate() {
        if completed.contains(&i) {
            continue;
        }

        while in_flight >= max_concurrent {
            if let Some(res) = join_set.join_next().await {
                finish_segment(res?, &mut completed, &snapshot_path, &segment_urls)?;
                in_flight -= 1;
                if let Some(bar) = pb {
                    bar.set_position(completed.len() as u64);
                }
            }
        }

        let client = Arc::clone(&client);
        let part_path = parts_dir.join(format!("{i:06}.part"));
        let url = url.clone();
        let limits = limits.clone();

        join_set.spawn(async move {
            let resp = client.get(&url).send().await?;
            let bytes = resp.bytes().await?;
            acquire_composed(limits.global.as_ref(), limits.local.as_ref(), bytes.len() as u64).await;
            tokio::fs::write(&part_path, &bytes).await?;
            Ok::<_, anyhow::Error>(i)
        });
        in_flight += 1;
    }

    while let Some(res) = join_set.join_next().await {
        finish_segment(res?, &mut completed, &snapshot_path, &segment_urls)?;
        if let Some(bar) = pb {
            bar.set_position(completed.len() as u64);
        }
    }

    // Concatenate parts in order into a temp file, then atomically rename.
    // This prevents a partial .ts file if the process dies mid-concatenation.
    let concat_path = entry.target_path.with_extension("hlspart");
    let mut output = tokio::fs::File::create(&concat_path).await?;
    let mut bytes_total = 0u64;
    for i in 0..total {
        let part = parts_dir.join(format!("{i:06}.part"));
        if part.exists() {
            let data = tokio::fs::read(&part).await?;
            bytes_total += data.len() as u64;
            output.write_all(&data).await?;
            tokio::fs::remove_file(part).await.ok();
        }
    }
    output.flush().await?;
    tokio::fs::rename(&concat_path, &entry.target_path).await?;
    let _ = tokio::fs::remove_dir(&parts_dir).await;
    let _ = tokio::fs::remove_file(&snapshot_path).await;

    Ok(bytes_total)
}

fn finish_segment(
    res: Result<usize, anyhow::Error>,
    completed: &mut HashSet<usize>,
    snapshot_path: &Path,
    segment_urls: &[String],
) -> Result<()> {
    let idx = res?;
    completed.insert(idx);
    save_snapshot(snapshot_path, segment_urls, completed)
}

async fn resolve_segments(
    client: &reqwest::Client,
    playlist_url: &str,
    quality: &str,
) -> Result<Vec<String>> {
    let body = client
        .get(playlist_url)
        .send()
        .await?
        .text()
        .await
        .context("read playlist")?;

    let playlist = m3u8_rs::parse_playlist_res(body.as_bytes())
        .map_err(|e| anyhow::anyhow!("parse playlist: {e:?}"))?;

    match playlist {
        m3u8_rs::Playlist::MasterPlaylist(master) => {
            let selected = select_variant(&master.variants, quality)?;
            let variant_url = resolve_url(playlist_url, &selected.uri);
            let media_body = client.get(&variant_url).send().await?.text().await?;
            let media = match m3u8_rs::parse_playlist_res(media_body.as_bytes())
                .map_err(|e| anyhow::anyhow!("parse media playlist: {e:?}"))?
            {
                m3u8_rs::Playlist::MediaPlaylist(m) => m,
                _ => anyhow::bail!("expected media playlist"),
            };
            Ok(media
                .segments
                .iter()
                .map(|s| resolve_url(&variant_url, &s.uri))
                .collect())
        }
        m3u8_rs::Playlist::MediaPlaylist(media) => Ok(media
            .segments
            .iter()
            .map(|s| resolve_url(playlist_url, &s.uri))
            .collect()),
    }
}

fn select_variant<'a>(
    variants: &'a [m3u8_rs::VariantStream],
    quality: &str,
) -> Result<&'a m3u8_rs::VariantStream> {
    if variants.is_empty() {
        anyhow::bail!("no variants in master playlist");
    }
    Ok(match quality {
        "low" => variants.iter().min_by_key(|v| v.bandwidth).context("no variant")?,
        "med" | "medium" => {
            let mid = variants.iter().map(|v| v.bandwidth).sum::<u64>() / variants.len() as u64;
            variants
                .iter()
                .min_by_key(|v| (v.bandwidth as i64 - mid as i64).abs())
                .context("no variant")?
        }
        _ => variants.iter().max_by_key(|v| v.bandwidth).context("no variant")?,
    })
}

fn resolve_url(base: &str, uri: &str) -> String {
    if uri.starts_with("http") {
        return uri.to_string();
    }
    let base = base
        .trim_end_matches('/')
        .rsplit_once('/')
        .map(|(b, _)| b)
        .unwrap_or(base);
    format!("{base}/{}", uri.trim_start_matches('/'))
}

fn load_snapshot(path: &Path) -> Result<HlsSnapshot> {
    let data = std::fs::read(path)?;
    Ok(bincode::deserialize(&data)?)
}

fn save_snapshot(path: &Path, urls: &[String], completed: &HashSet<usize>) -> Result<()> {
    let snap = HlsSnapshot {
        segment_urls: urls.to_vec(),
        completed: completed.iter().copied().collect(),
    };
    std::fs::write(path, bincode::serialize(&snap)?)?;
    Ok(())
}
