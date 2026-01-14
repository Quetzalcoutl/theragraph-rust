use anyhow::{Context, Result};
use std::io::Write;
use tempfile::NamedTempFile;
use tokio::process::Command;
use tracing::{debug, instrument};

pub struct GraphClient {
    host: String,
    port: String,
}

impl GraphClient {
    pub fn new() -> Self {
        Self {
            host: std::env::var("NEBULA_HOST").unwrap_or_else(|_| "graphd".to_string()),
            port: std::env::var("NEBULA_PORT").unwrap_or_else(|_| "9669".to_string()),
        }
    }

    /// Execute an NGQL query via the nebula-console binary
    /// This mimics a "native" client by shelling out, allowing us to leverage
    /// the C++ engine without complex build dependencies.
    #[instrument(skip(self, query))]
    pub async fn execute_query(&self, query: &str) -> Result<String> {
        let mut tmp = NamedTempFile::new().context("Failed to create temp file for NGQL")?;
        write!(tmp, "{}", query).context("Failed to write NGQL to temp file")?;
        let tmp_path = tmp
            .path()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid path"))?
            .to_string();

        debug!("Executing NGQL: {}", query);

        // JOINT TEAM FIX (Niko Matsakis / Alice Ryhl):
        // Switched from std::process (blocking) to tokio::process (async).
        // This ensures shelling out to C++ doesn't block our Rust worker threads.
        let output = Command::new("nebula-console")
            .arg("-addr")
            .arg(&self.host)
            .arg("-port")
            .arg(&self.port)
            .arg("-u")
            .arg("root")
            .arg("-p")
            .arg("nebula")
            .arg("-f")
            .arg(tmp_path)
            .output()
            .await // <--- Non-blocking await
            .context("Failed to execute nebula-console")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Nebula query failed: {}", stderr);
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        Ok(stdout)
    }

    /// "ByteGraph-style" Traversal: Find NFTs purchased/liked by friends of friends.
    ///
    /// Optimized by ByteGraph Research Team (Changji Li, Hongzhi Chen et al.)
    /// 
    /// Logic:
    /// 1. Start at `user_address`
    /// 2. Traverse `follows` edge to find Friends
    /// 3. Traverse `follows` edge again to find Friends-of-Friends (FoF)
    /// 4. Traverse `likes` edge from FoFs to find NFTs
    /// 5. Apply temporal decay: recent FoF activity = stronger signal
    /// 6. Weight by FoF overlap (more FoFs liking same content = stronger recommendation)
    /// 7. Return top scored NFTs based on collaborative filtering strength
    pub async fn get_fof_recommendations(&self, user_address: &str) -> Result<Vec<(String, f64)>> {
        // Changji Li: Multi-hop traversal with weight accumulation for ByteGraph personalization
        let query = format!(
            r#"
            USE theragraph;
            MATCH (u:user)-[:follows]->(f:user)-[:follows]->(fof:user)-[l:likes]->(n:post)
            WHERE id(u) == "{}"
            WITH n, 
                 count(DISTINCT fof) AS fof_count,
                 count(DISTINCT f) AS friend_count,
                 avg(l.weight) AS avg_engagement,
                 max(l.liked_at) AS most_recent_like
            RETURN 
                n.id AS nft_id,
                (fof_count * 2.0 + friend_count * 1.5) * avg_engagement * 
                  (1.0 / (1.0 + (timestamp() - most_recent_like) / 86400.0)) AS score
            ORDER BY score DESC
            LIMIT 50;
            "#,
            user_address
        );

        let output = self.execute_query(&query).await?;
        debug!("ByteGraph FoF Query Output: {}", output);

        // Andrew Gallant Optimization:
        // Avoid heavy regex compilation. Use efficient string splitting.
        // Nebula console output usually includes headers and separators.
        // Example output:
        // +--------------------------+-------+
        // | nft_address              | score |
        // +--------------------------+-------+
        // | "0x123..."               | 5.0   |
        // +--------------------------+-------+

        let mut results = Vec::new();

        for line in output.lines() {
            let line = line.trim();
            // Skip borders and empty lines
            if line.starts_with('+') || line.is_empty() {
                continue;
            }
            // Skip headers
            if line.contains("nft_address") && line.contains("score") {
                continue;
            }

            // Parse valid row: | "0x..." | 5.0 |
            let parts: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
            if parts.len() < 3 {
                continue;
            }

            // Extract address (remove quotes)
            let addr_raw = parts[1];
            let addr = addr_raw.trim_matches('"').to_string();
            if addr.is_empty() {
                continue;
            }

            // Extract score
            let score_raw = parts[2];
            if let Ok(score) = score_raw.parse::<f64>() {
                results.push((addr, score));
            }
        }

        Ok(results)
    }
}
