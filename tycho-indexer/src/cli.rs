use clap::{Args, Parser, Subcommand};
use tycho_core::models::Chain;

/// Tycho Indexer using Substreams
///
/// Extracts state from the Ethereum blockchain and stores it in a Postgres database.
#[derive(Parser, PartialEq, Debug)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
pub struct Cli {
    #[command(flatten)]
    global_args: GlobalArgs,
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    pub fn args(&self) -> GlobalArgs {
        self.global_args.clone()
    }

    pub fn command(&self) -> Command {
        self.command.clone()
    }
}

#[derive(Subcommand, Clone, PartialEq, Debug)]
pub enum Command {
    /// Starts the indexing service.
    Index(IndexArgs),
    /// Runs a single substream, intended for testing.
    Run(RunSpkgArgs),
    /// Starts a job to analyze stored tokens for tax and gas cost.
    AnalyzeTokens(AnalyzeTokenArgs),
}

#[derive(Parser, Debug, Clone, PartialEq, Eq)]
#[command(version, about, long_about = None)]
pub struct GlobalArgs {
    /// Ethereum node rpc url
    #[clap(env, long)]
    pub rpc_url: String,

    /// Substreams API token
    #[clap(long, env, hide_env_values = true, alias = "api_token")]
    pub substreams_api_token: String,

    /// PostgresDB Connection Url
    #[clap(
        long,
        env,
        hide_env_values = true,
        default_value = "postgres://postgres:mypassword@localhost:5432/tycho_indexer_0"
    )]
    pub database_url: String,

    /// Substreams API endpoint
    #[clap(name = "endpoint", long, default_value = "https://mainnet.eth.streamingfast.io")]
    pub endpoint_url: String,
}

#[derive(Args, Debug, Clone, PartialEq)]
pub struct IndexArgs {
    /// Extractors configuration file
    #[clap(long, env, default_value = "./extractors.yaml")]
    pub extractors_config: String,
}

#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct RunSpkgArgs {
    /// Substreams Package file
    #[clap(long)]
    pub spkg: String,

    /// Substreams Module name
    #[clap(long)]
    pub module: String,

    /// Substreams start block
    #[clap(long)]
    pub start_block: i64,

    /// Substreams stop block
    ///
    /// Optional. If not provided, the extractor will run until the latest block.
    /// If prefixed with a `+` the value is interpreted as an increment to the start block.
    /// Defaults to STOP_BLOCK env var or None.
    #[clap(long)]
    stop_block: Option<String>,
}

impl RunSpkgArgs {
    #[allow(dead_code)]
    fn stop_block(&self) -> Option<i64> {
        if let Some(s) = &self.stop_block {
            if s.starts_with('+') {
                let increment: i64 = s
                    .strip_prefix('+')
                    .expect("stripped stop block value")
                    .parse()
                    .expect("stop block value");
                Some(self.start_block + increment)
            } else {
                Some(s.parse().expect("stop block value"))
            }
        } else {
            None
        }
    }
}

#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct AnalyzeTokenArgs {
    /// Blockchain to execute analysis for.
    #[clap(long)]
    pub chain: Chain,
    /// How many concurrent threads to use for token analysis.
    #[clap(long)]
    pub concurrency: usize,
    /// How many tokens to update in a batch per thread.
    #[clap(long)]
    pub update_batch_size: usize,
    /// How many tokens to fetch from the db to distribute to threads (page size). This
    /// should be at least `concurrency * update_batch_size`.
    #[clap(long)]
    pub fetch_batch_size: usize,
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    #[tokio::test]
    async fn test_arg_parsing_run_cmd() {
        let cli = Cli::try_parse_from(vec![
            "tycho-indexer",
            "--endpoint",
            "http://example.com",
            "--api_token",
            "your_api_token",
            "--database-url",
            "my_db",
            "--rpc-url",
            "http://example.com",
            "run",
            "--spkg",
            "package.spkg",
            "--module",
            "module_name",
            "--start-block",
            "17361664",
        ])
        .expect("parse errored");

        let expected_args = Cli {
            global_args: GlobalArgs {
                endpoint_url: "http://example.com".to_string(),
                substreams_api_token: "your_api_token".to_string(),
                database_url: "my_db".to_string(),
                rpc_url: "http://example.com".to_string(),
            },
            command: Command::Run(RunSpkgArgs {
                spkg: "package.spkg".to_string(),
                module: "module_name".to_string(),
                start_block: 17361664,
                stop_block: None,
            }),
        };

        assert_eq!(cli, expected_args);
    }

    #[tokio::test]
    async fn test_arg_parsing_index_cmd() {
        let cli = Cli::try_parse_from(vec![
            "tycho-indexer",
            "--endpoint",
            "http://example.com",
            "--api_token",
            "your_api_token",
            "--database-url",
            "my_db",
            "--rpc-url",
            "http://example.com",
            "index",
            "--extractors-config",
            "/opt/extractors.yaml",
        ])
        .expect("parse errored");

        let expected_args = Cli {
            global_args: GlobalArgs {
                endpoint_url: "http://example.com".to_string(),
                substreams_api_token: "your_api_token".to_string(),
                database_url: "my_db".to_string(),
                rpc_url: "http://example.com".to_string(),
            },
            command: Command::Index(IndexArgs {
                extractors_config: "/opt/extractors.yaml".to_string(),
            }),
        };

        assert_eq!(cli, expected_args);
    }

    #[test]
    fn test_arg_parsing_missing_val() {
        let args = Cli::try_parse_from(vec![
            "tycho-indexer",
            "--spkg",
            "package.spkg",
            "--module",
            "module_name",
        ]);

        assert!(args.is_err());
    }
}