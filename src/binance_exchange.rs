use {
    crate::exchange::*,
    async_trait::async_trait,
    chrono::{Local, TimeZone},
    serde::Deserialize,
    solana_sdk::pubkey::Pubkey,
    tokio_binance::AccountClient,
};

pub struct BinanceExchangeClient {
    account_client: AccountClient,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountInfoBalance {
    asset: String,
    free: String,
    locked: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountInfo {
    account_type: String,
    balances: Vec<AccountInfoBalance>,
    can_deposit: bool,
    can_trade: bool,
    can_withdraw: bool,
}

#[derive(Debug, Deserialize)]
struct DepositAddress {
    address: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AveragePrice {
    mins: usize,
    price: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TickerPrice {
    ask_price: String,
    bid_price: String,
    high_price: String,
    low_price: String,
    price_change: String,
    price_change_percent: String,
    symbol: String,
    volume: String,
    quote_volume: String,
    weighted_avg_price: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DepositRecord {
    address: String,
    asset: String,
    amount: f64,
    tx_id: String,
    status: usize, // 0 = pending, 1 = success, 6 = credited but cannot withdraw
}

impl DepositRecord {
    fn success(&self) -> bool {
        self.status == 1
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DepositHistory {
    deposit_list: Vec<DepositRecord>,
    success: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Order {
    client_order_id: String,
    cummulative_quote_qty: String,
    executed_qty: String,
    order_id: usize,
    order_list_id: isize,
    orig_qty: String,
    price: String,
    side: String,
    status: String, // "NEW" / "FILLED" / "CANCELED"
    symbol: String,
    time_in_force: String,
    r#type: String,
    time: Option<i64>,
    update_time: Option<i64>,
}

#[async_trait]
impl ExchangeClient for BinanceExchangeClient {
    async fn deposit_address(&self) -> Result<Pubkey, Box<dyn std::error::Error>> {
        if !self
            .account_client
            .get_account()
            .json::<AccountInfo>()
            .await?
            .can_deposit
        {
            return Err("deposits not available".into());
        }
        let withdrawal_client = self.account_client.to_withdraw_client();

        Ok(withdrawal_client
            .get_deposit_address("SOL")
            .with_status(true)
            .json::<DepositAddress>()
            .await?
            .address
            .ok_or("no deposit address returned")?
            .parse::<Pubkey>()?)
    }

    async fn recent_deposits(&self) -> Result<Vec<DepositInfo>, Box<dyn std::error::Error>> {
        let withdrawal_client = self.account_client.to_withdraw_client();
        Ok(withdrawal_client
            .get_deposit_history()
            .with_asset("SOL")
            .json::<DepositHistory>()
            .await?
            .deposit_list
            .into_iter()
            .filter_map(|dr| {
                if dr.success() {
                    Some(DepositInfo {
                        tx_id: dr.tx_id,
                        amount: dr.amount,
                    })
                } else {
                    None
                }
            })
            .collect())
    }

    async fn balance(&self) -> Result<ExchangeBalance, Box<dyn std::error::Error>> {
        let account_info = self
            .account_client
            .get_account()
            .json::<AccountInfo>()
            .await?;

        let sol_balance = account_info
            .balances
            .iter()
            .find(|b| b.asset == "SOL")
            .ok_or("No SOL balance")?;

        let free = sol_balance.free.parse::<f64>()?;
        let locked = sol_balance.locked.parse::<f64>()?;

        Ok(ExchangeBalance {
            available: free,
            total: free + locked,
        })
    }

    async fn print_market_info(
        &self,
        pair: &str,
        format: MarketInfoFormat,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let market_data_client = self.account_client.to_market_data_client();

        let ticker_price = market_data_client
            .get_24hr_ticker_price()
            .with_symbol(pair)
            .json::<TickerPrice>()
            .await?;

        match format {
            MarketInfoFormat::All => {
                println!("Pair: {}", pair);
                println!(
                    "Ask: ${}, Bid: ${}, High: ${}, Low: ${}, ",
                    ticker_price.ask_price,
                    ticker_price.bid_price,
                    ticker_price.high_price,
                    ticker_price.low_price
                );

                let average_price = market_data_client
                    .get_average_price(pair)
                    .json::<AveragePrice>()
                    .await?;

                println!(
                    "Last {} minute average: ${}",
                    average_price.mins, average_price.price
                );
                println!(
                    "Last 24h change: ${} ({}%)",
                    ticker_price.price_change, ticker_price.price_change_percent
                );
                println!(
                    "Weighted 24h average price: ${}",
                    ticker_price.weighted_avg_price
                );
            }
            MarketInfoFormat::Ask => {
                println!("{}", ticker_price.ask_price);
            }
            MarketInfoFormat::Weighted24hAveragePrice => {
                println!("{}", ticker_price.weighted_avg_price);
            }
        }
        Ok(())
    }

    async fn bid_ask(&self, pair: &str) -> Result<BidAsk, Box<dyn std::error::Error>> {
        let market_data_client = self.account_client.to_market_data_client();

        let ticker_price = market_data_client
            .get_24hr_ticker_price()
            .with_symbol(&pair)
            .json::<TickerPrice>()
            .await?;

        let ask_price = ticker_price.ask_price.parse::<f64>().expect("ask_price");
        let bid_price = ticker_price.bid_price.parse::<f64>().expect("bid_price");

        Ok(BidAsk {
            bid_price,
            ask_price,
        })
    }

    async fn place_sell_order(
        &self,
        pair: &str,
        price: f64,
        amount: f64,
    ) -> Result<OrderId, Box<dyn std::error::Error>> {
        if price * amount < 10. {
            return Err("Total order amount must be 10 or greater".into());
        }

        let response = self
            .account_client
            .place_limit_order(pair, tokio_binance::Side::Sell, price, amount, true)
            .with_new_order_resp_type(tokio_binance::OrderRespType::Full)
            .json::<Order>()
            .await?;

        Ok(response.client_order_id)
    }

    async fn cancel_sell_order(
        &self,
        pair: &str,
        order_id: &OrderId,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.account_client
            .cancel_order(pair, tokio_binance::ID::ClientOId(order_id))
            .json::<serde_json::Value>()
            .await?;
        Ok(())
    }

    async fn sell_order_status(
        &self,
        pair: &str,
        order_id: &OrderId,
    ) -> Result<SellOrderStatus, Box<dyn std::error::Error>> {
        let order = self
            .account_client
            .get_order(pair, tokio_binance::ID::ClientOId(order_id))
            .json::<Order>()
            .await?;

        assert_eq!(order.side, "SELL");
        assert_eq!(order.r#type, "LIMIT");
        assert_eq!(order.time_in_force, "GTC");
        assert_eq!(order.symbol, pair);
        assert_eq!(order.client_order_id, *order_id);

        let open = match order.status.as_str() {
            "NEW" | "PARTIALLY_FILLED" => true,
            "CANCELED" | "FILLED" => false,
            wtf_is_this => {
                panic!("Unknown order status: {}", wtf_is_this);
            }
        };

        let last_update = Local
            .timestamp(order.update_time.unwrap_or_default() / 1000, 0)
            .date()
            .naive_local();

        Ok(SellOrderStatus {
            open,
            price: order.price.parse()?,
            amount: order.orig_qty.parse()?,
            filled_amount: order.executed_qty.parse()?,
            last_update,
        })
    }
}

fn new_with_url(
    url: &str,
    ExchangeCredentials { api_key, secret }: ExchangeCredentials,
) -> Result<BinanceExchangeClient, Box<dyn std::error::Error>> {
    Ok(BinanceExchangeClient {
        account_client: AccountClient::connect(api_key, secret, url)?,
    })
}

pub fn new(
    exchange_credentials: ExchangeCredentials,
) -> Result<BinanceExchangeClient, Box<dyn std::error::Error>> {
    new_with_url("https://api.binance.com", exchange_credentials)
}

pub fn new_us(
    exchange_credentials: ExchangeCredentials,
) -> Result<BinanceExchangeClient, Box<dyn std::error::Error>> {
    new_with_url("https://api.binance.us", exchange_credentials)
}
