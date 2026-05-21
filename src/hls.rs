//! HLS (HTTP Live Streaming) downloader.
//!
//! Downloads M3U8 playlists and their media segments by:
//! 1. Fetching and parsing the master playlist
//! 2. Selecting the best matching quality variant
//! 3. Parsing the media playlist for segment URLs
//! 4. Downloading segments in parallel
//! 5. Concatenating into a single output file

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Downloads an HLS stream from a playlist URL.
pub async fn download_hls(
    playlist_url: &str,
    output: &Option<PathBuf>,
    quality: String,
    max_concurrent: usize,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent("Warp/0.1")
        .build()
        .context("Failed to build HTTP client")?;

    println!("Fetching playlist: {}", playlist_url);
    let playlist_body = client
        .get(playlist_url)
        .send()
        .await
        .context("Failed to fetch playlist")?
        .text()
        .await
        .context("Failed to read playlist body")?;

    let playlist = m3u8_rs::parse_playlist_res(&playlist_body.as_bytes())
        .map_err(|e| anyhow::anyhow!("Failed to parse M3U8 playlist: {:?}", e))?;

    let (base_url, segment_urls) = match playlist {
        m3u8_rs::Playlist::MasterPlaylist(master) => {
            println!("Found master playlist with {} variant(s)", master.variants.len());
            let variants = &master.variants;

            if variants.is_empty() {
                anyhow::bail!("Master playlist has no variants");
            }

            // Select the best variant based on quality preference
            let selected = match quality.as_str() {
                "low" => variants.iter().min_by_key(|v| v.bandwidth as i64),
                "med" | "medium" => {
                    let mid = variants.iter().map(|v| v.bandwidth).sum::<u64>() / variants.len() as u64;
                    variants.iter().min_by_key(|v| (v.bandwidth as i64 - mid as i64).abs())
                }
                "high" => variants.iter().max_by_key(|v| v.bandwidth),
                _ => variants.iter().max_by_key(|v| v.bandwidth),
            }
            .context("Could not select variant")?;

            let variant_url = match &selected.uri {
                uri if uri.starts_with("http") => uri.clone(),
                _ => {
                    // Resolve relative URL against playlist base
                    let base = playlist_url.trim_end_matches(|c| c == '/')
                        .rsplit_once('/')
                        .map(|(base, _)| base)
                        .unwrap_or(playlist_url);
                    format!("{}/{}", base, selected.uri.trim_start_matches('/'))
                }
            };

            println!(
                "Selected variant: {} kbps, resolution: {:?}",
                selected.bandwidth / 1000,
                selected.resolution
            );

            // Fetch the media playlist
            println!("Fetching media playlist: {}", variant_url);
            let media_body = client
                .get(&variant_url)
                .send()
                .await
                .context("Failed to fetch media playlist")?
                .text()
                .await
                .context("Failed to read media playlist body")?;

            let media_playlist = match m3u8_rs::parse_playlist_res(&media_body.as_bytes())
                .map_err(|e| anyhow::anyhow!("Failed to parse media playlist: {:?}", e))?
            {
                m3u8_rs::Playlist::MediaPlaylist(m) => m,
                _ => anyhow::bail!("Expected media playlist but got master playlist"),
            };

            let segments: Vec<String> = media_playlist
                .segments
                .iter()
                .map(|seg| {
                    let uri = &seg.uri;
                    if uri.starts_with("http") {
                        uri.clone()
                    } else {
                        let base = variant_url
                            .trim_end_matches(|c| c == '/')
                            .rsplit_once('/')
                            .map(|(base, _)| base)
                            .unwrap_or(&variant_url);
                        format!("{}/{}", base, uri.trim_start_matches('/'))
                    }
                })
                .collect();

            let base = playlist_url
                .trim_end_matches(|c| c == '/')
                .rsplit_once('/')
                .map(|(base, _)| base)
                .unwrap_or(playlist_url);

            (base.to_string(), segments)
        }
        m3u8_rs::Playlist::MediaPlaylist(media) => {
            println!("Found media playlist with {} segment(s)", media.segments.len());
            let segments: Vec<String> = media
                .segments
                .iter()
                .map(|seg| {
                    let uri = &seg.uri;
                    if uri.starts_with("http") {
                        uri.clone()
                    } else {
                        let base = playlist_url
                            .trim_end_matches(|c| c == '/')
                            .rsplit_once('/')
                            .map(|(base, _)| base)
                            .unwrap_or(playlist_url);
                        format!("{}/{}", base, uri.trim_start_matches('/'))
                    }
                })
                .collect();

            let base = playlist_url
                .trim_end_matches(|c| c == '/')
                .rsplit_once('/')
                .map(|(base, _)| base)
                .unwrap_or(playlist_url);

            (base.to_string(), segments)
        }
    };

    let total_segments = segment_urls.len();
    if total_segments == 0 {
        anyhow::bail!("No segments found in playlist");
    }

    println!("Found {} segment(s) to download", total_segments);

    let output_path = match output {
        Some(path) => path.clone(),
        None => {
            let name = base_url
                .trim_end_matches('/')
                .rsplit_once('/')
                .map(|(_, name)| format!("{}.ts", name))
                .unwrap_or_else(|| "output.ts".to_string());
            PathBuf::from(name)
        }
    };

    // Download segments in parallel using a semaphore
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(max_concurrent));
    let client = std::sync::Arc::new(client);

    println!("Downloading segments ({} concurrent)...", max_concurrent);

    let mut segment_data: Vec<(usize, Vec<u8>)> = Vec::with_capacity(total_segments);
    let mut handles = tokio::task::JoinSet::new();

    for (i, url) in segment_urls.iter().enumerate() {
        let client = std::sync::Arc::clone(&client);
        let semaphore = std::sync::Arc::clone(&semaphore);
        let url = url.clone();

        handles.spawn(async move {
            let _permit = semaphore.acquire().await.context("Failed to acquire semaphore")?;
            let resp = client
                .get(&url)
                .send()
                .await
                .context(format!("Failed to download segment {}: {}", i, url))?;
            let bytes = resp
                .bytes()
                .await
                .context(format!("Failed to read segment {}: {}", i, url))?;
            Ok::<(usize, Vec<u8>), anyhow::Error>((i, bytes.to_vec()))
        });
    }

    while let Some(result) = handles.join_next().await {
        match result {
            Ok(Ok((i, data))) => {
                segment_data.push((i, data));
                print!("\rDownloaded {}/{} segments", segment_data.len(), total_segments);
                use std::io::{Write, stdout};
                stdout().flush().ok();
            }
            Ok(Err(e)) => eprintln!("\nSegment download error: {}", e),
            Err(e) => eprintln!("\nTask panic: {}", e),
        }
    }

    println!();

    if segment_data.len() < total_segments {
        eprintln!(
            "Warning: Only downloaded {}/{} segments. Output may be incomplete.",
            segment_data.len(),
            total_segments
        );
    }

    // Sort segments by index and concatenate
    segment_data.sort_by_key(|(i, _)| *i);

    println!("Concatenating {} segments into '{}'...", segment_data.len(), output_path.display());
    let mut output_file = tokio::fs::File::create(&output_path)
        .await
        .context("Failed to create output file")?;

    use tokio::io::AsyncWriteExt;
    for (_i, data) in &segment_data {
        output_file.write_all(data).await?;
    }
    output_file.flush().await?;

    println!("Done! Output: {}", output_path.display());
    Ok(())
}
