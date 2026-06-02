//! HLS (HTTP Live Streaming) support — registry-integrated via `runner`.

mod runner;

pub use runner::run_entry;

/// Legacy direct download — adds to registry and runs through unified pipeline.
pub async fn download_hls_via_registry(
    registry: &crate::download_registry::Registry,
    playlist_url: &str,
    output: &Option<std::path::PathBuf>,
    quality: String,
    concurrent: usize,
) -> anyhow::Result<String> {
    let target = match output {
        Some(p) => p.clone(),
        None => {
            let name = playlist_url
                .trim_end_matches('/')
                .rsplit_once('/')
                .map(|(_, n)| format!("{n}.ts"))
                .unwrap_or_else(|| "output.ts".to_string());
            std::path::PathBuf::from(name)
        }
    };

    let id = registry
        .add_hls(
            playlist_url.to_string(),
            target,
            Some(quality),
            Some(concurrent as u32),
        )
        .await?;

    Ok(id)
}
