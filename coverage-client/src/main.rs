use clap::{Parser, Subcommand};
use coverage_client::CoverageClient;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "coverage-client")]
#[command(about = "Collect Rust code coverage from instrumented applications")]
struct Cli {
    /// Kubernetes namespace
    #[arg(short, long, default_value = "default")]
    namespace: String,

    /// Output directory for coverage data
    #[arg(short, long, default_value = "./coverage-output")]
    output_dir: String,

    /// Path to the instrumented binary (required for report generation)
    #[arg(short, long)]
    binary: Option<String>,

    /// Local source directory for path remapping
    #[arg(short, long)]
    source_dir: Option<String>,

    /// Path to llvm-profdata tool
    #[arg(long)]
    llvm_profdata: Option<String>,

    /// Path to llvm-cov tool
    #[arg(long)]
    llvm_cov: Option<String>,

    /// Filter patterns to exclude from reports
    #[arg(long)]
    filter: Vec<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Collect coverage from a pod via port-forward
    Collect {
        /// Pod name (or use --selector to find it)
        #[arg(long)]
        pod: Option<String>,

        /// Label selector to find the pod
        #[arg(long, short)]
        selector: Option<String>,

        /// Target port for the coverage server
        #[arg(long, default_value = "9095")]
        port: u16,

        /// Test name (used as subdirectory name)
        #[arg(long, default_value = "e2e-tests")]
        test_name: String,
    },

    /// Collect coverage directly from a URL
    CollectUrl {
        /// Coverage server URL
        #[arg(long)]
        url: String,

        /// Test name (used as subdirectory name)
        #[arg(long, default_value = "e2e-tests")]
        test_name: String,
    },

    /// Process collected coverage data into reports
    Report {
        /// Test name (subdirectory with profraw files)
        #[arg(long, default_value = "e2e-tests")]
        test_name: String,
    },

    /// Collect and immediately generate reports (full pipeline)
    CollectAndReport {
        /// Pod name (or use --selector to find it)
        #[arg(long)]
        pod: Option<String>,

        /// Label selector to find the pod
        #[arg(long, short)]
        selector: Option<String>,

        /// Target port for the coverage server
        #[arg(long, default_value = "9095")]
        port: u16,

        /// Test name
        #[arg(long, default_value = "e2e-tests")]
        test_name: String,
    },

    /// Reset coverage counters on the server
    Reset {
        /// Coverage server URL (e.g. http://localhost:9095/coverage/reset)
        #[arg(long)]
        url: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let mut client = CoverageClient::new(&cli.namespace, &cli.output_dir);

    if let Some(ref binary) = cli.binary {
        client.set_binary_path(binary);
    }
    if let Some(ref source_dir) = cli.source_dir {
        client.set_source_dir(source_dir);
    }
    if let Some(ref path) = cli.llvm_profdata {
        client.set_llvm_profdata_path(path);
    }
    if let Some(ref path) = cli.llvm_cov {
        client.set_llvm_cov_path(path);
    }
    if !cli.filter.is_empty() {
        client.set_filters(cli.filter);
    }

    match cli.command {
        Commands::Collect {
            pod,
            selector,
            port,
            test_name,
        } => {
            let pod_name = resolve_pod(&client, pod, selector)?;
            let path = client.collect_from_pod(&pod_name, &test_name, port).await?;
            println!("Coverage collected: {}", path.display());
        }

        Commands::CollectUrl { url, test_name } => {
            let path = client.collect_from_url(&url, &test_name).await?;
            println!("Coverage collected: {}", path.display());
        }

        Commands::Report { test_name } => {
            client.process_reports(&test_name)?;
            client.print_summary(&test_name)?;
        }

        Commands::CollectAndReport {
            pod,
            selector,
            port,
            test_name,
        } => {
            let pod_name = resolve_pod(&client, pod, selector)?;
            client.collect_from_pod(&pod_name, &test_name, port).await?;
            client.process_reports(&test_name)?;
            client.print_summary(&test_name)?;
        }

        Commands::Reset { url } => {
            client.reset_counters(&url).await?;
            println!("Counters reset successfully");
        }
    }

    Ok(())
}

fn resolve_pod(
    client: &CoverageClient,
    pod: Option<String>,
    selector: Option<String>,
) -> anyhow::Result<String> {
    match (pod, selector) {
        (Some(name), _) => Ok(name),
        (None, Some(sel)) => Ok(client.get_pod_name(&sel)?),
        (None, None) => {
            anyhow::bail!("Either --pod or --selector must be provided")
        }
    }
}
