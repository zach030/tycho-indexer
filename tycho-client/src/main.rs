use std::time::Duration;

use clap::Parser;
use tracing_appender::rolling::{self};

use tycho_client::{
    deltas::DeltasClient,
    feed::{
        component_tracker::ComponentFilter, synchronizer::ProtocolStateSynchronizer,
        BlockSynchronizer,
    },
    HttpRPCClient, WsDeltasClient,
};
use tycho_core::dto::{Chain, ExtractorIdentity};

#[derive(Parser, Debug, Clone, PartialEq, Eq)]
#[clap(version = "0.1.0")]
struct CliArgs {
    /// Tycho server URL, without protocol. Example: localhost:8888
    #[clap(long, default_value = "localhost:8888")]
    tycho_url: String,

    /// Specifies exchanges and optionally a pool address in the format name:address
    #[clap(long, number_of_values = 1)]
    exchange: Vec<String>,

    /// Specifies the minimum TVL to filter the components. Ignored if addresses are provided.
    #[clap(long, default_value = "10")]
    min_tvl: u32,

    /// Specifies the client's block time
    #[clap(long, default_value = "600")]
    block_time: u64,

    /// Specifies the client's timeout
    #[clap(long, default_value = "1")]
    timeout: u64,

    /// Logging folder path.
    #[clap(long, default_value = "logs")]
    log_folder: String,

    /// Run the example on a single block with UniswapV2 and UniswapV3.
    #[clap(long)]
    example: bool,

    /// If set, only component and tokens are streamed, any snapshots or state updates
    /// are omitted from the stream.
    #[clap(long)]
    no_state: bool,
}

#[tokio::main]
async fn main() {
    // Parse CLI Args
    let args: CliArgs = CliArgs::parse();

    // Setup Logging
    let (non_blocking, _guard) =
        tracing_appender::non_blocking(rolling::never(args.log_folder, "dev_logs.log"));
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().expect("Bad env filter"),
        )
        .with_writer(non_blocking)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set up logging subscriber");

    // Runs example if flag is set.
    if args.example {
        // Run a simple example of a block synchronizer.
        //
        // You need to port-forward tycho before running this:
        //
        // ```bash
        // kubectl port-forward deploy/tycho-indexer 8888:4242
        // ```
        let tycho_url = "localhost:8888".to_string();
        let exchanges = vec![
            (
                "uniswap_v3".to_string(),
                Some("0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640".to_string()),
            ),
            (
                "uniswap_v2".to_string(),
                Some("0xa478c2975ab1ea89e8196811f51a7b7ade33eb11".to_string()),
            ),
        ];
        run(tycho_url, exchanges, 0.0, 600, 1, true).await;
        return;
    }

    // Parse exchange name and addresses from name:address format.
    let exchanges: Vec<(String, Option<String>)> = args
        .exchange
        .iter()
        .filter_map(|e| {
            if e.contains('-') {
                let parts: Vec<&str> = e.split('-').collect();
                if parts.len() == 2 {
                    Some((parts[0].to_string(), Some(parts[1].to_string())))
                } else {
                    tracing::warn!("Ignoring invalid exchange format: {}", e);
                    None
                }
            } else {
                Some((e.to_string(), None))
            }
        })
        .collect();

    tracing::info!("Running with exchanges: {:?}", exchanges);

    run(
        args.tycho_url,
        exchanges,
        args.min_tvl.into(),
        args.block_time,
        args.timeout,
        !args.no_state,
    )
    .await;
}

async fn run(
    tycho_url: String,
    exchanges: Vec<(String, Option<String>)>,
    tvl: f64,
    block_time: u64,
    timeout: u64,
    include_state: bool,
) {
    let tycho_ws_url = format!("ws://{tycho_url}");
    let tycho_rpc_url = format!("http://{tycho_url}");
    let ws_client = WsDeltasClient::new(&tycho_ws_url).unwrap();
    ws_client
        .connect()
        .await
        .expect("ws client connection error");

    let mut block_sync =
        BlockSynchronizer::new(Duration::from_secs(block_time), Duration::from_secs(timeout));

    for (name, address) in exchanges {
        let id = ExtractorIdentity { chain: Chain::Ethereum, name: name.clone() };
        let filter = if address.is_some() {
            ComponentFilter::Ids(vec![address.unwrap()])
        } else {
            ComponentFilter::MinimumTVL(tvl)
        };
        let is_native: bool = !name.starts_with("vm:");
        let sync = ProtocolStateSynchronizer::new(
            id.clone(),
            is_native,
            true,
            filter,
            1,
            include_state,
            HttpRPCClient::new(&tycho_rpc_url).unwrap(),
            ws_client.clone(),
        );
        block_sync = block_sync.register_synchronizer(id, sync);
    }

    let (jh, mut rx) = block_sync
        .run()
        .await
        .expect("block sync start error");

    while let Some(msg) = rx.recv().await {
        if let Ok(msg_json) = serde_json::to_string(&msg) {
            println!("{}", msg_json);
        } else {
            tracing::error!("Failed to serialize FeedMessage");
        }
    }

    tracing::debug!("RX closed");
    jh.await.unwrap();
}

#[cfg(test)]
mod cli_tests {
    use clap::Parser;

    use super::CliArgs;

    #[tokio::test]
    async fn test_cli_args() {
        let args = CliArgs::parse_from([
            "tycho-client",
            "--tycho-url",
            "localhost:5000",
            "--exchange",
            "uniswap_v2",
            "--min-tvl",
            "3000",
            "--block-time",
            "50",
            "--timeout",
            "5",
            "--log-folder",
            "test_logs",
            "--example",
        ]);
        let exchanges: Vec<String> = vec!["uniswap_v2".to_string()];
        assert_eq!(args.tycho_url, "localhost:5000");
        assert_eq!(args.exchange, exchanges);
        assert_eq!(args.min_tvl, 3000);
        assert_eq!(args.block_time, 50);
        assert_eq!(args.timeout, 5);
        assert_eq!(args.log_folder, "test_logs");
        assert!(args.example);
    }
}
