use anyhow::Result;
use futures::{SinkExt, StreamExt, Stream};
use log::{error, info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tokio;
use tokio::sync::broadcast;
use bs58;
use axum::{
    extract::State,
    http::{StatusCode, HeaderMap, HeaderValue},
    response::{Response, IntoResponse},
    routing::get,
    Router,
};
use tokio_stream::{wrappers::BroadcastStream, StreamExt as TokioStreamExt};
use serde::{Deserialize, Serialize};
use yellowstone_grpc_client::{GeyserGrpcClient, ClientTlsConfig};
use yellowstone_grpc_proto::prelude::{
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterTransactions,
    SubscribeUpdate, subscribe_update::UpdateOneof,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreateTransaction {
    signature: String,
    mint_address: String,
    creator_address: String,
    slot: u64,
}

type AppState = Arc<broadcast::Sender<CreateTransaction>>;

#[tokio::main]
async fn main() -> Result<()> {
    // Устанавливаем уровень логирования по умолчанию, если не задан
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    env_logger::init();
    
    // Устанавливаем CryptoProvider для rustls
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install crypto provider");

    // Создаем broadcast channel для отправки Create транзакций
    let (tx, _) = broadcast::channel::<CreateTransaction>(1000);
    let state = Arc::new(tx);

    // Запускаем HTTP сервер для SSE
    let state_clone = state.clone();
    tokio::spawn(async move {
        let app = Router::new()
            .route("/events", get(sse_handler))
            .route("/health", get(health_handler))
            .with_state(state_clone);

        let listener = tokio::net::TcpListener::bind("0.0.0.0:8724").await.unwrap();
        info!("🌐 HTTP сервер запущен на http://0.0.0.0:8724/events");
        axum::serve(listener, app).await.unwrap();
    });

    // Запускаем GRPC подписку
    let grpc_state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = run_grpc_subscription(grpc_state).await {
            error!("GRPC ошибка: {}", e);
        }
    });

    // Ждем бесконечно
    tokio::signal::ctrl_c().await?;
    info!("Остановка сервера...");
    Ok(())
}

async fn run_grpc_subscription(state: AppState) -> Result<()> {
    let endpoint = "http://fr.grpc.gadflynode.com:25565";
    let mut backoff = tokio::time::Duration::from_secs(1);

    loop {
        match subscribe_once(endpoint, state.clone()).await {
            Ok(_) => {
                backoff = tokio::time::Duration::from_secs(1);
                warn!("GRPC соединение закрыто, переподключение через {:?}...", backoff);
            }
            Err(e) => {
                error!("GRPC ошибка: {} (переподключение через {:?})", e, backoff);
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = std::cmp::min(backoff * 2, tokio::time::Duration::from_secs(30));
    }
}

async fn subscribe_once(endpoint: &str, state: AppState) -> Result<()> {
    let mut client = GeyserGrpcClient::build_from_shared(endpoint.to_string())?
        .tls_config(ClientTlsConfig::new().with_native_roots())?
        .connect()
        .await?;

    info!("✅ GRPC подключен: {}", endpoint);

    // Фильтр для Pump.fun Create транзакций
    let pump_fun_program_id = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
    
    let mut transactions_filters: HashMap<String, SubscribeRequestFilterTransactions> = HashMap::new();
    transactions_filters.insert(
        "pump_fun".to_string(),
        SubscribeRequestFilterTransactions {
            vote: Some(false),
            failed: Some(false),
            signature: None,
            account_include: vec![pump_fun_program_id.to_string()],
            account_exclude: vec![],
            account_required: vec![],
        },
    );

    let (mut subscribe_tx, mut updates_stream) = client.subscribe().await?;

    let request = SubscribeRequest {
        transactions: transactions_filters,
        commitment: Some(CommitmentLevel::Processed as i32),
        ..Default::default()
    };

    subscribe_tx.send(request).await?;
    info!("✅ Подписка на Pump.fun Create отправлена");

    while let Some(message) = updates_stream.next().await {
        match message {
            Ok(update) => {
                if let Some(create_tx) = process_update(update) {
                    // Отправляем Create транзакцию через broadcast
                    if state.send(create_tx.clone()).is_ok() {
                        info!("📤 Отправлено Create: mint={} creator={}", create_tx.mint_address, create_tx.creator_address);
                    }
                }
            }
            Err(e) => {
                error!("Ошибка стрима: {:?}", e);
                return Err(e.into());
            }
        }
    }

    Ok(())
}

async fn sse_handler(State(tx): State<AppState>) -> impl IntoResponse {
    use axum::body::Body;
    use axum::body::HttpBody;
    
    let rx = tx.subscribe();
    let stream = BroadcastStream::new(rx);
    
    let stream = stream.filter_map(|result| {
        futures::future::ready(match result {
            Ok(create_tx) => {
                let json = serde_json::to_string(&create_tx).ok()?;
                Some(Ok::<_, std::io::Error>(format!("data: {}\n\n", json)))
            }
            Err(_) => None,
        })
    });

    let body = Body::from_stream(stream);
    
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .body(body)
        .unwrap()
}

async fn health_handler() -> (StatusCode, &'static str) {
    (StatusCode::OK, "OK")
}

fn process_update(update: SubscribeUpdate) -> Option<CreateTransaction> {
    if let Some(update_oneof) = update.update_oneof {
        match update_oneof {
            UpdateOneof::Transaction(tx_info) => {
                // Проверяем, что это Create транзакция Pump.fun
                if !is_pump_fun_create(&tx_info) {
                    return None; // Пропускаем, если не Create
                }

                if let Some(tx) = &tx_info.transaction {
                    // Получаем подпись
                    let signature = if !tx.signature.is_empty() {
                        bs58::encode(&tx.signature).into_string()
                    } else if let Some(tx_data) = &tx.transaction {
                        if let Some(first_sig) = tx_data.signatures.first() {
                            bs58::encode(first_sig).into_string()
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    };

                    // Получаем creator (первый аккаунт)
                    let creator_address = if let Some(tx_data) = &tx.transaction {
                        if let Some(message) = &tx_data.message {
                            if let Some(first_key) = message.account_keys.first() {
                                bs58::encode(first_key).into_string()
                            } else {
                                return None;
                            }
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    };

                    // Получаем mint из post_token_balances
                    let mint_address = if let Some(meta) = &tx.meta {
                        let post_balances = &meta.post_token_balances;
                        let pre_balances = &meta.pre_token_balances;
                        
                        let pre_mints: std::collections::HashSet<String> = pre_balances.iter()
                            .filter_map(|b| b.mint.clone())
                            .collect();
                        
                        let mut candidate_mints = vec![];
                        for balance in post_balances {
                            if let Some(mint) = &balance.mint {
                                if !pre_mints.contains(mint) && !mint.contains("11111111111111111111111111111111") {
                                    candidate_mints.push(mint.clone());
                                }
                            }
                        }
                        
                        candidate_mints.iter()
                            .find(|m| m.ends_with("pump"))
                            .or_else(|| candidate_mints.first())
                            .cloned()
                    } else {
                        None
                    };

                    if let Some(mint) = mint_address {
                        info!("🔥 Pump.fun Create: mint={} creator={} signature={}", mint, creator_address, signature);
                        return Some(CreateTransaction {
                            signature,
                            mint_address: mint,
                            creator_address,
                            slot: tx_info.slot,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn is_pump_fun_create(tx_info: &yellowstone_grpc_proto::prelude::SubscribeUpdateTransaction) -> bool {
    let pump_fun_program_id = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
    
    // Проверяем метаданные транзакции
    if let Some(tx) = &tx_info.transaction {
        if let Some(meta) = &tx.meta {
            // Проверяем логи на наличие Pump.fun и Create
            if let Some(log_messages) = &meta.log_messages {
                let log_str = match std::str::from_utf8(log_messages) {
                    Ok(s) => s,
                    Err(_) => return false,
                };
                
                let has_pump_fun = log_str.contains(pump_fun_program_id);
                let is_create = log_str.contains("Instruction: Create") && !log_str.contains("CreateV2");
                let is_create_v2 = log_str.contains("Instruction: CreateV2");
                
                if has_pump_fun && (is_create || is_create_v2) {
                    return true;
                }
            }
        }
    }
    
    false
}