use crate::api_v3::response::{ApiV3PoolsPage, ApiV3StandardPool, ApiV3StandardPoolKeys};
use crate::api_v3::{ApiV3Client, PoolFetchParams, PoolSort, PoolSortOrder, PoolType};
use crate::builder::SwapInstructionsBuilder;
use crate::types::{
    ComputeUnitLimits, PriorityFeeConfig, SwapConfig, SwapConfigOverrides, SwapInput,
};
use std::sync::Arc;

use anyhow::{anyhow, Context};
use arrayref::array_ref;
use raydium_library::amm::AmmKeys;
use safe_transmute::{transmute_one_pedantic, transmute_to_bytes};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::account_info::IntoAccountInfo;
use solana_sdk::instruction::Instruction;
use solana_sdk::program_pack::Pack;
use solana_sdk::transaction::VersionedTransaction;
use solana_sdk::{pubkey, pubkey::Pubkey};

const RAYDIUM_LIQUIDITY_POOL_V4_PROGRAM_ID: Pubkey =
    pubkey!("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
// // https://api-v3.raydium.io/pools/info/mint?mint1=So11111111111111111111111111111111111111112&mint2=EKpQGSJtjMFqKZ9KQanSqYXRcF8fBopzLHYxdM65zcjm&poolType=standard&poolSortField=liquidity&sortType=desc&pageSize=100&page=1

#[derive(Clone)]
pub struct RaydiumAmm {
    client: Arc<RpcClient>,
    api: ApiV3Client,
    config: SwapConfig,
    load_keys_by_api: bool,
}

// todo: Builder pattern for this
#[derive(Default)]
pub struct RaydiumAmmExecutorOpts {
    pub priority_fee: Option<PriorityFeeConfig>,
    pub cu_limits: Option<ComputeUnitLimits>,
    pub wrap_and_unwrap_sol: Option<bool>,
    pub load_keys_by_api: Option<bool>,
}

impl RaydiumAmmExecutorOpts {
    pub fn new() -> Self {
        Self {
            priority_fee: None,
            cu_limits: None,
            wrap_and_unwrap_sol: Some(true),
            load_keys_by_api: Some(true),
        }
    }
    
}
impl RaydiumAmm {
    // 构建一个新的 RaydiumAmm 实例
    // 该函数接受一个 Arc<RpcClient>、RaydiumAmmExecutorOpts 和 ApiV3Client 作为参数
    //  Arc<RpcClient> 用于与 Solana 区块链进行交互,Arc 是一个线程安全的智能指针,用于在多个线程之间共享数据,这样RpcClient 可以在多个线程中安全地使用
    // RaydiumAmmExecutorOpts 是 Raydium AMM 执行器的配置选项,包括优先费用、计算单位限制、是否包装和解包 SOL 以及是否通过 API 加载密钥
    // 这样设计可以使 RaydiumAmm 的实例化更加灵活,允许用户根据自己的需求配置执行器的行为,方便后期维护和扩展,而不必修改 Raydium 的字段和方法
    // ApiV3Client 用于与 Raydium API 进行交互,获取有关池和市场的信息
    pub fn new(client: Arc<RpcClient>, config: RaydiumAmmExecutorOpts, api: ApiV3Client) -> Self {
        let RaydiumAmmExecutorOpts {
            priority_fee,
            cu_limits,
            wrap_and_unwrap_sol,
            load_keys_by_api,
        } = config;
        Self {
            client,
            api,
            load_keys_by_api: load_keys_by_api.unwrap_or(true),
            config: SwapConfig {
                priority_fee,
                cu_limits,
                wrap_and_unwrap_sol,
                as_legacy_transaction: Some(true),
            },
        }
    }

    // quote 方法用于获取 Raydium AMM 的交换报价
    // 它接受一个 SwapInput 结构体作为参数,该结构体包含输入代币的 mint 地址、输出代币的 mint 地址、滑点、交换金额、执行模式和市场信息
    // 返回一个 RaydiumAmmQuote 结构体,该结构体包含交换的详细信息,包括市场地址、输入和输出代币的 mint 地址、交换金额、其他金额、滑点等
    // 如果输入代币和输出代币相同,则返回一个错误
    // 如果 swap_input.market 为 None,则通过 API 获取市场信息
    // 如果 swap_input.market 已经有值,则直接使用它
    // 通过检查输入和输出代币的 mint 地址是否匹配来决定交换的方向
    // 如果输入代币的 mint 地址与 AMM 的 coin mint 匹配,则交换方向为 Coin2PC,否则为 PC2Coin
    // 通过调用 raydium_library::amm::swap_with_slippage 函数计算交换的其他金额和其他金额阈值
    // 最后返回一个 RaydiumAmmQuote 结构体,包含交换的详细信息       

    pub async fn quote(&self, swap_input: &SwapInput) -> anyhow::Result<RaydiumAmmQuote> {
        // 为什么要检查输入和输出代币是否相同？
        // 因为在交换过程中，输入代币和输出代币必须是不同的，否则没有意义。
        if swap_input.input_token_mint == swap_input.output_token_mint {
            return Err(anyhow!(
                "Input token cannot equal output token {}",
                swap_input.input_token_mint
            ));
        }
// 通过检查 swap_input.market 是否为 None 来决定是否需要通过 API 获取市场信息
        // 如果 swap_input.market 为 None，则通过 API 获取市场信息
        // 如果 swap_input.market 已经有值，则直接使用它
        let mut pool_id = swap_input.market;
        if pool_id.is_none() {
            let response: ApiV3PoolsPage<ApiV3StandardPool> = self
                .api
                .fetch_pool_by_mints(
                    &swap_input.input_token_mint,
                    Some(&swap_input.output_token_mint),
                    &PoolFetchParams {
                        pool_type: PoolType::Standard,
                        pool_sort: PoolSort::Liquidity,
                        sort_type: PoolSortOrder::Descending,
                        page_size: 10,
                        page: 1,
                    },
                )
                .await?;
            pool_id = response.pools.into_iter().find_map(|pool| {
                if pool.mint_a.address == swap_input.input_token_mint
                    && pool.mint_b.address == swap_input.output_token_mint
                    || pool.mint_a.address == swap_input.output_token_mint
                        && pool.mint_b.address == swap_input.input_token_mint
                        && pool.program_id == RAYDIUM_LIQUIDITY_POOL_V4_PROGRAM_ID
                {
                    Some(pool.id)
                } else {
                    None
                }
            });
        }

        let Some(pool_id) = pool_id else {
            return Err(anyhow!("Failed to get market for swap"));
        };

        let (amm_keys, market_keys) = if self.load_keys_by_api {
            let response = self
                .api
                .fetch_pool_keys_by_ids::<ApiV3StandardPoolKeys>(
                    [&pool_id].into_iter().map(|id| id.to_string()).collect(),
                )
                .await?;
            let keys = response.first().context(format!(
                "Failed to get pool keys for raydium standard pool {}",
                pool_id
            ))?;

            (AmmKeys::try_from(keys)?, MarketKeys::try_from(keys)?)
        } else {
            let amm_keys = raydium_library::amm::utils::load_amm_keys(
                &self.client,
                &RAYDIUM_LIQUIDITY_POOL_V4_PROGRAM_ID,
                &pool_id,
            )
            .await?;

            let market_keys = MarketKeys::from(
                &raydium_library::amm::openbook::get_keys_for_market(
                    &self.client,
                    &amm_keys.market_program,
                    &amm_keys.market,
                )
                .await?,
            );

            (amm_keys, market_keys)
        };

        // reload accounts data to calculate amm pool vault amount
        // get multiple accounts at the same time to ensure data consistency
        let load_pubkeys = vec![
            pool_id,
            amm_keys.amm_target,
            amm_keys.amm_pc_vault,
            amm_keys.amm_coin_vault,
            amm_keys.amm_open_order,
            amm_keys.market,
            market_keys.event_queue,
        ];
        let rsps = crate::utils::get_multiple_account_data(&self.client, &load_pubkeys).await?;
        let accounts = array_ref![rsps, 0, 7];
        let [amm_account, amm_target_account, amm_pc_vault_account, amm_coin_vault_account, amm_open_orders_account, market_account, market_event_q_account] =
            accounts;
        let amm_account_unpacked = match amm_account.as_ref() {
            Some(account) => account,
            None => {
                return Err(anyhow!(
                    "Failed to get amm account for pool {}",
                    pool_id
                ));
            }
        };
        let amm: raydium_amm::state::AmmInfo = transmute_one_pedantic::<super::amm_info::AmmInfo>(
            transmute_to_bytes(&amm_account_unpacked.clone().data),
        )
        .map_err(|e| e.without_src())?
        .into();
        let _amm_target: raydium_amm::state::TargetOrders =
            transmute_one_pedantic::<raydium_amm::state::TargetOrders>(transmute_to_bytes(
                &amm_target_account.as_ref().unwrap().clone().data,
            ))
            .map_err(|e| e.without_src())?;
        let amm_pc_vault =
            spl_token::state::Account::unpack(&amm_pc_vault_account.as_ref().unwrap().clone().data)
                .unwrap();
        let amm_coin_vault = spl_token::state::Account::unpack(
            &amm_coin_vault_account.as_ref().unwrap().clone().data,
        )
        .unwrap();
        let (amm_pool_pc_vault_amount, amm_pool_coin_vault_amount) =
            if raydium_amm::state::AmmStatus::from_u64(amm.status).orderbook_permission() {
                let amm_open_orders_account =
                    &mut amm_open_orders_account.as_ref().unwrap().clone();
                let market_account = &mut market_account.as_ref().unwrap().clone();
                let market_event_q_account = &mut market_event_q_account.as_ref().unwrap().clone();
                let amm_open_orders_info =
                    (&amm.open_orders, amm_open_orders_account).into_account_info();
                let market_account_info = (&amm.market, market_account).into_account_info();
                let market_event_queue_info =
                    (&(market_keys.event_queue), market_event_q_account).into_account_info();
                let amm_authority = Pubkey::find_program_address(
                    &[raydium_amm::processor::AUTHORITY_AMM],
                    &RAYDIUM_LIQUIDITY_POOL_V4_PROGRAM_ID,
                )
                .0;
                let lamports = &mut 0;
                let data = &mut [0u8];
                let owner = Pubkey::default();
                let amm_authority_info = solana_program::account_info::AccountInfo::new(
                    &amm_authority,
                    false,
                    false,
                    lamports,
                    data,
                    &owner,
                    false,
                    0,
                );
                let (market_state, open_orders) =
                    raydium_amm::processor::Processor::load_serum_market_order(
                        &market_account_info,
                        &amm_open_orders_info,
                        &amm_authority_info,
                        &amm,
                        false,
                    )?;
                let (amm_pool_pc_vault_amount, amm_pool_coin_vault_amount) =
                    raydium_amm::math::Calculator::calc_total_without_take_pnl(
                        amm_pc_vault.amount,
                        amm_coin_vault.amount,
                        &open_orders,
                        &amm,
                        &market_state,
                        &market_event_queue_info,
                        &amm_open_orders_info,
                    )?;
                (amm_pool_pc_vault_amount, amm_pool_coin_vault_amount)
            } else {
                let (amm_pool_pc_vault_amount, amm_pool_coin_vault_amount) =
                    raydium_amm::math::Calculator::calc_total_without_take_pnl_no_orderbook(
                        amm_pc_vault.amount,
                        amm_coin_vault.amount,
                        &amm,
                    )?;
                (amm_pool_pc_vault_amount, amm_pool_coin_vault_amount)
            };

        let (direction, coin_to_pc) = if swap_input.input_token_mint == amm_keys.amm_coin_mint
            && swap_input.output_token_mint == amm_keys.amm_pc_mint
        {
            (raydium_library::amm::utils::SwapDirection::Coin2PC, true)
        } else {
            (raydium_library::amm::utils::SwapDirection::PC2Coin, false)
        };

        let amount_specified_is_input = swap_input.mode.amount_specified_is_input();
        let (other_amount, other_amount_threshold) = raydium_library::amm::swap_with_slippage(
            amm_pool_pc_vault_amount,
            amm_pool_coin_vault_amount,
            amm.fees.swap_fee_numerator,
            amm.fees.swap_fee_denominator,
            direction,
            swap_input.amount,
            amount_specified_is_input,
            swap_input.slippage_bps as u64,
        )?;
        log::debug!(
            "raw quote: {}. raw other_amount_threshold: {}",
            other_amount,
            other_amount_threshold
        );

        Ok(RaydiumAmmQuote {
            market: pool_id,
            input_mint: swap_input.input_token_mint,
            output_mint: swap_input.output_token_mint,
            amount: swap_input.amount,
            other_amount,
            other_amount_threshold,
            amount_specified_is_input,
            input_mint_decimals: if coin_to_pc {
                amm.coin_decimals
            } else {
                amm.pc_decimals
            } as u8,
            output_mint_decimals: if coin_to_pc {
                amm.pc_decimals
            } else {
                amm.coin_decimals
            } as u8,
            amm_keys,
            market_keys,
        })
    }

    // 定义一个异步函数swap_instructions，用于生成交换指令
    pub async fn swap_instructions(
        &self,
        // 输入公钥
        input_pubkey: Pubkey,
        // 交换报价
        output: RaydiumAmmQuote,
        // 交换配置覆盖
        overrides: Option<&SwapConfigOverrides>,
    ) -> anyhow::Result<Vec<solana_sdk::instruction::Instruction>> {
        // 调用make_swap函数，生成交换指令构建器
        let builder = self.make_swap(input_pubkey, output, overrides).await?;
        // 构建交换指令
        builder.build_instructions()
    }

    // 定义一个异步函数swap_transaction，用于交换交易
    pub async fn swap_transaction(
        // 接收一个self参数，表示当前对象
        &self,
        // 接收一个input_pubkey参数，表示输入公钥
        input_pubkey: Pubkey,
        // 接收一个output参数，表示输出
        output: RaydiumAmmQuote,
        // 接收一个overrides参数，表示覆盖配置
        overrides: Option<&SwapConfigOverrides>,
    ) -> anyhow::Result<VersionedTransaction> {
        // 调用make_swap函数，生成交换交易
        let builder = self.make_swap(input_pubkey, output, overrides).await?;
        // 构建交易
        builder.build_transaction(Some(&input_pubkey), None)
    }

    // 更新配置
    pub fn update_config(&mut self, config: &SwapConfig) {
        // 将传入的配置赋值给self的config
        self.config = *config;
    }

    // 异步函数，用于创建交换指令
    async fn make_swap(
        &self,
        input_pubkey: Pubkey, // 输入公钥
        output: RaydiumAmmQuote, // 交换输出
        overrides: Option<&SwapConfigOverrides>, // 交换配置覆盖
    ) -> anyhow::Result<SwapInstructionsBuilder> { // 返回交换指令构建器
        // 获取优先费用
        let priority_fee = overrides
            .and_then(|o| o.priority_fee)
            .or(self.config.priority_fee);
        // 获取计算单元限制
        let cu_limits = overrides
            .and_then(|o| o.cu_limits)
            .or(self.config.cu_limits);
        // 获取是否需要包装和解包 SOL
        let wrap_and_unwrap_sol = overrides
            .and_then(|o| o.wrap_and_unwrap_sol)
            .or(self.config.wrap_and_unwrap_sol)
            .unwrap_or(true);

        // 创建交换指令构建器
        let mut builder = SwapInstructionsBuilder::default();
        // 处理令牌包装和解包以及账户创建
        let _associated_accounts = builder.handle_token_wrapping_and_accounts_creation(
            input_pubkey,
            wrap_and_unwrap_sol,
            if output.amount_specified_is_input {
                output.amount
            } else {
                output.other_amount
            },
            output.input_mint,
            output.output_mint,
            spl_token::ID,
            spl_token::ID,
            None,
        )?;
        // 创建交换指令
        let instruction = swap_instruction(
            &RAYDIUM_LIQUIDITY_POOL_V4_PROGRAM_ID,
            &output.amm_keys,
            &output.market_keys,
            &input_pubkey,
            &spl_associated_token_account::get_associated_token_address(
                &input_pubkey,
                &output.input_mint,
            ),
            &spl_associated_token_account::get_associated_token_address(
                &input_pubkey,
                &output.output_mint,
            ),
            output.amount,
            output.other_amount_threshold,
            output.amount_specified_is_input,
        )?;
        // 将交换指令添加到构建器中
        builder.swap_instruction = Some(instruction);

        // 处理计算单元参数
        let compute_units = builder
            .handle_compute_units_params(cu_limits, &self.client, input_pubkey)
            .await?;
        // 处理优先费用参数
        builder.handle_priority_fee_params(priority_fee, compute_units, input_pubkey)?;

        // 返回交换指令构建器
        Ok(builder)
    }
}

#[derive(Debug)]
pub struct RaydiumAmmQuote {
    /// The address of the amm pool
    pub market: Pubkey,
    /// The input mint
    pub input_mint: Pubkey,
    /// The output mint,
    pub output_mint: Pubkey,
    /// The amount specified
    pub amount: u64,
    /// The other amount
    pub other_amount: u64,
    /// The other amount with slippage
    pub other_amount_threshold: u64,
    /// Whether the amount specified is in terms of the input token
    pub amount_specified_is_input: bool,
    /// The input mint decimals
    pub input_mint_decimals: u8,
    /// The output mint decimals
    pub output_mint_decimals: u8,
    /// Amm keys
    pub amm_keys: AmmKeys,
    /// Market keys
    pub market_keys: MarketKeys,
}

#[derive(Debug, Clone, Copy)]
pub struct MarketKeys {
    pub event_queue: Pubkey,
    pub bids: Pubkey,
    pub asks: Pubkey,
    pub coin_vault: Pubkey,
    pub pc_vault: Pubkey,
    pub vault_signer_key: Pubkey,
}

#[allow(clippy::too_many_arguments)]
// 定义一个函数，用于生成交换指令
fn swap_instruction(
    // 交换指令的amm程序
    amm_program: &Pubkey,
    // 交换指令的amm键
    amm_keys: &AmmKeys,
    // 交换指令的市场键
    market_keys: &MarketKeys,
    // 用户所有者
    user_owner: &Pubkey,
    // 用户源地址
    user_source: &Pubkey,
    // 用户目标地址
    user_destination: &Pubkey,
    // 指定金额
    amount_specified: u64,
    // 其他金额阈值
    other_amount_threshold: u64,
    // 是否交换基础
    swap_base_in: bool,
) -> anyhow::Result<Instruction> {
    // 如果是交换基础，则生成交换基础指令
    let swap_instruction = if swap_base_in {
        raydium_amm::instruction::swap_base_in(
            // 交换指令的amm程序
            amm_program,
            // 交换指令的amm池
            &amm_keys.amm_pool,
            // 交换指令的amm权限
            &amm_keys.amm_authority,
            // 交换指令的amm开放订单
            &amm_keys.amm_open_order,
            // 交换指令的amm代币库
            &amm_keys.amm_coin_vault,
            // 交换指令的amm代币库
            &amm_keys.amm_pc_vault,
            // 交换指令的市场程序
            &amm_keys.market_program,
            // 交换指令的市场
            &amm_keys.market,
            // 交换指令的市场 bids
            &market_keys.bids,
            // 交换指令的市场 asks
            &market_keys.asks,
            // 交换指令的市场事件队列
            &market_keys.event_queue,
            // 交换指令的市场代币库
            &market_keys.coin_vault,
            // 交换指令的市场代币库
            &market_keys.pc_vault,
            // 交换指令的市场库签名者键
            &market_keys.vault_signer_key,
            // 用户源地址
            user_source,
            // 用户目标地址
            user_destination,
            // 用户所有者
            user_owner,
            // 指定金额
            amount_specified,
            // 其他金额阈值
            other_amount_threshold,
        )?
    // 否则，生成交换基础指令
    } else {
        raydium_amm::instruction::swap_base_out(
            // 交换指令的amm程序
            amm_program,
            // 交换指令的amm池
            &amm_keys.amm_pool,
            // 交换指令的amm权限
            &amm_keys.amm_authority,
            // 交换指令的amm开放订单
            &amm_keys.amm_open_order,
            // 交换指令的amm代币库
            &amm_keys.amm_coin_vault,
            // 交换指令的amm代币库
            &amm_keys.amm_pc_vault,
            // 交换指令的市场程序
            &amm_keys.market_program,
            // 交换指令的市场
            &amm_keys.market,
            // 交换指令的市场 bids
            &market_keys.bids,
            // 交换指令的市场 asks
            &market_keys.asks,
            // 交换指令的市场事件队列
            &market_keys.event_queue,
            // 交换指令的市场代币库
            &market_keys.coin_vault,
            // 交换指令的市场代币库
            &market_keys.pc_vault,
            // 交换指令的市场库签名者键
            &market_keys.vault_signer_key,
            // 用户源地址
            user_source,
            // 用户目标地址
            user_destination,
            // 用户所有者
            user_owner,
            // 其他金额阈值
            other_amount_threshold,
            // 指定金额
            amount_specified,
        )?
    };

    // 返回交换指令
    Ok(swap_instruction)
}

impl From<&raydium_library::amm::MarketPubkeys> for MarketKeys {
    // 从raydium_library::amm::MarketPubkeys类型转换为Self类型
    fn from(keys: &raydium_library::amm::MarketPubkeys) -> Self {
        // 创建一个MarketKeys类型的实例
        MarketKeys {
            // 将keys.event_q赋值给event_queue
            event_queue: *keys.event_q,
            // 将keys.bids赋值给bids
            bids: *keys.bids,
            // 将keys.asks赋值给asks
            asks: *keys.asks,
            // 将keys.coin_vault赋值给coin_vault
            coin_vault: *keys.coin_vault,
            // 将keys.pc_vault赋值给pc_vault
            pc_vault: *keys.pc_vault,
            // 将keys.vault_signer_key赋值给vault_signer_key
            vault_signer_key: *keys.vault_signer_key,
        }
    }
}
impl From<&crate::api_v3::response::pools::standard::MarketKeys> for MarketKeys {
    // 实现从&crate::api_v3::response::pools::standard::MarketKeys到MarketKeys的转换
    fn from(keys: &crate::api_v3::response::pools::standard::MarketKeys) -> Self {
        // 从keys中获取market_event_queue、market_bids、market_asks、market_base_vault、market_quote_vault、market_authority的值
        MarketKeys {
            event_queue: keys.market_event_queue,
            bids: keys.market_bids,
            asks: keys.market_asks,
            coin_vault: keys.market_base_vault,
            pc_vault: keys.market_quote_vault,
            vault_signer_key: keys.market_authority,
        }
    }
}
impl TryFrom<&crate::api_v3::response::ApiV3StandardPoolKeys> for MarketKeys {
    // 定义转换错误类型
    type Error = anyhow::Error;

    // 实现TryFrom trait的try_from方法
    fn try_from(
        keys: &crate::api_v3::response::ApiV3StandardPoolKeys,
    ) -> Result<Self, Self::Error> {
        // 获取market keys
        let keys = keys
            .keys
            .market
            .as_ref()
            .context("market keys should be present for amm")?;
        // 将获取到的market keys转换为MarketKeys类型
        Ok(MarketKeys::from(keys))
    }
}

impl TryFrom<&crate::api_v3::response::ApiV3StandardPoolKeys> for AmmKeys {
    type Error = anyhow::Error;

    // 将ApiV3StandardPoolKeys类型转换为AmmKeys类型
    fn try_from(
        keys: &crate::api_v3::response::ApiV3StandardPoolKeys,
    ) -> Result<Self, Self::Error> {
        // 获取market keys
        let market_keys = keys
            .keys
            .market
            .as_ref()
            .context("market keys should be present for amm")?;
        Ok(AmmKeys {
            // 获取amm池id
            amm_pool: keys.id,
            // 获取amm币种mint地址
            amm_coin_mint: keys.mint_a.address,
            // 获取amm代币mint地址
            amm_pc_mint: keys.mint_b.address,
            // 获取amm权限
            amm_authority: keys.keys.authority,
            // 获取amm目标订单
            amm_target: keys
                .keys
                .target_orders
                .context("target orders should be present for amm")?,
            // 获取amm币种vault
            amm_coin_vault: keys.vault.a,
            // 获取amm代币vault
            amm_pc_vault: keys.vault.b,
            // 获取ammlp mint地址
            amm_lp_mint: keys.keys.mint_lp.address,
            // 获取amm开放订单
            amm_open_order: keys
                .keys
                .open_orders
                .context("open orders should be present for amm")?,
            // 获取市场程序id
            market_program: market_keys.market_program_id,
            // 获取市场id
            market: market_keys.market_id,
            // 随机nonce
            nonce: 0, // random
        })
    }
}
