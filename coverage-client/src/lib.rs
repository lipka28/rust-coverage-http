//! # coverage-client
//!
//! Client library for collecting Rust code coverage from Kubernetes pods
//! or any HTTP-accessible coverage server endpoint.
//!
//! ## Features
//!
//! - Collect profraw data from a coverage server via HTTP
//! - Kubernetes pod discovery and port-forwarding
//! - Generate coverage reports using `llvm-profdata` and `llvm-cov`
//! - Filter coverage reports to exclude instrumentation code
//! - Generate HTML reports

use base64::Engine;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;
use tracing::{info, warn};

#[derive(Debug, Deserialize)]
pub struct CoverageResponse {
    pub profraw_filename: String,
    pub profraw_data: String,
    pub timestamp: u64,
    pub coverage_enabled: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CollectionMetadata {
    pub pod_name: String,
    pub namespace: String,
    pub container: Option<String>,
    pub test_name: String,
    pub timestamp: String,
    pub binary_path: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CoverageError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("kubectl error: {0}")]
    Kubectl(String),

    #[error("llvm-profdata error: {0}")]
    LlvmProfdata(String),

    #[error("llvm-cov error: {0}")]
    LlvmCov(String),

    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    #[error("Coverage not enabled on target")]
    CoverageNotEnabled,

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CoverageError>;

/// Client for collecting coverage from instrumented Rust applications.
pub struct CoverageClient {
    namespace: String,
    output_dir: PathBuf,
    source_dir: Option<PathBuf>,
    binary_path: Option<PathBuf>,
    filters: Vec<String>,
    llvm_profdata_path: Option<PathBuf>,
    llvm_cov_path: Option<PathBuf>,
}

impl CoverageClient {
    pub fn new(namespace: impl Into<String>, output_dir: impl Into<PathBuf>) -> Self {
        Self {
            namespace: namespace.into(),
            output_dir: output_dir.into(),
            source_dir: None,
            binary_path: None,
            filters: vec!["coverage_server".to_string()],
            llvm_profdata_path: None,
            llvm_cov_path: None,
        }
    }

    /// Set the local source directory for path remapping in reports.
    pub fn set_source_dir(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.source_dir = Some(path.into());
        self
    }

    /// Set the path to the instrumented binary (needed for llvm-cov).
    pub fn set_binary_path(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.binary_path = Some(path.into());
        self
    }

    /// Set custom path to llvm-profdata tool.
    pub fn set_llvm_profdata_path(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.llvm_profdata_path = Some(path.into());
        self
    }

    /// Set custom path to llvm-cov tool.
    pub fn set_llvm_cov_path(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.llvm_cov_path = Some(path.into());
        self
    }

    /// Add a filter pattern to exclude from coverage reports.
    pub fn add_filter(&mut self, pattern: impl Into<String>) -> &mut Self {
        self.filters.push(pattern.into());
        self
    }

    /// Set the filter patterns (replaces defaults).
    pub fn set_filters(&mut self, patterns: Vec<String>) -> &mut Self {
        self.filters = patterns;
        self
    }

    /// Find a pod by label selector in the configured namespace.
    pub fn get_pod_name(&self, label_selector: &str) -> Result<String> {
        let output = Command::new("kubectl")
            .args([
                "get",
                "pods",
                "-n",
                &self.namespace,
                "-l",
                label_selector,
                "--field-selector=status.phase=Running",
                "-o",
                "jsonpath={.items[0].metadata.name}",
            ])
            .output()
            .map_err(|e| CoverageError::Kubectl(format!("Failed to run kubectl: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CoverageError::Kubectl(format!(
                "kubectl get pods failed: {}",
                stderr
            )));
        }

        let pod_name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if pod_name.is_empty() {
            return Err(CoverageError::Kubectl(format!(
                "No running pod found with selector: {}",
                label_selector
            )));
        }

        Ok(pod_name)
    }

    /// Collect coverage from a pod using kubectl port-forward.
    pub async fn collect_from_pod(
        &self,
        pod_name: &str,
        test_name: &str,
        target_port: u16,
    ) -> Result<PathBuf> {
        let test_dir = self.output_dir.join(test_name);
        std::fs::create_dir_all(&test_dir)?;

        // Start port-forward
        let mut port_forward = Command::new("kubectl")
            .args([
                "port-forward",
                "-n",
                &self.namespace,
                &format!("pod/{}", pod_name),
                &format!("0:{}", target_port),
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                CoverageError::Kubectl(format!("Failed to start port-forward: {}", e))
            })?;

        // Wait for port-forward to be ready and extract local port
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Read stderr to find the forwarded port
        // kubectl outputs: "Forwarding from 127.0.0.1:XXXXX -> 9095"
        let local_port = self.detect_forwarded_port(&port_forward).unwrap_or(target_port);

        let result = self
            .collect_from_url(
                &format!("http://127.0.0.1:{}/coverage", local_port),
                test_name,
            )
            .await;

        // Kill port-forward
        let _ = port_forward.kill();
        let _ = port_forward.wait();

        let profraw_path = result?;

        // Save metadata
        let metadata = CollectionMetadata {
            pod_name: pod_name.to_string(),
            namespace: self.namespace.clone(),
            container: None,
            test_name: test_name.to_string(),
            timestamp: chrono_timestamp(),
            binary_path: self.binary_path.as_ref().map(|p| p.display().to_string()),
        };
        let metadata_path = test_dir.join("metadata.json");
        std::fs::write(&metadata_path, serde_json::to_string_pretty(&metadata)?)?;

        Ok(profraw_path)
    }

    /// Collect coverage directly from a URL (no port-forwarding).
    pub async fn collect_from_url(&self, url: &str, test_name: &str) -> Result<PathBuf> {
        let test_dir = self.output_dir.join(test_name);
        std::fs::create_dir_all(&test_dir)?;

        info!("Collecting coverage from {}", url);

        let client = reqwest::Client::new();
        let resp = client
            .post(url)
            .json(&serde_json::json!({"test_name": test_name}))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CoverageError::Other(format!(
                "Coverage server returned {}: {}",
                status, body
            )));
        }

        let coverage: CoverageResponse = resp.json().await?;

        if !coverage.coverage_enabled {
            return Err(CoverageError::CoverageNotEnabled);
        }

        let profraw_bytes =
            base64::engine::general_purpose::STANDARD.decode(&coverage.profraw_data)?;

        let profraw_path = test_dir.join(&coverage.profraw_filename);
        std::fs::write(&profraw_path, &profraw_bytes)?;

        info!(
            "Saved profraw ({} bytes) to {:?}",
            profraw_bytes.len(),
            profraw_path
        );

        Ok(profraw_path)
    }

    /// Merge profraw files into a single profdata file using llvm-profdata.
    pub fn merge_profdata(&self, test_name: &str) -> Result<PathBuf> {
        let test_dir = self.output_dir.join(test_name);
        let profdata_path = test_dir.join("coverage.profdata");

        let profraw_files: Vec<PathBuf> = std::fs::read_dir(&test_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "profraw"))
            .collect();

        if profraw_files.is_empty() {
            return Err(CoverageError::Other(
                "No profraw files found in test directory".to_string(),
            ));
        }

        let llvm_profdata = self.find_llvm_profdata()?;

        let mut cmd = Command::new(&llvm_profdata);
        cmd.arg("merge").arg("--sparse");
        for f in &profraw_files {
            cmd.arg(f);
        }
        cmd.arg("-o").arg(&profdata_path);

        info!("Running: {:?}", cmd);
        let output = cmd.output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CoverageError::LlvmProfdata(format!(
                "llvm-profdata merge failed: {}",
                stderr
            )));
        }

        info!("Generated profdata: {:?}", profdata_path);
        Ok(profdata_path)
    }

    /// Generate a text coverage report using llvm-cov.
    pub fn generate_text_report(&self, test_name: &str) -> Result<PathBuf> {
        let test_dir = self.output_dir.join(test_name);
        let profdata_path = test_dir.join("coverage.profdata");
        let report_path = test_dir.join("coverage.txt");

        let binary = self.binary_path.as_ref().ok_or_else(|| {
            CoverageError::Other(
                "Binary path required for llvm-cov report. Call set_binary_path() first."
                    .to_string(),
            )
        })?;

        let llvm_cov = self.find_llvm_cov()?;

        let mut cmd = Command::new(&llvm_cov);
        cmd.args(["report", "--use-color=false"])
            .arg(format!("--instr-profile={}", profdata_path.display()))
            .arg(binary);

        if let Some(ref source_dir) = self.source_dir {
            cmd.arg(format!("--source-dir={}", source_dir.display()));
        }

        // Apply ignore filters
        for filter in &self.filters {
            cmd.arg(format!("--ignore-filename-regex={}", filter));
        }

        info!("Running: {:?}", cmd);
        let output = cmd.output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CoverageError::LlvmCov(format!(
                "llvm-cov report failed: {}",
                stderr
            )));
        }

        std::fs::write(&report_path, &output.stdout)?;
        info!("Generated text report: {:?}", report_path);

        Ok(report_path)
    }

    /// Generate an HTML coverage report using llvm-cov.
    pub fn generate_html_report(&self, test_name: &str) -> Result<PathBuf> {
        let test_dir = self.output_dir.join(test_name);
        let profdata_path = test_dir.join("coverage.profdata");
        let html_dir = test_dir.join("html");

        let binary = self.binary_path.as_ref().ok_or_else(|| {
            CoverageError::Other(
                "Binary path required for llvm-cov. Call set_binary_path() first.".to_string(),
            )
        })?;

        let llvm_cov = self.find_llvm_cov()?;

        let mut cmd = Command::new(&llvm_cov);
        cmd.args(["show", "--format=html"])
            .arg(format!("--instr-profile={}", profdata_path.display()))
            .arg(format!("--output-dir={}", html_dir.display()))
            .arg(binary);

        if let Some(ref source_dir) = self.source_dir {
            cmd.arg(format!("--source-dir={}", source_dir.display()));
        }

        for filter in &self.filters {
            cmd.arg(format!("--ignore-filename-regex={}", filter));
        }

        info!("Running: {:?}", cmd);
        let output = cmd.output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CoverageError::LlvmCov(format!(
                "llvm-cov show --format=html failed: {}",
                stderr
            )));
        }

        info!("Generated HTML report in: {:?}", html_dir);
        Ok(html_dir)
    }

    /// Generate an lcov-format report (useful for Codecov, SonarCloud, etc).
    pub fn generate_lcov_report(&self, test_name: &str) -> Result<PathBuf> {
        let test_dir = self.output_dir.join(test_name);
        let profdata_path = test_dir.join("coverage.profdata");
        let lcov_path = test_dir.join("lcov.info");

        let binary = self.binary_path.as_ref().ok_or_else(|| {
            CoverageError::Other(
                "Binary path required for llvm-cov. Call set_binary_path() first.".to_string(),
            )
        })?;

        let llvm_cov = self.find_llvm_cov()?;

        let mut cmd = Command::new(&llvm_cov);
        cmd.args(["export", "--format=lcov"])
            .arg(format!("--instr-profile={}", profdata_path.display()))
            .arg(binary);

        if let Some(ref source_dir) = self.source_dir {
            cmd.arg(format!("--source-dir={}", source_dir.display()));
        }

        for filter in &self.filters {
            cmd.arg(format!("--ignore-filename-regex={}", filter));
        }

        info!("Running: {:?}", cmd);
        let output = cmd.output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CoverageError::LlvmCov(format!(
                "llvm-cov export --format=lcov failed: {}",
                stderr
            )));
        }

        std::fs::write(&lcov_path, &output.stdout)?;
        info!("Generated lcov report: {:?}", lcov_path);

        Ok(lcov_path)
    }

    /// Run the full coverage processing pipeline:
    /// merge profdata -> text report -> HTML report -> lcov report
    pub fn process_reports(&self, test_name: &str) -> Result<()> {
        self.merge_profdata(test_name)?;
        self.generate_text_report(test_name)?;
        self.generate_html_report(test_name)?;
        self.generate_lcov_report(test_name)?;
        Ok(())
    }

    /// Print a coverage summary to stdout.
    pub fn print_summary(&self, test_name: &str) -> Result<()> {
        let report_path = self.output_dir.join(test_name).join("coverage.txt");
        if report_path.exists() {
            let content = std::fs::read_to_string(&report_path)?;
            println!("=== Coverage Summary for '{}' ===", test_name);
            println!("{}", content);
        } else {
            warn!("No coverage report found at {:?}", report_path);
        }
        Ok(())
    }

    /// Reset coverage counters on the remote server.
    pub async fn reset_counters(&self, url: &str) -> Result<()> {
        let client = reqwest::Client::new();
        let resp = client.post(url).send().await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(CoverageError::Other(format!(
                "Failed to reset counters: {}",
                body
            )));
        }

        info!("Coverage counters reset");
        Ok(())
    }

    fn find_llvm_profdata(&self) -> Result<PathBuf> {
        if let Some(ref path) = self.llvm_profdata_path {
            return Ok(path.clone());
        }
        find_llvm_tool("llvm-profdata")
    }

    fn find_llvm_cov(&self) -> Result<PathBuf> {
        if let Some(ref path) = self.llvm_cov_path {
            return Ok(path.clone());
        }
        find_llvm_tool("llvm-cov")
    }

    fn detect_forwarded_port(&self, _child: &std::process::Child) -> Option<u16> {
        // In practice, we'd parse stderr output from kubectl port-forward.
        // For now, return None to fall back to target_port (which works
        // when kubectl forwards to the same port).
        None
    }
}

/// Find an LLVM tool by searching common paths.
fn find_llvm_tool(name: &str) -> Result<PathBuf> {
    // Try the tool directly (it might be in PATH)
    if let Ok(path) = which::which(name) {
        return Ok(path);
    }

    // Try versioned names (llvm-profdata-17, llvm-profdata-18, etc.)
    for version in (14..=20).rev() {
        let versioned = format!("{}-{}", name, version);
        if let Ok(path) = which::which(&versioned) {
            return Ok(path);
        }
    }

    // Try rust toolchain path
    if let Ok(rustup_home) = std::env::var("RUSTUP_HOME") {
        let toolchain_path = PathBuf::from(rustup_home).join("toolchains");
        if let Ok(entries) = std::fs::read_dir(&toolchain_path) {
            for entry in entries.flatten() {
                let tool_path = entry.path().join("lib").join("rustlib").join(
                    std::env::consts::ARCH.to_string()
                        + "-unknown-linux-gnu/bin/"
                        + name,
                );
                if tool_path.exists() {
                    return Ok(tool_path);
                }
            }
        }
    }

    // Try cargo-binutils style (rust-profdata, rust-cov)
    let rust_name = name.replace("llvm-", "rust-");
    if let Ok(path) = which::which(&rust_name) {
        return Ok(path);
    }

    Err(CoverageError::ToolNotFound(format!(
        "Could not find '{}'. Install LLVM tools or use `rustup component add llvm-tools-preview` \
         and `cargo install cargo-binutils`.",
        name
    )))
}

fn chrono_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s", now.as_secs())
}
