use anyhow::Result;
use futures::{SinkExt, StreamExt};
use log::{error, info};
use std::collections::HashMap;
use tokio;
use bs58;
use yellowstone_grpc_client::{GeyserGrpcClient, ClientTlsConfig};
use yellowstone_grpc_proto::prelude::{
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterTransactions,
    SubscribeUpdate, subscribe_update::UpdateOneof,
};

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

    let endpoint = "http://fr.grpc.gadflynode.com:25565";

    let mut client = GeyserGrpcClient::build_from_shared(endpoint.to_string())?
        .tls_config(ClientTlsConfig::new().with_native_roots())?
        .connect()
        .await?;

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
        commitment: Some(CommitmentLevel::Confirmed as i32),
        ..Default::default()
    };

    subscribe_tx.send(request).await?;
    info!("✅ Подписка отправлена");

    while let Some(message) = updates_stream.next().await {
        match message {
            Ok(update) => process_update(update),
            Err(e) => {
                error!("Ошибка стрима: {:?}", e);
                break;
            }
        }
    }

    Ok(())
}

fn process_update(update: SubscribeUpdate) {
    if let Some(update_oneof) = update.update_oneof {
        match update_oneof {
            UpdateOneof::Transaction(tx_info) => {
                // Проверяем, что это Create транзакция Pump.fun
                if !is_pump_fun_create(&tx_info) {
                    return; // Пропускаем, если не Create
                }

                info!("🔥 Pump.fun Create транзакция в слоте {}", tx_info.slot);

                if let Some(tx) = &tx_info.transaction {
                    // Получаем подпись из transaction
                    let signature = if !tx.signature.is_empty() {
                        bs58::encode(&tx.signature).into_string()
                    } else if let Some(tx_data) = &tx.transaction {
                        if let Some(first_sig) = tx_data.signatures.first() {
                            bs58::encode(first_sig).into_string()
                        } else {
                            "unknown".to_string()
                        }
                    } else {
                        "unknown".to_string()
                    };
                    info!("Подпись: {}", signature);

                    if let Some(tx_data) = &tx.transaction {
                        if let Some(message) = &tx_data.message {
                            info!("Аккаунты: {:?}", message.account_keys.iter().map(|key| bs58::encode(key).into_string()).collect::<Vec<_>>());
                            info!("Recent blockhash: {}", bs58::encode(&message.recent_blockhash).into_string());

                            for instr in &message.instructions {
                                let program_id_idx = instr.program_id_index as usize;
                                if program_id_idx < message.account_keys.len() {
                                    let program_id = bs58::encode(&message.account_keys[program_id_idx]).into_string();
                                    let accounts: Vec<String> = instr.accounts.iter()
                                        .filter_map(|&idx| {
                                            let idx = idx as usize;
                                            if idx < message.account_keys.len() {
                                                Some(bs58::encode(&message.account_keys[idx]).into_string())
                                            } else {
                                                None
                                            }
                                        })
                                        .collect();
                                    info!("Инструкция: Программа {}, Аккаунты: {:?}, Data: {:?}",
                                        program_id,
                                        accounts,
                                        instr.data
                                    );
                                }
                            }
                        }
                    }

                    if let Some(meta) = &tx.meta {
                        if meta.err.is_some() {
                            info!("Ошибка tx: {:?}", meta.err);
                        }
                        info!("Fee: {}", meta.fee);
                    }
                }
            }
            _ => {}
        }
    }
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