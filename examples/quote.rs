use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::VersionedTransaction;
use solana_sdk::{pubkey, pubkey::Pubkey};
use std::sync::Arc;
use raydium_swap::amm::executor::{RaydiumAmm, RaydiumAmmExecutorOpts};
use raydium_swap::api_v3::ApiV3Client;
use raydium_swap::types::{SwapExecutionMode, SwapInput};

// 定义常量,用于指定输入和输出代币,其中USDC和SOL是Solana上的代币,这两个地址是他们的mint地址
// mint地址是代币在Solana网络上的唯一标识符
const USDC: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
const SOL: Pubkey = pubkey!("So11111111111111111111111111111111111111112");

// 主函数,使用tokio运行时来执行异步代码
// 该函数首先加载环境变量,初始化日志记录器,然后创建一个RPC客户端和Raydium AMM执行器
// 接着定义一个交换输入,包括输入和输出代币的mint地址,滑点,金额,执行模式和市场
// 然后调用执行器的quote方法获取交换报价,并打印出来
// 最后创建一个新的密钥对,构建交换交易,设置最新的区块哈希,并尝试创建一个版本化交易
#[tokio::main]
pub async fn main() -> anyhow::Result<()> {
    dotenv::dotenv()?;
    env_logger::init();
// create a new RPC client and Raydium AMM executor,and the RPC_URL is read from the 
// environment variable "RPC_URL"(which should be set in the .env file)
    let client = Arc::new(RpcClient::new(std::env::var("RPC_URL")?));
    let executor = RaydiumAmm::new(
        Arc::clone(&client),
        RaydiumAmmExecutorOpts::default(),
        ApiV3Client::new(None),
    );
    let swap_input = SwapInput {
        input_token_mint: SOL,
        output_token_mint: USDC,
        slippage_bps: 1000,    // 10%
        amount: 1_000_000_000, // 1 SOL
        mode: SwapExecutionMode::ExactIn,
        market: None,
    };

    let quote = executor.quote(&swap_input).await?;
    log::info!("Quote: {:#?}", quote);

    let keypair = Keypair::new();
    let mut transaction = executor
        .swap_transaction(keypair.pubkey(), quote, None)
        .await?;
    let blockhash = client.get_latest_blockhash().await?;
    transaction.message.set_recent_blockhash(blockhash);
    let _final_tx = VersionedTransaction::try_new(transaction.message, &[&keypair])?;

    Ok(())
}
